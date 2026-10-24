//! metronomon: per-source PPS statistics and the screen layout.
//!
//! Offset convention: `offset = assert timestamp − nearest UTC second`, i.e.
//! the system clock reading at the moment of the pulse. Positive means the
//! system clock is ahead of the pulse.

use crate::app::{fmt_tod, fmt_utc};
use crate::pps::{Caps, Pulse};
use crate::term::{st, Canvas};
use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

pub const KEEP: usize = 600; // 10 minutes at 1 PPS
pub const RECENT_KEEP: usize = 200;
/// A pulse farther than this from the reference offset is not "the" pulse
/// for its second (glitch, ringing, a second source on the same line...).
pub const ONTIME_NS: i64 = 100_000_000;
/// No pulse for this long is shown as a stall.
pub const STALL: Duration = Duration::from_millis(2500);
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
}

impl App {
    pub fn new(labels: Vec<String>, ascii: bool) -> Self {
        App {
            tracks: labels.into_iter().map(Track::new).collect(),
            recent: VecDeque::new(),
            ascii,
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
            }
            Ev::Timeout(i) => {
                self.tracks[i].timeouts += 1;
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
                            .map(|v| format!("{:.9} s", v as f64 / 1e9))
                            .unwrap_or_else(|| "-".into());
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Us,
    Ms,
}

impl Unit {
    pub fn for_track(t: &Track) -> Unit {
        let m = t
            .hist
            .iter()
            .map(|s| s.offset_ns.unsigned_abs())
            .max()
            .unwrap_or(0);
        if m >= 1_000_000 {
            Unit::Ms
        } else {
            Unit::Us
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Unit::Us => "us",
            Unit::Ms => "ms",
        }
    }
    pub fn scale(self) -> f64 {
        match self {
            Unit::Us => 1e3,
            Unit::Ms => 1e6,
        }
    }
    pub fn fmt(self, ns: f64) -> String {
        format!("{:+.3} {}", ns / self.scale(), self.name())
    }
    pub fn num(self, ns: f64) -> String {
        format!("{:+.3}", ns / self.scale())
    }
}

// ------------------------------------------------------------------ UI ---

const BOX_FULL: usize = 11; // with a 3-row sparkline
const BOX_SMALL: usize = 7;
pub const MIN_W: usize = 80;
pub const MIN_H: usize = 16;

pub fn render(app: &App, now: Instant, wall: SystemTime, w: usize, h: usize) -> Canvas {
    let mut c = Canvas::new(w, h, app.ascii);
    if w < MIN_W || h < MIN_H {
        c.put(
            0,
            0,
            &format!("terminal too small: {w}x{h}, need {MIN_W}x{MIN_H}"),
            st::BOLD,
        );
        c.put(0, 1, "q to quit", 0);
        return c;
    }
    c.fill(0, 0, w, st::REV);
    let left = format!(" metronomon  {} source(s)", app.tracks.len());
    c.put(0, 0, &left, st::REV | st::BOLD);
    let right = format!("host {} ", fmt_utc(wall));
    c.put(w - right.len(), 0, &right, st::REV);

    let n = app.tracks.len().max(1);
    let avail = h - 2;
    // Prefer full boxes and a recent log of at least 5 lines.
    let box_h = if n * BOX_FULL + 5 <= avail {
        BOX_FULL
    } else if n * BOX_SMALL + 3 <= avail {
        BOX_SMALL
    } else {
        (avail / n).max(4)
    };
    let mut y = 1;
    for t in &app.tracks {
        if y + box_h > h - 1 {
            break;
        }
        track_box(&mut c, t, now, y, w, box_h);
        y += box_h;
    }
    if h - 1 > y + 2 {
        recent_box(&mut c, app, y, w, h - 1 - y);
    }
    c.fill(0, h - 1, w, st::REV);
    c.put(
        0,
        h - 1,
        " q:quit c:clear | offset = system clock at pulse - UTC second (+ = clock ahead)",
        st::REV,
    );
    c
}

fn track_box(c: &mut Canvas, t: &Track, now: Instant, y: usize, w: usize, h: usize) {
    let mut title = t.label.clone();
    if !t.name.is_empty() {
        title.push_str(&format!(" | {}", t.name));
    }
    if t.caps != Caps::default() {
        title.push_str(&format!(" | {}", t.caps.describe()));
    }
    c.boxed(0, y, w, h, &title);
    let unit = Unit::for_track(t);
    let inner = w - 2;
    let mut line = y + 1;
    let put = |c: &mut Canvas, line: &mut usize, s: &str, style: u8| {
        if *line < y + h - 1 {
            let s: String = s.chars().take(inner).collect();
            c.put(1, *line, &s, style);
        }
        *line += 1;
    };

    // Status line
    let (state, sstyle) = match (&t.state, t.stalled_for(now)) {
        (State::Error(e), _) => (format!("ERROR {e}"), st::RED | st::BOLD),
        (State::Opening, _) => ("opening".to_string(), st::DIM),
        (State::Ok, Some(d)) => (format!("NO PULSE for {}s", d.as_secs()), st::RED | st::BOLD),
        (State::Ok, None) if t.hist.is_empty() => ("waiting for pulses".to_string(), st::YELLOW),
        (State::Ok, None) => ("OK".to_string(), st::GREEN | st::BOLD),
    };
    let mut s = format!("{state:<10}");
    if let (State::Ok, Some(last)) = (&t.state, t.hist.back()) {
        s.push_str(&format!(
            "  seq {}  last {}",
            last.seq,
            unit.fmt(last.offset_ns as f64)
        ));
        if let Some(iv) = last.interval_ns {
            s.push_str(&format!("  intvl {:.9} s", iv as f64 / 1e9));
        }
        let width = if let Some(wd) = last.width_ns {
            format!("  width {:.3} ms", wd as f64 / 1e6)
        } else if t.caps.clear && !t.clear_seen {
            "  width: clear not captured".to_string()
        } else {
            String::new()
        };
        s.push_str(&width);
    }
    put(c, &mut line, &s, sstyle);
    if h < 5 || (matches!(t.state, State::Error(_)) && t.hist.is_empty()) {
        return;
    }

    // Statistics
    put(
        c,
        &mut line,
        &format!(
            "{:<8}{:>5} {:>11} {:>10} {:>11} {:>11} {:>10}   [{}]",
            "",
            "n",
            "mean",
            "sd",
            "min",
            "max",
            "drms",
            unit.name()
        ),
        st::BOLD,
    );
    for (label, secs) in [("1 min", 60), ("10 min", KEEP)] {
        let v = t.window(secs);
        let s = stats(&v);
        let text = if s.n == 0 {
            format!("{label:<8}{:>5}", 0)
        } else {
            format!(
                "{label:<8}{:>5} {:>11} {:>10.3} {:>11} {:>11} {:>10.3}",
                s.n,
                unit.num(s.mean),
                s.sd / unit.scale(),
                unit.num(s.min as f64),
                unit.num(s.max as f64),
                s.drms / unit.scale()
            )
        };
        put(c, &mut line, &text, 0);
    }
    let bad = t.missing + t.extra + t.reader_gaps > 0;
    let mut counts = format!(
        "pulses {}  missing {}  extra {}  reader-gaps {}  timeouts {}",
        t.total, t.missing, t.extra, t.reader_gaps, t.timeouts
    );
    if t.stepped > 0 {
        counts.push_str(&format!("  re-baselined {}x", t.stepped));
    }
    put(
        c,
        &mut line,
        &counts,
        if bad { st::YELLOW } else { st::DIM },
    );

    // Sparkline of per-second offsets (newest on the right)
    let rows = (y + h - 1).saturating_sub(line);
    if rows > 0 {
        sparkline(c, t, unit, 1, line, inner, rows);
    }
}

fn sparkline(c: &mut Canvas, t: &Track, unit: Unit, x: usize, y: usize, w: usize, rows: usize) {
    const LABEL: usize = 11;
    if w <= LABEL + 4 {
        return;
    }
    let cols = w - LABEL - 1;
    let Some(last) = t.hist.back() else { return };
    let first_sec = last.sec - cols as i64 + 1;
    let mut series: Vec<Option<i64>> = vec![None; cols];
    let mut have = vec![false; cols];
    let oldest = t.hist.front().map(|s| s.sec).unwrap_or(last.sec);
    for s in t.hist.iter().filter(|s| s.sec >= first_sec) {
        let i = (s.sec - first_sec) as usize;
        series[i] = Some(s.offset_ns);
        have[i] = true;
    }
    let vals: Vec<i64> = series.iter().flatten().copied().collect();
    let (lo, hi) = (*vals.iter().min().unwrap(), *vals.iter().max().unwrap());
    let span = (hi - lo).max(1) as f64;
    let levels = rows * 8;
    let blocks = [
        ' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    let ascii = [' ', '.', '.', ':', ':', '|', '|', '#', '#'];
    for r in 0..rows {
        let label = match r {
            0 => unit.num(hi as f64),
            _ if r == rows - 1 => unit.num(lo as f64),
            _ => String::new(),
        };
        c.put(
            x,
            y + r,
            &format!("{label:>width$}", width = LABEL),
            st::DIM,
        );
    }
    for (i, v) in series.iter().enumerate() {
        let sec = first_sec + i as i64;
        let cx = x + LABEL + 1 + i;
        match v {
            Some(v) => {
                let lvl = (((v - lo) as f64 / span) * (levels - 1) as f64).round() as usize + 1;
                for r in 0..rows {
                    // row 0 is the top
                    let base = (rows - 1 - r) * 8;
                    let fill = lvl.saturating_sub(base).min(8);
                    let ch = if c.ascii { ascii[fill] } else { blocks[fill] };
                    c.put(cx, y + r, &ch.to_string(), st::CYAN);
                }
            }
            None if sec >= oldest => {
                // A second inside the history without an on-time pulse.
                c.put(cx, y + rows - 1, "!", st::RED | st::BOLD);
            }
            None => {}
        }
    }
}

fn recent_box(c: &mut Canvas, app: &App, y: usize, w: usize, h: usize) {
    c.boxed(0, y, w, h, "Recent pulses");
    let rows = h - 2;
    let start = app.recent.len().saturating_sub(rows);
    let multi = app.tracks.len() > 1;
    for (i, r) in app.recent.iter().skip(start).enumerate() {
        let mut x = 1;
        if multi {
            let lbl: String = app.tracks[r.src].label.chars().take(12).collect();
            x = c.put(1, y + 1 + i, &format!("{lbl:<12} "), st::DIM);
        }
        let text: String = r.text.chars().take(w - 1 - x).collect();
        c.put(x, y + 1 + i, &text, r.style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulse(seq: u32, sec: i64, nsec: i32) -> Pulse {
        Pulse {
            assert_seq: seq,
            assert_sec: sec,
            assert_nsec: nsec,
            clear_seq: 0,
            clear_sec: 0,
            clear_nsec: 0,
        }
    }

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

    #[test]
    fn render_sizes_never_panic() {
        let mut app = App::new(
            vec!["/dev/pps0".into(), "sim:uart".into(), "sim:bad".into()],
            false,
        );
        let now = Instant::now();
        let mut s = crate::pps::SimState::new(crate::pps::SimProfile::bad(), 3);
        for sec in 0..700 {
            for p in s.second(1_000_000 + sec) {
                app.handle(crate::pps::Ev::Pulse(1, p), now);
            }
        }
        app.handle(crate::pps::Ev::Error(0, "Permission denied".into()), now);
        for (w, h) in [
            (80, 16),
            (80, 24),
            (100, 40),
            (200, 60),
            (79, 10),
            (120, 18),
        ] {
            for ascii in [false, true] {
                app.ascii = ascii;
                let c = render(&app, now, SystemTime::now(), w, h);
                assert_eq!(c.text().lines().count(), h);
            }
        }
    }
}
