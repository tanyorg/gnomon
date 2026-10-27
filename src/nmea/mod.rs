//! NMEA 0183 sentence parsing and receiver state accumulation.
//!
//! Handles NMEA 2.x through 4.11 style output (multi-constellation talkers,
//! GSA system ID, GSV signal ID).
//!
//! - [`sentence`]: framing, checksum and field parsing of one sentence
//! - [`sats`]: satellites in view and in use
//! - [`stats`]: per-sentence counts and output intervals
//! - this module: the fix and the epoch / lag bookkeeping

mod sats;
mod sentence;
mod stats;

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

pub use sats::{SatTracker, SatView, SAT_STALE, USED_STALE};
pub use sentence::{coord, split, tod, CkStatus, Sentence, Sys};
pub use stats::SentenceStat;

use sentence::{f, s, u};

// Resource limits. Input comes from untrusted peers (or line noise at a wrong
// baud rate), so every table keyed by received data is bounded.
/// Distinct sentence addresses tracked; the rest are lumped into `OTHER`.
pub const MAX_STAT_KEYS: usize = 128;
/// Statistics bucket for addresses beyond `MAX_STAT_KEYS`.
pub const OTHER: &str = "~other";
/// Longest address kept as a statistics key (real ones are 5-6 chars).
const MAX_ADDR_LEN: usize = 16;

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
    sats: SatTracker,
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
        let key = if snt.addr.len() > MAX_ADDR_LEN {
            OTHER
        } else if self.stats.contains_key(snt.addr) || self.stats.len() < MAX_STAT_KEYS {
            snt.addr
        } else {
            OTHER
        };
        let st = self.stats.entry(key.to_string()).or_default();
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
            "GSV" => self.sats.gsv(talker, fl, now),
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
        if let Some(lay) = sentence::gsa_layout(fl) {
            self.fix.pdop = f(fl, lay.dop);
            self.fix.hdop = f(fl, lay.dop + 1).or(self.fix.hdop);
            self.fix.vdop = f(fl, lay.dop + 2);
        }
        self.sats.gsa(talker, fl, now);
    }

    pub fn used_set(&self, now: Instant) -> HashSet<(Sys, u16)> {
        self.sats.used_set(now)
    }

    /// Merged satellite view across talkers and signals, sorted for display.
    pub fn satellites(&self, now: Instant) -> Vec<SatView> {
        self.sats.satellites(now)
    }
}

#[cfg(test)]
mod tests {
    use super::sats::{MAX_GSV_KEYS, MAX_GSV_SATS, MAX_USED_KEYS};
    use super::*;
    use std::time::Duration;

    fn ck(body: &str) -> String {
        let c = body.bytes().fold(0u8, |a, b| a ^ b);
        format!("${body}*{c:02X}")
    }

    /// SEIKO TS-2560: GN-talker GSA with 16 PRN slots and a trailing system
    /// ID. Galileo 06/09 must not be taken for GPS, and the GPS set must not
    /// be overwritten by the Galileo one.
    #[test]
    fn gsa_sixteen_slots_ts2560() {
        let mut n = Nmea::default();
        let t0 = Instant::now();
        let host = 7.0 * 3600.0 + 28.0 * 60.0 + 26.0;
        let lines = [
            "$GNGNS,072845.000,3334.8306,N,13015.6643,E,AAA,09,1.0,17.7,27.0,,,V*68",
            "$GNGSA,A,3,09,27,28,31,95,,,,,,,,,,,,1.8,1.0,1.5,1*35",
            "$GNGSA,A,3,71,84,,,,,,,,,,,,,,,1.8,1.0,1.5,2*34",
            "$GNGSA,A,3,06,09,,,,,,,,,,,,,,,1.8,1.0,1.5,3*30",
        ];
        for l in &lines {
            assert_eq!(n.feed(l, t0, host), Feed::Good, "{l}");
        }
        let mut used: Vec<_> = n.used_set(t0).into_iter().collect();
        used.sort();
        let mut want = vec![
            (Sys::Gps, 9),
            (Sys::Gps, 27),
            (Sys::Gps, 28),
            (Sys::Gps, 31),
            (Sys::Gps, 95),
            (Sys::Glonass, 71),
            (Sys::Glonass, 84),
            (Sys::Galileo, 6),
            (Sys::Galileo, 9),
        ];
        want.sort();
        assert_eq!(used, want);
        assert_eq!(n.fix.pdop, Some(1.8));
        assert_eq!(n.fix.hdop, Some(1.0));
        assert_eq!(n.fix.vdop, Some(1.5));
    }

