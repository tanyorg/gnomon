//! Per-source pulse bookkeeping: assigning pulses to UTC seconds, detecting
//! missing and extra pulses, and the window statistics. No display code.
//!
//! Offset convention: `offset = assert timestamp - nearest UTC second`, i.e.
//! the system clock reading at the moment of the pulse. Positive means the
//! system clock is ahead of the pulse.

use crate::pps::{Caps, Pulse};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const KEEP: usize = 600; // 10 minutes at 1 PPS
/// A pulse farther than this from the reference offset is not "the" pulse
/// for its second (glitch, ringing, a second source on the same line...).
pub const ONTIME_NS: i64 = 100_000_000;
/// No pulse for this long is shown as a stall.
pub const STALL: Duration = Duration::from_millis(2500);
/// A fetch timeout only counts when the last pulse (or the open) is at least
/// this old; the kernel can report one spuriously right after opening.
pub const TIMEOUT_GAP: Duration = Duration::from_millis(1500);
const NS: i64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub seq: u32,
    /// UTC second this pulse marks.
    pub sec: i64,
    pub offset_ns: i64,
    /// Since the previous on-time pulse, when that was the previous second.
    pub interval_ns: Option<i64>,
    pub width_ns: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Opening,
    Ok,
    Error(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub n: usize,
    pub mean: f64,
    pub sd: f64,
    pub min: i64,
    pub max: i64,
    /// RMS of differences between consecutive seconds' offsets.
    pub drms: f64,
}

pub fn stats(s: &[Sample]) -> Stats {
    if s.is_empty() {
        return Stats::default();
    }
    let n = s.len();
    let mean = s.iter().map(|x| x.offset_ns as f64).sum::<f64>() / n as f64;
    let var = if n > 1 {
        s.iter()
            .map(|x| (x.offset_ns as f64 - mean).powi(2))
            .sum::<f64>()
            / (n - 1) as f64
    } else {
        0.0
    };
    let (mut dsum, mut dn) = (0.0, 0usize);
    for w in s.windows(2) {
        if w[1].sec == w[0].sec + 1 {
            dsum += ((w[1].offset_ns - w[0].offset_ns) as f64).powi(2);
            dn += 1;
        }
    }
    Stats {
        n,
        mean,
        sd: var.sqrt(),
        min: s.iter().map(|x| x.offset_ns).min().unwrap_or(0),
        max: s.iter().map(|x| x.offset_ns).max().unwrap_or(0),
        drms: if dn > 0 {
            (dsum / dn as f64).sqrt()
        } else {
            0.0
        },
    }
}

pub struct Track {
    pub label: String,
    pub name: String,
    pub caps: Caps,
    pub state: State,
    pub hist: VecDeque<Sample>,
    pub last_pulse: Option<Pulse>,
    pub last_at: Option<Instant>,
    pub opened_at: Option<Instant>,
    /// Offset that on-time pulses cluster around (median of recent ones).
    reference: Option<i64>,
    /// Consecutive off-time pulses, to re-baseline after a clock step.
    off_run: Vec<i64>,
    pub total: u64,
    pub missing: u64,
    pub extra: u64,
    pub reader_gaps: u64,
    pub timeouts: u64,
    pub clear_seen: bool,
    pub stepped: u64,
}

/// Result of feeding one pulse, for the recent-pulses log.
#[derive(Debug, Clone, PartialEq)]
pub enum Fed {
    Duplicate,
    WidthUpdate,
    OnTime(Sample, u64), // sample, seconds missing before it
    Extra(i64, i64),     // second, offset
}

impl Track {
    pub fn new(label: String) -> Self {
        Track {
            label,
            name: String::new(),
            caps: Caps::default(),
            state: State::Opening,
            hist: VecDeque::new(),
            last_pulse: None,
            last_at: None,
            opened_at: None,
            reference: None,
            off_run: Vec::new(),
            total: 0,
            missing: 0,
            extra: 0,
            reader_gaps: 0,
            timeouts: 0,
            clear_seen: false,
            stepped: 0,
        }
    }

    pub fn clear_stats(&mut self) {
        let keep = (
            self.label.clone(),
            self.name.clone(),
            self.caps,
            self.state.clone(),
        );
        *self = Track::new(keep.0);
        (self.name, self.caps, self.state) = (keep.1, keep.2, keep.3);
    }

