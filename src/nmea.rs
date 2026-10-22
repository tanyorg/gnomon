//! NMEA 0183 sentence parsing and receiver state accumulation.
//!
//! Handles NMEA 2.x through 4.11 style output (multi-constellation talkers,
//! GSA system ID, GSV signal ID).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

/// Satellite data older than this is dropped from the display.
pub const SAT_STALE: Duration = Duration::from_secs(10);
/// "Used in fix" sets older than this are ignored.
pub const USED_STALE: Duration = Duration::from_secs(5);

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

    fn from_talker(t: &str) -> Option<Sys> {
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
    fn from_system_id(id: u8) -> Option<Sys> {
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
    fn from_prn(prn: u16) -> Sys {
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
    fn refine(hint: Sys, prn: u16) -> Sys {
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

fn f(fields: &[&str], i: usize) -> Option<f64> {
    fields
        .get(i)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}
fn u(fields: &[&str], i: usize) -> Option<u32> {
    fields
        .get(i)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}
fn s<'a>(fields: &[&'a str], i: usize) -> Option<&'a str> {
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

#[derive(Debug, Clone, Default)]
pub struct Fix {
    pub tod: Option<f64>,
    pub date: Option<(i32, u32, u32)>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt: Option<f64>,
    pub sep: Option<f64>,
    pub speed_kn: Option<f64>,
    pub track: Option<f64>,
    pub magvar: Option<f64>,
    pub rmc_status: Option<char>,
    pub pos_mode: Option<String>,
    pub quality: Option<u32>,
    pub nav_mode: Option<u32>,
    pub sats_used: Option<u32>,
    pub pdop: Option<f64>,
    pub hdop: Option<f64>,
    pub vdop: Option<f64>,
    pub gst_lat: Option<f64>,
    pub gst_lon: Option<f64>,
    pub gst_alt: Option<f64>,
    pub diff_age: Option<f64>,
    pub diff_station: Option<String>,
    pub last_txt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SatView {
    pub sys: Sys,
    pub prn: u16,
    pub el: Option<i32>,
    pub az: Option<i32>,
    /// C/N0 per signal ID (0 = not given / single signal).
    pub snr: BTreeMap<u8, Option<u32>>,
    pub used: bool,
}

#[derive(Debug, Clone)]
struct GsvSat {
    sys: Sys,
    prn: u16,
    el: Option<i32>,
    az: Option<i32>,
    snr: Option<u32>,
}

#[derive(Debug, Default)]
pub struct SentenceStat {
    pub count: u64,
    pub bad: u64,
    pub last: Option<Instant>,
    /// Smoothed interval between epochs in which this sentence appeared (s).
    pub period: Option<f64>,
    burst_epoch: u64,
    burst_start: Option<Instant>,
    outliers: u32,
}

impl SentenceStat {
    /// How long without this sentence before it is considered missing.
    pub fn stale_after(&self) -> Duration {
        let p = self.period.unwrap_or(1.0);
        Duration::from_secs_f64((p * 3.0).max(3.0))
    }

    fn mark(&mut self, now: Instant, epoch_seq: u64) {
        self.count += 1;
        // A new "burst" is the first arrival in a new NMEA epoch. Without any
        // time-bearing sentences fall back to a plain inter-arrival gap.
        let new_burst = if epoch_seq > 0 {
            self.burst_epoch != epoch_seq
        } else {
            self.last
                .map_or(true, |l| now.duration_since(l) > Duration::from_millis(300))
        };
        if new_burst {
            if let Some(bs) = self.burst_start {
                let iv = now.duration_since(bs).as_secs_f64();
                if iv > 0.0 && iv < 60.0 {
                    self.period = Some(match self.period {
                        None => iv,
                        // Missed epochs: ignore, unless the rate really changed.
                        Some(p) if iv > p * 1.8 && self.outliers < 3 => {
                            self.outliers += 1;
                            p
                        }
                        Some(p) if iv > p * 1.8 => {
                            self.outliers = 0;
                            iv
                        }
                        Some(p) => {
                            self.outliers = 0;
                            p + 0.2 * (iv - p)
                        }
                    });
                }
            }
            self.burst_start = Some(now);
            self.burst_epoch = epoch_seq;
        }
        self.last = Some(now);
    }
}

#[derive(Debug, Default)]
pub struct Nmea {
    pub fix: Fix,
    pub stats: BTreeMap<String, SentenceStat>,
    pub total: u64,
    pub bad_ck: u64,
    pub no_ck: u64,
    /// Host receive time minus fix time-of-day for the first sentence of each epoch.
    pub lag: Option<f64>,
    last_epoch_tod: Option<f64>,
    epoch_seq: u64,
    gsv_stage: HashMap<(String, u8), (u32, Vec<GsvSat>)>,
    gsv_done: HashMap<(String, u8), (Instant, Vec<GsvSat>)>,
    used: HashMap<String, (Instant, HashSet<(Sys, u16)>)>,
}

/// What the caller should do with a line after feeding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    NotNmea,
    Good,
    NoChecksum,
    BadChecksum,
}

impl Nmea {
    pub fn clear(&mut self) {
        *self = Nmea::default();
    }

    /// `host_tod`: host UTC seconds-of-day at receipt (for lag measurement).
    pub fn feed(&mut self, line: &str, now: Instant, host_tod: f64) -> Feed {
        let Some(snt) = split(line) else {
            return Feed::NotNmea;
        };
        self.total += 1;
        let bad = matches!(snt.ck, CkStatus::Bad { .. });
        match snt.ck {
            CkStatus::Bad { .. } => self.bad_ck += 1,
            CkStatus::Missing => self.no_ck += 1,
            CkStatus::Ok => {}
        }
        // Apply first so an epoch-starting sentence is counted in its new epoch.
        if !bad && snt.talker.is_some() {
            self.apply(&snt, now, host_tod);
        }
        let seq = self.epoch_seq;
        let st = self.stats.entry(snt.addr.to_string()).or_default();
        st.mark(now, seq);
        if bad {
            st.bad += 1;
            return Feed::BadChecksum;
        }
        if snt.ck == CkStatus::Missing {
            Feed::NoChecksum
        } else {
            Feed::Good
        }
    }

    fn epoch(&mut self, t: Option<f64>, host_tod: f64) {
        let Some(t) = t else { return };
        if self.last_epoch_tod.map_or(true, |p| (p - t).abs() > 1e-6) {
            self.last_epoch_tod = Some(t);
            self.epoch_seq += 1;
            let mut d = host_tod - t;
            if d > 43200.0 {
                d -= 86400.0;
            } else if d < -43200.0 {
                d += 86400.0;
            }
            self.lag = Some(d);
        }
        self.fix.tod = Some(t);
    }

    fn apply(&mut self, snt: &Sentence, now: Instant, host_tod: f64) {
        let fl = &snt.fields;
        let talker = snt.talker.unwrap_or("");
        match snt.kind {
            "RMC" => {
                self.epoch(tod(s(fl, 0)), host_tod);
                self.fix.rmc_status = s(fl, 1).and_then(|v| v.chars().next());
                self.fix.lat = coord(s(fl, 2), s(fl, 3));
                self.fix.lon = coord(s(fl, 4), s(fl, 5));
                self.fix.speed_kn = f(fl, 6);
                self.fix.track = f(fl, 7);
                if let Some(d) = s(fl, 8).filter(|d| d.len() == 6) {
                    let dd = d[0..2].parse().ok();
                    let mm = d[2..4].parse().ok();
                    let yy: Option<i32> = d[4..6].parse().ok();
                    if let (Some(dd), Some(mm), Some(yy)) = (dd, mm, yy) {
                        // Two-digit year: pivot at 80 like gpsd does.
                        let y = if yy < 80 { 2000 + yy } else { 1900 + yy };
                        self.fix.date = Some((y, mm, dd));
                    }
                }
                self.fix.magvar = f(fl, 9).map(|m| if s(fl, 10) == Some("W") { -m } else { m });
                if let Some(m) = s(fl, 11) {
                    self.fix.pos_mode = Some(m.to_string());
                }
            }
            "GGA" => {
                self.epoch(tod(s(fl, 0)), host_tod);
                self.fix.lat = coord(s(fl, 1), s(fl, 2)).or(self.fix.lat);
                self.fix.lon = coord(s(fl, 3), s(fl, 4)).or(self.fix.lon);
                self.fix.quality = u(fl, 5);
                self.fix.sats_used = u(fl, 6);
                self.fix.hdop = f(fl, 7).or(self.fix.hdop);
                self.fix.alt = f(fl, 8);
                self.fix.sep = f(fl, 10);
                self.fix.diff_age = f(fl, 12);
                self.fix.diff_station = s(fl, 13).map(str::to_string);
            }
            "GNS" => {
                self.epoch(tod(s(fl, 0)), host_tod);
                self.fix.lat = coord(s(fl, 1), s(fl, 2)).or(self.fix.lat);
                self.fix.lon = coord(s(fl, 3), s(fl, 4)).or(self.fix.lon);
                if let Some(m) = s(fl, 5) {
                    self.fix.pos_mode = Some(m.to_string());
                }
                self.fix.sats_used = u(fl, 6).or(self.fix.sats_used);
                self.fix.hdop = f(fl, 7).or(self.fix.hdop);
                self.fix.alt = f(fl, 8).or(self.fix.alt);
                self.fix.sep = f(fl, 9).or(self.fix.sep);
            }
            "GLL" => {
                self.fix.lat = coord(s(fl, 0), s(fl, 1)).or(self.fix.lat);
                self.fix.lon = coord(s(fl, 2), s(fl, 3)).or(self.fix.lon);
                self.epoch(tod(s(fl, 4)), host_tod);
            }
            "ZDA" => {
                self.epoch(tod(s(fl, 0)), host_tod);
                if let (Some(d), Some(m), Some(y)) = (u(fl, 1), u(fl, 2), u(fl, 3)) {
                    self.fix.date = Some((y as i32, m, d));
                }
            }
            "VTG" => {
                self.fix.track = f(fl, 0).or(self.fix.track);
                self.fix.speed_kn = f(fl, 4).or(self.fix.speed_kn);
            }
            "GST" => {
                self.fix.gst_lat = f(fl, 5);
                self.fix.gst_lon = f(fl, 6);
                self.fix.gst_alt = f(fl, 7);
            }
            "GSA" => self.gsa(talker, fl, now),
            "GSV" => self.gsv(talker, fl, now),
            "TXT" => {
                if let Some(t) = s(fl, 3) {
                    self.fix.last_txt = Some(t.to_string());
                }
            }
            _ => {}
        }
    }

    fn gsa(&mut self, talker: &str, fl: &[&str], now: Instant) {
        self.fix.nav_mode = u(fl, 1);
        self.fix.pdop = f(fl, 14);
        self.fix.hdop = f(fl, 15).or(self.fix.hdop);
        self.fix.vdop = f(fl, 16);
        let sysid = u(fl, 17).and_then(|v| u8::try_from(v).ok());
        let hint = sysid
            .and_then(Sys::from_system_id)
            .or_else(|| Sys::from_talker(talker))
            .unwrap_or(Sys::Unknown);
        let prns: Vec<u16> = (2..14)
            .filter_map(|i| u(fl, i))
            .filter_map(|p| u16::try_from(p).ok())
            .collect();
        let set: HashSet<(Sys, u16)> = prns.iter().map(|&p| (Sys::refine(hint, p), p)).collect();
        let group = match (sysid, talker) {
            (Some(id), _) => format!("id{id}"),
            (None, "GN") => match prns.first() {
                Some(&p) => Sys::from_prn(p).label().to_string(),
                None => return, // empty GN GSA without system id: nothing attributable
            },
            (None, t) => t.to_string(),
        };
        self.used.insert(group, (now, set));
    }

    fn gsv(&mut self, talker: &str, fl: &[&str], now: Instant) {
        let (Some(total), Some(num)) = (u(fl, 0), u(fl, 1)) else {
            return;
        };
        // After the 3 header fields come 4-field blocks, optionally followed
        // by a signal ID (NMEA 4.10+).
        let body = fl.len().saturating_sub(3);
        let signal = if body % 4 == 1 {
            u(fl, fl.len() - 1)
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(0)
        } else {
            0
        };
        let hint = Sys::from_talker(talker).unwrap_or(Sys::Unknown);
        let key = (talker.to_string(), signal);
        if num == 1 {
            self.gsv_stage.insert(key.clone(), (1, Vec::new()));
        }
        let Some(stage) = self.gsv_stage.get_mut(&key) else {
            return;
        };
        if num != stage.0 && num != 1 {
            // Out of sequence: discard the partial cycle.
            self.gsv_stage.remove(&key);
            return;
        }
        let blocks = body / 4;
        for b in 0..blocks {
            let i = 3 + b * 4;
            let Some(prn) = u(fl, i).and_then(|p| u16::try_from(p).ok()) else {
                continue;
            };
            if prn == 0 {
                continue;
            }
            stage.1.push(GsvSat {
                sys: Sys::refine(hint, prn),
                prn,
                el: f(fl, i + 1).map(|v| v as i32),
                az: f(fl, i + 2).map(|v| v as i32),
                snr: u(fl, i + 3),
            });
        }
        stage.0 = num + 1;
        if num >= total {
            let (_, sats) = self.gsv_stage.remove(&key).unwrap();
            self.gsv_done.insert(key, (now, sats));
        }
    }

    pub fn used_set(&self, now: Instant) -> HashSet<(Sys, u16)> {
        self.used
            .values()
            .filter(|(t, _)| now.duration_since(*t) < USED_STALE)
            .flat_map(|(_, s)| s.iter().copied())
            .collect()
    }

    /// Merged satellite view across talkers and signals, sorted for display.
    pub fn satellites(&self, now: Instant) -> Vec<SatView> {
        let used = self.used_set(now);
        let mut map: BTreeMap<(Sys, u16), SatView> = BTreeMap::new();
        for ((_, sig), (t, sats)) in &self.gsv_done {
            if now.duration_since(*t) >= SAT_STALE {
                continue;
            }
            for g in sats {
                let e = map.entry((g.sys, g.prn)).or_insert_with(|| SatView {
                    sys: g.sys,
                    prn: g.prn,
                    el: None,
                    az: None,
                    snr: BTreeMap::new(),
                    used: false,
                });
                e.el = e.el.or(g.el);
                e.az = e.az.or(g.az);
                e.snr.insert(*sig, g.snr);
            }
        }
        // A used satellite might not have been in view list (or vice versa).
        for &(sys, prn) in &used {
            map.entry((sys, prn)).or_insert_with(|| SatView {
                sys,
                prn,
                el: None,
                az: None,
                snr: BTreeMap::new(),
                used: true,
            });
        }
        let mut v: Vec<SatView> = map
            .into_values()
            .map(|mut s| {
                s.used = used.contains(&(s.sys, s.prn));
                s
            })
            .collect();
        v.sort_by_key(|s| (!s.used, s.sys, s.prn));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(body: &str) -> String {
        let c = body.bytes().fold(0u8, |a, b| a ^ b);
        format!("${body}*{c:02X}")
    }

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

    #[test]
    fn fix_and_sats() {
        let mut n = Nmea::default();
        let t0 = Instant::now();
        let host = 12.0 * 3600.0 + 34.0 * 60.0 + 56.120;
        let lines = [
            ck("GNRMC,123456.00,A,3335.4073,N,13024.1030,E,0.012,,221026,,,A,V"),
            ck("GNGGA,123456.00,3335.4073,N,13024.1030,E,1,12,0.62,15.3,M,28.4,M,,"),
            ck("GNGSA,A,3,05,13,15,,,,,,,,,,1.10,0.62,0.91,1"),
            ck("GNGSA,A,3,72,81,,,,,,,,,,,1.10,0.62,0.91,2"),
            ck("GPGSV,2,1,05,05,45,120,40,13,60,200,44,15,30,300,38,20,10,050,,1"),
            ck("GPGSV,2,2,05,42,48,160,35,1"),
            ck("GPGSV,1,1,02,05,45,120,37,13,60,200,41,8"),
            ck("GLGSV,1,1,02,72,20,100,30,81,70,010,42,1"),
            ck("GNGST,123456.00,10,1.2,0.8,45,0.9,0.8,1.5"),
        ];
        for l in &lines {
            assert_eq!(n.feed(l, t0, host), Feed::Good, "{l}");
        }
        assert_eq!(n.fix.date, Some((2026, 10, 22)));
        assert_eq!(n.fix.quality, Some(1));
        assert_eq!(n.fix.nav_mode, Some(3));
        assert!((n.lag.unwrap() - 0.120).abs() < 1e-6);
        let sats = n.satellites(t0);
        // 5 GPS/SBAS + 2 GLONASS
        assert_eq!(sats.len(), 7);
        let used: Vec<_> = sats
            .iter()
            .filter(|s| s.used)
            .map(|s| (s.sys, s.prn))
            .collect();
        assert_eq!(
            used,
            vec![
                (Sys::Gps, 5),
                (Sys::Gps, 13),
                (Sys::Gps, 15),
                (Sys::Glonass, 72),
                (Sys::Glonass, 81)
            ]
        );
        let sb = sats.iter().find(|s| s.prn == 42).unwrap();
        assert_eq!(sb.sys, Sys::Sbas);
        let g5 = sats.iter().find(|s| s.prn == 5).unwrap();
        assert_eq!(g5.snr.get(&1), Some(&Some(40)));
        assert_eq!(g5.snr.get(&8), Some(&Some(37)));
        assert_eq!(n.fix.gst_alt, Some(1.5));
    }

    #[test]
    fn legacy_gsv_and_bad_ck() {
        let mut n = Nmea::default();
        let t0 = Instant::now();
        // NMEA 2.x style: no signal id, GPS-only GSA
        n.feed(&ck("GPGSA,A,3,04,05,,,,,,,,,,,2.5,1.3,2.1"), t0, 0.0);
        n.feed(
            &ck("GPGSV,1,1,03,04,40,083,46,05,15,265,,33,30,200,40"),
            t0,
            0.0,
        );
        let sats = n.satellites(t0);
        assert_eq!(sats.len(), 3);
        assert!(sats.iter().any(|s| s.sys == Sys::Sbas && s.prn == 33));
        assert_eq!(sats.iter().filter(|s| s.used).count(), 2);
        assert_eq!(n.feed("$GPGGA,1*00", t0, 0.0), Feed::BadChecksum);
        assert_eq!(n.bad_ck, 1);
        // stale after timeout
        assert!(n
            .satellites(t0 + SAT_STALE + Duration::from_millis(1))
            .iter()
            .all(|s| s.el.is_none()));
    }

    #[test]
    fn garbage_never_panics() {
        // Wrong baud rate / line noise: arbitrary bytes after lossy UTF-8.
        let mut n = Nmea::default();
        let t0 = Instant::now();
        let mut x: u32 = 0x1234_5678;
        for _ in 0..20_000 {
            let mut b = vec![b'$'];
            for _ in 0..(x % 90) {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                b.push(match x % 7 {
                    0 => b',',
                    1 => b'*',
                    2 => b'.',
                    _ => (x >> 8) as u8,
                });
            }
            let l = String::from_utf8_lossy(&b).into_owned();
            n.feed(&l, t0, 0.0);
        }
        let _ = n.satellites(t0);
        // Non-ASCII is rejected outright.
        assert!(split("$GPRMC,\u{fffd}*00").is_none());
        assert!(split("$GPGGA,1*\u{e9}").is_none());
    }

    #[test]
    fn short_and_odd_fields_never_panic() {
        let mut n = Nmea::default();
        let t0 = Instant::now();
        for l in [
            "$GPRMC",
            "$GPRMC,",
            "$GPRMC,1,A,1,N,1,E,,,1",
            "$GPGGA,12,..,N,.,E",
            "$GPZDA,999999",
            "$GPGSV,1,1",
            "$GPGSV,9,1,99,1",
            "$GPGSA,A,3,,,,,,,,,,,,,,,,,,,,",
            "$",
            "$*",
            "$G*Z",
            "$GPGLL,,,,,1234567",
            "$GPRMC,000000.00,A,00.0,N,000.0,E,0,0,999999",
        ] {
            n.feed(l, t0, 0.0);
        }
    }

    #[test]
    fn sentence_period() {
        let mut n = Nmea::default();
        let t0 = Instant::now();
        for e in 0..6u64 {
            let t = t0 + Duration::from_millis(1000 * e + if e % 2 == 1 { 30 } else { 0 });
            let tod = format!("1200{:02}.00", e);
            n.feed(
                &ck(&format!(
                    "GPRMC,{tod},A,3335.4073,N,13024.1030,E,0,0,221026,,,A"
                )),
                t,
                0.0,
            );
            // 4 GSV parts spread over 300 ms (slow serial): still one per epoch
            for k in 1..=4u64 {
                let line = ck(&format!("GPGSV,4,{k},04,05,45,120,40"));
                n.feed(&line, t + Duration::from_millis(75 * k), 0.0);
            }
        }
        // a dropped epoch must not disturb the estimate
        let t = t0 + Duration::from_millis(7000);
        n.feed(
            &ck("GPRMC,120007.00,A,3335.4073,N,13024.1030,E,0,0,221026,,,A"),
            t,
            0.0,
        );
        for k in ["GPRMC", "GPGSV"] {
            let p = n.stats[k].period.unwrap();
            assert!((p - 1.0).abs() < 0.05, "{k} {p}");
        }
        assert_eq!(n.stats["GPGSV"].count, 24);
    }

    #[test]
    fn lag_wraps_midnight() {
        let mut n = Nmea::default();
        n.feed(
            &ck("GPRMC,235959.90,A,3335.4073,N,13024.1030,E,0,0,221026,,,A"),
            Instant::now(),
            0.05,
        );
        assert!((n.lag.unwrap() - 0.15).abs() < 1e-6);
    }
}
