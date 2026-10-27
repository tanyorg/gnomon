//! One NMEA 0183 sentence: framing, checksum, field access and the
//! constellation a talker or PRN belongs to. Stateless.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Sys {
    Gps,
    Sbas,
    Glonass,
    Galileo,
    Beidou,
    Qzss,
    Navic,
    Unknown,
}

impl Sys {
    pub fn label(self) -> &'static str {
        match self {
            Sys::Gps => "GP",
            Sys::Sbas => "SB",
            Sys::Glonass => "GL",
            Sys::Galileo => "GA",
            Sys::Beidou => "BD",
            Sys::Qzss => "QZ",
            Sys::Navic => "IR",
            Sys::Unknown => "??",
        }
    }

    pub(crate) fn from_talker(t: &str) -> Option<Sys> {
        Some(match t {
            "GP" => Sys::Gps,
            "GL" => Sys::Glonass,
            "GA" => Sys::Galileo,
            "GB" | "BD" => Sys::Beidou,
            "GQ" | "QZ" => Sys::Qzss,
            "GI" | "IN" => Sys::Navic,
            _ => return None,
        })
    }

    /// NMEA 4.10+ system ID (GSA field 18, GSV signal context).
    pub(crate) fn from_system_id(id: u8) -> Option<Sys> {
        Some(match id {
            1 => Sys::Gps,
            2 => Sys::Glonass,
            3 => Sys::Galileo,
            4 => Sys::Beidou,
            5 => Sys::Qzss,
            6 => Sys::Navic,
            _ => return None,
        })
    }

    /// Classic NMEA PRN numbering when no better hint exists.
    pub(crate) fn from_prn(prn: u16) -> Sys {
        match prn {
            1..=32 => Sys::Gps,
            33..=64 | 120..=158 => Sys::Sbas,
            65..=96 => Sys::Glonass,
            193..=202 => Sys::Qzss,
            203..=264 => Sys::Beidou, // some receivers offset BeiDou PRNs into 201+
            301..=336 => Sys::Galileo,
            _ => Sys::Unknown,
        }
    }

    /// Resolve a PRN reported under `hint`, catching SBAS/QZSS carried under GPS.
    pub(crate) fn refine(hint: Sys, prn: u16) -> Sys {
        match hint {
            Sys::Gps => match prn {
                33..=64 | 120..=158 => Sys::Sbas,
                193..=202 => Sys::Qzss,
                _ => Sys::Gps,
            },
            Sys::Unknown => Sys::from_prn(prn),
            s => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CkStatus {
    Ok,
    Missing,
    Bad { got: u8, want: u8 },
}

#[derive(Debug, Clone)]
pub struct Sentence<'a> {
    /// Address field without the leading '$'/'!', e.g. "GNRMC", "PUBX".
    pub addr: &'a str,
    /// Two-letter talker for standard sentences, None for proprietary.
    pub talker: Option<&'a str>,
    /// Formatter ("RMC", "GSV", ...); for proprietary sentences the full address.
    pub kind: &'a str,
    pub fields: Vec<&'a str>,
    pub ck: CkStatus,
}

/// Split and checksum-verify one sentence. Returns None if it is not NMEA-shaped.
pub fn split(line: &str) -> Option<Sentence<'_>> {
    let line = line.trim_end_matches(['\r', '\n']);
    let first = line.as_bytes().first()?;
    // NMEA 0183 is printable ASCII by definition. Rejecting anything else
    // also keeps every byte-index slice below on a char boundary.
    if (*first != b'$' && *first != b'!') || !line.is_ascii() {
        return None;
    }
    let body_end = line.rfind('*');
    let (body, ck) = match body_end {
        Some(p) if line.len() >= p + 3 => {
            let body = &line[1..p];
            let want = body.bytes().fold(0u8, |a, b| a ^ b);
            match u8::from_str_radix(&line[p + 1..p + 3], 16) {
                Ok(got) if got == want => (body, CkStatus::Ok),
                Ok(got) => (body, CkStatus::Bad { got, want }),
                Err(_) => (body, CkStatus::Bad { got: 0, want }),
            }
        }
        Some(p) => (&line[1..p], CkStatus::Missing),
        None => (&line[1..], CkStatus::Missing),
    };
    let mut it = body.split(',');
    let addr = it.next()?;
    if addr.is_empty() || !addr.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let fields: Vec<&str> = it.collect();
    let (talker, kind) = if addr.starts_with('P') || addr.len() < 5 {
        (None, addr)
    } else {
        (Some(&addr[..2]), &addr[2..])
    };
    Some(Sentence {
        addr,
        talker,
        kind,
        fields,
        ck,
    })
}

/// Field `i` as a number, if present and well formed.
pub(crate) fn f(fields: &[&str], i: usize) -> Option<f64> {
    fields
        .get(i)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}
/// Field `i` as an unsigned integer, if present and well formed.
pub(crate) fn u(fields: &[&str], i: usize) -> Option<u32> {
    fields
        .get(i)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}
/// Field `i` as text, if present and non-empty.
pub(crate) fn s<'a>(fields: &[&'a str], i: usize) -> Option<&'a str> {
    fields.get(i).copied().filter(|s| !s.is_empty())
}

/// ddmm.mmmm + hemisphere -> signed decimal degrees.
pub fn coord(v: Option<&str>, hemi: Option<&str>) -> Option<f64> {
    let v = v?;
    let dot = v.find('.').unwrap_or(v.len());
    if dot < 3 {
        return None;
    }
    let deg: f64 = v[..dot - 2].parse().ok()?;
    let min: f64 = v[dot - 2..].parse().ok()?;
    if min >= 60.0 {
        return None;
    }
    let mut d = deg + min / 60.0;
    match hemi? {
        "S" | "W" => d = -d,
        "N" | "E" => {}
        _ => return None,
    }
    Some(d)
}

/// hhmmss[.sss] -> seconds of day.
pub fn tod(v: Option<&str>) -> Option<f64> {
    let v = v?;
    if v.len() < 6 {
        return None;
    }
    let h: f64 = v[0..2].parse().ok()?;
    let m: f64 = v[2..4].parse().ok()?;
    let sec: f64 = v[4..].parse().ok()?;
    if h > 23.0 || m > 59.0 || sec >= 61.0 {
        return None;
    }
    Some(h * 3600.0 + m * 60.0 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_and_split() {
        let s = split("$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47").unwrap();
        assert_eq!(s.ck, CkStatus::Ok);
        assert_eq!(s.talker, Some("GP"));
        assert_eq!(s.kind, "GGA");
        let s = split("$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*48").unwrap();
        assert!(matches!(s.ck, CkStatus::Bad { .. }));
        let s = split("$PUBX,00,foo").unwrap();
        assert_eq!((s.talker, s.kind, s.ck), (None, "PUBX", CkStatus::Missing));
        assert!(split("{\"class\":\"TPV\"}").is_none());
    }

    #[test]
    fn coords() {
        let lat = coord(Some("3335.4073"), Some("N")).unwrap();
        assert!((lat - 33.590122).abs() < 1e-5);
        let lon = coord(Some("13024.1030"), Some("W")).unwrap();
        assert!((lon + 130.401717).abs() < 1e-5);
        assert!(coord(Some("3365.0"), Some("N")).is_none());
    }
}