    fn width_of(&self, p: &Pulse, assert_ns: i128) -> Option<i64> {
        if !self.caps.clear || p.clear_seq == 0 {
            return None;
        }
        let w = p.clear_ns() - assert_ns;
        (w > 0 && w < NS as i128).then_some(w as i64)
    }

    fn update_reference(&mut self) {
        let mut v: Vec<i64> = self
            .hist
            .iter()
            .rev()
            .take(15)
            .map(|s| s.offset_ns)
            .collect();
        if v.is_empty() {
            return;
        }
        v.sort_unstable();
        self.reference = Some(v[v.len() / 2]);
    }

    pub fn feed(&mut self, p: Pulse, now: Instant) -> Fed {
        if let Some(lp) = self.last_pulse {
            if lp.assert_seq == p.assert_seq {
                // Woken by a clear event (capture-clear enabled): the clear
                // belongs to the last pulse.
                if p.clear_seq != lp.clear_seq {
                    self.clear_seen = true;
                    let w = self.width_of(&p, lp.assert_ns());
                    self.last_pulse = Some(p);
                    if let (Some(w), Some(last)) = (w, self.hist.back_mut()) {
                        if last.seq == p.assert_seq {
                            last.width_ns = Some(w);
                        }
                    }
                    return Fed::WidthUpdate;
                }
                return Fed::Duplicate;
            }
            let gap = p.assert_seq.wrapping_sub(lp.assert_seq);
            if gap > 1 && gap < 1_000_000 {
                self.reader_gaps += (gap - 1) as u64;
            }
            if p.clear_seq != lp.clear_seq {
                self.clear_seen = true;
            }
        }
        self.total += 1;
        self.last_at = Some(now);
        self.last_pulse = Some(p);

        let total = p.assert_ns();
        let sec = (total + NS as i128 / 2).div_euclid(NS as i128) as i64;
        let offset = (total - sec as i128 * NS as i128) as i64;

        let ontime = match self.reference {
            None => true,
            Some(r) => (offset - r).abs() < ONTIME_NS,
        };
        if !ontime {
            // A clock step makes every pulse look off-time: re-baseline once
            // several consecutive pulses agree with each other.
            self.off_run.push(offset);
            let consistent = self.off_run.len() >= 5
                && self.off_run.iter().max().unwrap() - self.off_run.iter().min().unwrap()
                    < ONTIME_NS;
            if !consistent {
                self.extra += 1;
                return Fed::Extra(sec, offset);
            }
            self.stepped += 1;
            self.extra = self.extra.saturating_sub(self.off_run.len() as u64 - 1);
            self.hist.clear();
            self.reference = Some(offset);
        }
        self.off_run.clear();

        let mut missed = 0;
        let mut interval = None;
        if let Some(prev) = self.hist.back() {
            if sec == prev.sec {
                // Second pulse inside the same second.
                self.extra += 1;
                return Fed::Extra(sec, offset);
            }
            if sec > prev.sec + 1 {
                missed = (sec - prev.sec - 1) as u64;
                self.missing += missed;
            } else if sec == prev.sec + 1 {
                interval = Some(NS + offset - prev.offset_ns);
            }
        }
        let s = Sample {
            seq: p.assert_seq,
            sec,
            offset_ns: offset,
            interval_ns: interval,
            width_ns: self.width_of(&p, total),
        };
        self.hist.push_back(s);
        while self.hist.len() > KEEP {
            self.hist.pop_front();
        }
        self.update_reference();
        Fed::OnTime(s, missed)
    }

    pub fn window(&self, secs: usize) -> Vec<Sample> {
        let Some(last) = self.hist.back() else {
            return Vec::new();
        };
        let from = last.sec - secs as i64 + 1;
        self.hist
            .iter()
            .filter(|s| s.sec >= from)
            .copied()
            .collect()
    }

    pub fn stalled_for(&self, now: Instant) -> Option<Duration> {
        let d = now.duration_since(self.last_at?);
        (d > STALL).then_some(d)
    }
}

