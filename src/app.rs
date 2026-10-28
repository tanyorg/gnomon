//! Application state: everything the UI shows, updated from source events.

use crate::json;
use crate::nmea::{Feed, Nmea};
use crate::source::Kind;
use crate::term::st;
use std::collections::VecDeque;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const RAW_KEEP: usize = 2000;
/// gpsd devices remembered from DEVICES/DEVICE reports. A real gpsd has a
/// handful; the bound keeps a hostile peer from growing the list forever.
pub const MAX_DEVICES: usize = 16;

#[derive(Clone)]
pub struct RawLine {
    pub ts: String,
    pub text: String,
    pub style: u8,
    pub json: bool,
}

#[derive(Default)]
pub struct Device {
    pub path: String,
    pub driver: String,
    pub bps: Option<i64>,
}

/// A (real - clock) measurement from gpsd PPS or TOFF reports.
#[derive(Clone, Copy)]
pub struct Offset {
    pub ns: i128,
    pub precision: Option<i64>,
    pub at: Instant,
}

#[derive(Default)]
pub struct Gpsd {
    pub release: Option<String>,
    pub proto: Option<String>,
    pub devices: Vec<Device>,
    pub tpv_time: Option<String>,
    pub tpv_mode: Option<i64>,
    pub tpv_ept: Option<f64>,
    pub tpv_at: Option<Instant>,
    pub pps: Option<Offset>,
    pub toff: Option<Offset>,
    pub error: Option<String>,
}

pub struct App {
    pub target: String,
    pub kind: Kind,
    pub connected: bool,
    pub status: String,
    pub reconnects: u64,
    pub nmea: Nmea,
    pub gpsd: Gpsd,
    pub raw: VecDeque<RawLine>,
    pub frozen: Option<Vec<RawLine>>,
    pub show_json: bool,
    pub rx_lines: u64,
    pub ascii: bool,
}

/// (year, month, day) from days since 1970-01-01 (H. Hinnant's algorithm).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn unix(t: SystemTime) -> f64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

pub fn fmt_tod(sod: f64, decimals: usize) -> String {
    let scale = 10f64.powi(decimals as i32);
    let sod = ((sod * scale).round() / scale).rem_euclid(86400.0);
    let h = (sod / 3600.0).floor();
    let m = ((sod - h * 3600.0) / 60.0).floor();
    let s = sod - h * 3600.0 - m * 60.0;
    let w = if decimals > 0 { decimals + 3 } else { 2 };
    format!(
        "{:02}:{:02}:{:0w$.decimals$}",
        h as u32,
        m as u32,
        s,
        w = w,
        decimals = decimals
    )
}

pub fn fmt_utc(t: SystemTime) -> String {
    let u = unix(t);
    let days = u.div_euclid(86400.0) as i64;
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {}Z",
        fmt_tod(u.rem_euclid(86400.0), 0)
    )
}

impl App {
    pub fn new(target: String, ascii: bool) -> Self {
        App {
            target,
            kind: Kind::Unknown,
            connected: false,
            status: String::new(),
            reconnects: 0,
            nmea: Nmea::default(),
            gpsd: Gpsd::default(),
            raw: VecDeque::new(),
            frozen: None,
            show_json: false,
            rx_lines: 0,
            ascii,
        }
    }

    pub fn clear(&mut self) {
        self.nmea.clear();
        let keep = (
            self.gpsd.release.take(),
            self.gpsd.proto.take(),
            std::mem::take(&mut self.gpsd.devices),
        );
        self.gpsd = Gpsd::default();
        (self.gpsd.release, self.gpsd.proto, self.gpsd.devices) = keep;
        self.raw.clear();
        self.frozen = None;
        self.rx_lines = 0;
    }

    pub fn toggle_pause(&mut self) {
        self.frozen = match self.frozen {
            Some(_) => None,
            None => Some(self.raw.iter().cloned().collect()),
        };
    }

    pub fn line(&mut self, ts: SystemTime, text: String, now: Instant) {
        self.rx_lines += 1;
        let u = unix(ts);
        let host_tod = u.rem_euclid(86400.0);
        let mut is_json = false;
        let style = if text.starts_with('{') {
            is_json = true;
            match json::parse(&text) {
                Ok(j) => {
                    self.gpsd_report(&j, now);
                    st::CYAN
                }
                Err(_) => st::RED,
            }
        } else {
            match self.nmea.feed(&text, now, host_tod) {
                Feed::Good => 0,
                Feed::NoChecksum => st::YELLOW,
                Feed::BadChecksum => st::RED | st::BOLD,
                Feed::NotNmea => st::DIM,
            }
        };
        self.raw.push_back(RawLine {
            ts: fmt_tod(host_tod, 3),
            text,
            style,
            json: is_json,
        });
        while self.raw.len() > RAW_KEEP {
            self.raw.pop_front();
        }
    }