    #[test]
    fn gsa_layout_variants() {
        use super::sentence::gsa_layout;
        let f = |s: &'static str| s.split(',').collect::<Vec<_>>();
        // NMEA 2.x/3.x: 12 slots, no system ID.
        let l = gsa_layout(&f("A,3,05,13,15,,,,,,,,,,1.10,0.62,0.91")).unwrap();
        assert_eq!((l.prns, l.dop, l.sysid), (2..14, 14, None));
        // NMEA 4.10: 12 slots + system ID.
        let l = gsa_layout(&f("A,3,05,13,15,,,,,,,,,,1.10,0.62,0.91,1")).unwrap();
        assert_eq!((l.prns, l.dop, l.sysid), (2..14, 14, Some(1)));
        // Legacy sentence with an empty VDOP is not mistaken for 4.10.
        let l = gsa_layout(&f("A,3,05,,,,,,,,,,,,1.10,0.62,")).unwrap();
        assert_eq!(l.sysid, None);
        assert!(gsa_layout(&f("A,3")).is_none());
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
    fn tables_are_bounded() {
        let mut n = Nmea::default();
        let t0 = Instant::now();
        // Flood of distinct sentence addresses.
        for i in 0..10_000u32 {
            n.feed(&ck(&format!("XX{i:05}")), t0, 0.0);
        }
        assert!(n.stats.len() <= MAX_STAT_KEYS + 1);
        assert!(n.stats.get(OTHER).map_or(0, |s| s.count) > 9000);
        // Over-long address goes to the overflow bucket.
        n.feed(&ck(&format!("GP{}", "A".repeat(100))), t0, 0.0);
        assert!(n.stats.keys().all(|k| k.len() <= MAX_ADDR_LEN));
        // Distinct talker/signal pairs for GSV and GSA.
        for t in 0..36u32 * 36 {
            let a = char::from_digit(t / 36, 36).unwrap().to_ascii_uppercase();
            let b = char::from_digit(t % 36, 36).unwrap().to_ascii_uppercase();
            for sig in [1u8, 7, 200] {
                n.feed(
                    &ck(&format!("{a}{b}GSV,1,1,01,05,45,120,40,{sig}")),
                    t0,
                    0.0,
                );
            }
            n.feed(
                &ck(&format!("{a}{b}GSA,A,3,05,13,,,,,,,,,,,1.1,0.6,0.9")),
                t0,
                0.0,
            );
        }
        assert!(n.sats.gsv_done.len() <= MAX_GSV_KEYS);
        assert!(n.sats.gsv_stage.len() <= MAX_GSV_KEYS);
        assert!(n.sats.used.len() <= MAX_USED_KEYS);
    }

    #[test]
    fn gsv_hostile_counters() {
        let mut n = Nmea::default();
        let t0 = Instant::now();
        // Message number at u32::MAX: used to overflow `num + 1` (debug panic).
        n.feed(&ck("GPGSV,4294967295,4294967295,01,05,45,120,40"), t0, 0.0);
        n.feed(&ck("GPGSV,4294967295,1,01,05,45,120,40"), t0, 0.0);
        n.feed(&ck("GPGSV,0,0,01,05,45,120,40"), t0, 0.0);
        n.feed(&ck("GPGSV,2,3,01,05,45,120,40"), t0, 0.0);
        assert!(n.sats.gsv_stage.is_empty() && n.sats.gsv_done.is_empty());
        // A never-ending cycle of max-length messages stays bounded.
        n.feed(&ck("GPGSV,32,1,99,01,1,1,1"), t0, 0.0);
        let blk: String = (1..=2000).map(|p| format!(",{p},10,10,30")).collect();
        for m in 2..=31 {
            n.feed(&ck(&format!("GPGSV,32,{m},99{blk}")), t0, 0.0);
        }
        assert!(n
            .sats
            .gsv_stage
            .values()
            .all(|(_, v)| v.len() <= MAX_GSV_SATS));
        // Still works normally afterwards.
        n.feed(&ck("GPGSV,1,1,01,05,45,120,40"), t0, 0.0);
        assert_eq!(n.satellites(t0).len(), 1);
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
