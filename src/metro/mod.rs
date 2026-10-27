//! metronomon: per-source PPS statistics and the screen layout.
//!
//! - [`track`]: per-source pulse bookkeeping and statistics (no display code)
//! - [`render`]: the screen
//! - this module: the application state shared by both, and event handling

mod render;
mod track;

pub use render::{render, spark_range, Unit, MIN_H, MIN_W};
pub use track::{stats, Fed, Sample, State, Stats, Track, KEEP, ONTIME_NS, STALL, TIMEOUT_GAP};

use crate::app::fmt_tod;
use crate::term::st;
use std::collections::VecDeque;
use std::time::Instant;

pub const RECENT_KEEP: usize = 200;

#[derive(Clone)]
pub struct Recent {
    pub src: usize,
    pub text: String,
    pub style: u8,
}

pub struct App {
    pub tracks: Vec<Track>,
    pub recent: VecDeque<Recent>,
    pub ascii: bool,
    /// Name of the machine the PPS sources are attached to.
    pub host: String,
}

extern "C" {
    fn gethostname(name: *mut std::ffi::c_char, len: usize) -> i32;
}

/// Short host name (up to the first dot), or "?" if unavailable.
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    let r = unsafe { gethostname(buf.as_mut_ptr() as *mut std::ffi::c_char, buf.len()) };
    if r != 0 {
        return "?".into();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    let short = name.split('.').next().unwrap_or("").to_string();
    if short.is_empty() {
        "?".into()
    } else {
        short
    }
}

impl App {
    pub fn new(labels: Vec<String>, ascii: bool) -> Self {
        App {
            tracks: labels.into_iter().map(Track::new).collect(),
            recent: VecDeque::new(),
            ascii,
            host: hostname(),
        }
    }

    fn log(&mut self, src: usize, text: String, style: u8) {
        self.recent.push_back(Recent { src, text, style });
        while self.recent.len() > RECENT_KEEP {
            self.recent.pop_front();
        }
    }

    pub fn clear(&mut self) {
        for t in &mut self.tracks {
            t.clear_stats();
        }
        self.recent.clear();
    }

    pub fn handle(&mut self, ev: crate::pps::Ev, now: Instant) {
        use crate::pps::Ev;
        match ev {
            Ev::Opened(i, name, caps) => {
                let t = &mut self.tracks[i];
                t.name = name;
                t.caps = caps;
                t.state = State::Ok;
                t.opened_at = Some(now);
            }
            Ev::Timeout(i) => {
                let t = &mut self.tracks[i];
                if let Some(since) = t.last_at.or(t.opened_at) {
                    if now.duration_since(since) >= TIMEOUT_GAP {
                        t.timeouts += 1;
                    }
                }
            }
            Ev::Error(i, e) => {
                self.tracks[i].state = State::Error(e);
            }
            Ev::Pulse(i, p) => {
                self.tracks[i].state = State::Ok;
                let unit = Unit::for_track(&self.tracks[i]);
                match self.tracks[i].feed(p, now) {
                    Fed::OnTime(s, missed) => {
                        if missed > 0 {
                            self.log(
                                i,
                                format!("{missed} pulse(s) missing before this one"),
                                st::RED,
                            );
                        }
                        let iv = s
                            .interval_ns
                            .map_or_else(|| "-".into(), |v| format!("{:.9} s", v as f64 / 1e9));
                        let w = s
                            .width_ns
                            .map(|v| format!("  width {:.3} ms", v as f64 / 1e6))
                            .unwrap_or_default();
                        let text = format!(
                            "seq {:>10}  {}Z  {:>12}  intvl {}{}",
                            s.seq,
                            fmt_tod(s.sec.rem_euclid(86400) as f64, 0),
                            unit.fmt(s.offset_ns as f64),
                            iv,
                            w
                        );
                        self.log(i, text, 0);
                    }
                    Fed::Extra(sec, off) => {
                        let text = format!(
                            "extra pulse in {}Z at {:+.6} s (not counted)",
                            fmt_tod(sec.rem_euclid(86400) as f64, 0),
                            off as f64 / 1e9
                        );
                        self.log(i, text, st::YELLOW);
                    }
                    Fed::Duplicate | Fed::WidthUpdate => {}
                }
            }
            Ev::Key(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::track::pulse;
    use super::*;
    use crate::pps::Caps;
    use std::time::Duration;

    #[test]
    fn startup_timeout_not_counted() {
        let mut app = App::new(vec!["x".into()], false);
        let t0 = Instant::now();
        app.handle(crate::pps::Ev::Opened(0, "n".into(), Caps::default()), t0);
        app.handle(crate::pps::Ev::Timeout(0), t0 + Duration::from_millis(200));
        assert_eq!(app.tracks[0].timeouts, 0);
        app.handle(
            crate::pps::Ev::Pulse(0, pulse(1, 100, 10)),
            t0 + Duration::from_millis(500),
        );
        app.handle(crate::pps::Ev::Timeout(0), t0 + Duration::from_millis(900));
        assert_eq!(app.tracks[0].timeouts, 0);
        // genuinely nothing for 2 s
        app.handle(crate::pps::Ev::Timeout(0), t0 + Duration::from_millis(2600));
        assert_eq!(app.tracks[0].timeouts, 1);
    }
}