    fn device(j: &json::Json) -> Device {
        Device {
            path: j.str("path").unwrap_or("?").to_string(),
            driver: j.str("driver").unwrap_or("").to_string(),
            bps: j.i64("bps"),
        }
    }

    fn offset(j: &json::Json, now: Instant) -> Option<Offset> {
        let rs = j.i64("real_sec")? as i128;
        let rn = j.i64("real_nsec")? as i128;
        let cs = j.i64("clock_sec")? as i128;
        let cn = j.i64("clock_nsec")? as i128;
        Some(Offset {
            ns: (rs - cs) * 1_000_000_000 + (rn - cn),
            precision: j.i64("precision"),
            at: now,
        })
    }

    fn gpsd_report(&mut self, j: &json::Json, now: Instant) {
        let g = &mut self.gpsd;
        match j.str("class") {
            Some("VERSION") => {
                g.release = j.str("release").map(str::to_string);
                if let (Some(a), Some(b)) = (j.i64("proto_major"), j.i64("proto_minor")) {
                    g.proto = Some(format!("{a}.{b}"));
                }
            }
            Some("DEVICES") => {
                g.devices = j
                    .arr("devices")
                    .unwrap_or(&[])
                    .iter()
                    .take(MAX_DEVICES)
                    .map(Self::device)
                    .collect();
            }
            Some("DEVICE") => {
                let d = Self::device(j);
                let full = g.devices.len() >= MAX_DEVICES;
                match g.devices.iter_mut().find(|x| x.path == d.path) {
                    Some(x) => {
                        if !d.driver.is_empty() {
                            x.driver = d.driver;
                        }
                        x.bps = d.bps.or(x.bps);
                    }
                    None if !full => g.devices.push(d),
                    None => {}
                }
            }
            Some("TPV") => {
                g.tpv_time = j.str("time").map(str::to_string).or(g.tpv_time.take());
                g.tpv_mode = j.i64("mode");
                g.tpv_ept = j.f64("ept");
                g.tpv_at = Some(now);
            }
            Some("PPS") => g.pps = Self::offset(j, now).or(g.pps),
            Some("TOFF") => g.toff = Self::offset(j, now).or(g.toff),
            Some("ERROR") => g.error = j.str("message").map(str::to_string),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_748), (2026, 10, 22));
        assert_eq!(civil(11_016), (2000, 2, 29));
        assert_eq!(fmt_tod(45296.125, 3), "12:34:56.125");
        assert_eq!(fmt_tod(59.9996, 3), "00:01:00.000");
    }

    #[test]
    fn pps_offset() {
        let mut a = App::new("x".into(), true);
        let now = Instant::now();
        a.line(
            SystemTime::now(),
            r#"{"class":"PPS","device":"/dev/pps0","real_sec":100,"real_nsec":0,"clock_sec":99,"clock_nsec":999999000,"precision":-20}"#.into(),
            now,
        );
        assert_eq!(a.gpsd.pps.unwrap().ns, 1000);
    }

    #[test]
    fn devices_bounded() {
        let mut a = App::new("x".into(), true);
        let now = Instant::now();
        let dev = |p: &str, d: &str| format!(r#"{{"class":"DEVICE","path":"{p}","driver":"{d}"}}"#);
        a.line(SystemTime::now(), dev("/dev/ttyAMA0", ""), now);
        for i in 0..1000 {
            a.line(SystemTime::now(), dev(&format!("/dev/x{i}"), ""), now);
        }
        assert_eq!(a.gpsd.devices.len(), MAX_DEVICES);
        // Known devices are still updated once the list is full.
        a.line(SystemTime::now(), dev("/dev/ttyAMA0", "u-blox"), now);
        assert_eq!(a.gpsd.devices[0].driver, "u-blox");

        let many: Vec<String> = (0..100)
            .map(|i| format!(r#"{{"path":"/dev/y{i}"}}"#))
            .collect();
        let devices = format!(r#"{{"class":"DEVICES","devices":[{}]}}"#, many.join(","));
        a.line(SystemTime::now(), devices, now);
        assert_eq!(a.gpsd.devices.len(), MAX_DEVICES);
        assert_eq!(a.gpsd.devices[0].path, "/dev/y0");
    }
}