#[cfg(test)]
pub(crate) fn pulse(seq: u32, sec: i64, nsec: i32) -> Pulse {
    Pulse {
        assert_seq: seq,
        assert_sec: sec,
        assert_nsec: nsec,
        clear_seq: 0,
        clear_sec: 0,
        clear_nsec: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_and_intervals() {
        let mut t = Track::new("x".into());
        let now = Instant::now();
        // ghost-like: +1.3us, +0.737us; pi4-like: -6.553us (previous second)
        assert!(
            matches!(t.feed(pulse(1, 100, 1300), now), Fed::OnTime(s, 0) if s.offset_ns == 1300 && s.sec == 100)
        );
        let Fed::OnTime(s, 0) = t.feed(pulse(2, 101, 737), now) else {
            panic!()
        };
        assert_eq!(s.interval_ns, Some(1_000_000_000 - 563));
        let Fed::OnTime(s, 0) = t.feed(pulse(3, 101, 999_993_447), now) else {
            panic!()
        };
        assert_eq!((s.sec, s.offset_ns), (102, -6553));
        assert_eq!(s.interval_ns, Some(1_000_000_000 - 7290));
        let st = stats(&t.window(60));
        assert_eq!((st.n, st.min, st.max), (3, -6553, 1300));
    }

    #[test]
    fn missing_extra_and_reader_gaps() {
        let mut t = Track::new("x".into());
        let now = Instant::now();
        t.feed(pulse(1, 100, 500), now);
        // kernel saw pulses 2,3 but we did not fetch them; second 103 next
        assert!(matches!(t.feed(pulse(4, 103, 400), now), Fed::OnTime(_, 2)));
        assert_eq!((t.missing, t.reader_gaps), (2, 2));
        // glitch half way through the second
        assert!(matches!(
            t.feed(pulse(5, 103, 450_000_000), now),
            Fed::Extra(..)
        ));
        // duplicate fetch of the same event
        assert_eq!(t.feed(pulse(5, 103, 450_000_000), now), Fed::Duplicate);
        // next genuine pulse is fine, interval measured from the real one
        let Fed::OnTime(s, 0) = t.feed(pulse(6, 104, 600), now) else {
            panic!()
        };
        assert_eq!(s.interval_ns, Some(1_000_000_200));
        // total counts distinct kernel events: 1, 4, 5 (extra), 6
        assert_eq!((t.extra, t.missing, t.total), (1, 2, 4));
    }

    #[test]
    fn rebaseline_after_clock_step() {
        let mut t = Track::new("x".into());
        let now = Instant::now();
        for i in 0..20 {
            t.feed(pulse(i + 1, 100 + i as i64, 1000), now);
        }
        // system clock steps by -300 ms: pulses now read .3 s past the second
        for i in 20..30 {
            t.feed(pulse(i + 1, 100 + i as i64, 300_000_000), now);
        }
        assert_eq!(t.stepped, 1);
        assert_eq!(t.extra, 0);
        assert_eq!(t.hist.back().unwrap().offset_ns, 300_000_000);
        assert!(t.hist.len() >= 5);
    }

    #[test]
    fn width_from_clear_events() {
        let mut t = Track::new("x".into());
        t.caps = Caps {
            assert: true,
            clear: true,
            can_wait: true,
        };
        let now = Instant::now();
        let mut p = pulse(1, 100, 1000);
        t.feed(p, now);
        // woken again by the clear edge 100 ms later
        p.clear_seq = 1;
        p.clear_sec = 100;
        p.clear_nsec = 100_001_000;
        assert_eq!(t.feed(p, now), Fed::WidthUpdate);
        assert_eq!(t.hist.back().unwrap().width_ns, Some(100_000_000));
        assert!(t.clear_seen);
    }

    #[test]
    fn stale_clear_is_not_a_width() {
        // ghost: clear_seq stuck at 1 with a timestamp from weeks ago
        let mut t = Track::new("x".into());
        t.caps = Caps {
            assert: true,
            clear: true,
            can_wait: true,
        };
        let now = Instant::now();
        for i in 0..3 {
            let mut p = pulse(10 + i, 1000 + i as i64, 700);
            p.clear_seq = 1;
            p.clear_sec = 500;
            t.feed(p, now);
        }
        assert!(t.hist.iter().all(|s| s.width_ns.is_none()));
        assert!(!t.clear_seen);
    }
}
