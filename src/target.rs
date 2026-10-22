//! Parsing of the source specification given on the command line.
//!
//! Accepted forms (gpsmon compatible where it makes sense):
//!   host                 -> host:2947 (auto-detect, gpsd expected)
//!   host:port            -> auto-detect gpsd JSON vs raw NMEA
//!   host:port:/dev/xxx   -> gpsd, watch only that device
//!   [v6addr]:port[:dev]  -> IPv6 literal
//!   tcp://host:port      -> raw NMEA over TCP (never write to the peer)
//!   gpsd://host[:port]   -> force gpsd protocol

pub const GPSD_PORT: u16 = 2947;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Auto,
    Gpsd,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub device: Option<String>,
    pub proto: Proto,
}

impl Target {
    /// Human readable form, IPv6 literals bracketed.
    pub fn display(&self) -> String {
        let h = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match &self.device {
            Some(d) => format!("{}:{}:{}", h, self.port, d),
            None => format!("{}:{}", h, self.port),
        }
    }
}

pub fn parse(spec: &str) -> Result<Target, String> {
    let (proto, rest) = if let Some(r) = spec.strip_prefix("tcp://") {
        (Proto::Raw, r)
    } else if let Some(r) = spec.strip_prefix("gpsd://") {
        (Proto::Gpsd, r)
    } else {
        (Proto::Auto, spec)
    };
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return Err("empty target".into());
    }

    let (host, tail): (String, Option<&str>) = if let Some(r) = rest.strip_prefix('[') {
        let end = r.find(']').ok_or("unterminated '[' in IPv6 literal")?;
        let host = r[..end].to_string();
        let after = &r[end + 1..];
        match after.strip_prefix(':') {
            Some(t) => (host, Some(t)),
            None if after.is_empty() => (host, None),
            None => return Err(format!("garbage after IPv6 literal: {after}")),
        }
    } else if rest.matches(':').count() >= 2 && !rest.contains(":/") {
        // Unbracketed IPv6 literal: whole thing is the host.
        (rest.to_string(), None)
    } else {
        match rest.split_once(':') {
            Some((h, t)) => (h.to_string(), Some(t)),
            None => (rest.to_string(), None),
        }
    };
    if host.is_empty() {
        return Err("empty host".into());
    }

    let (port, device) = match tail {
        None => (None, None),
        Some(t) => {
            let (p, d) = match t.split_once(':') {
                Some((p, d)) => (p, Some(d)),
                None => (t, None),
            };
            let port = if p.is_empty() {
                None
            } else {
                Some(p.parse::<u16>().map_err(|_| format!("bad port: {p}"))?)
            };
            let device = d.filter(|s| !s.is_empty()).map(str::to_string);
            (port, device)
        }
    };

    if proto == Proto::Raw && port.is_none() {
        return Err("tcp:// requires an explicit port".into());
    }
    if proto == Proto::Raw && device.is_some() {
        return Err("a device path only makes sense with gpsd".into());
    }
    // Naming a device implies gpsd.
    let proto = if device.is_some() && proto == Proto::Auto {
        Proto::Gpsd
    } else {
        proto
    };

    Ok(Target {
        host,
        port: port.unwrap_or(GPSD_PORT),
        device,
        proto,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        let t = parse("ntp.home.tany.org:4001").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.proto),
            ("ntp.home.tany.org", 4001, Proto::Auto)
        );
        let t = parse("localhost").unwrap();
        assert_eq!(t.port, 2947);
        let t = parse("pi4:2947:/dev/ttyAMA0").unwrap();
        assert_eq!(t.device.as_deref(), Some("/dev/ttyAMA0"));
        assert_eq!(t.proto, Proto::Gpsd);
        let t = parse("[2001:db8::1]:4001").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("2001:db8::1", 4001));
        assert_eq!(t.display(), "[2001:db8::1]:4001");
        let t = parse("2001:db8::1").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("2001:db8::1", 2947));
        let t = parse("tcp://192.168.111.12:4001").unwrap();
        assert_eq!(t.proto, Proto::Raw);
        assert!(parse("tcp://host").is_err());
        assert!(parse("host:notaport").is_err());
        let t = parse("gpsd://ghost").unwrap();
        assert_eq!((t.port, t.proto), (2947, Proto::Gpsd));
    }
}
