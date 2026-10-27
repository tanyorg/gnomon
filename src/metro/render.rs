//! The metronomon screen: one box per source and the recent-pulses log.

use super::track::{stats, State, Track, KEEP};
use super::App;
use crate::app::fmt_utc;
use crate::pps::Caps;
use crate::term::{st, Canvas};
use std::fmt::Write as _;
use std::time::{Instant, SystemTime};

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
    let host: String = app.host.chars().take(32).collect();
    let left = format!(" metronomon @ {host}  {} source(s)", app.tracks.len());
    c.put(0, 0, &left, st::REV | st::BOLD);
    let right = format!("clock {} ", fmt_utc(wall));
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
        let _ = write!(title, " | {}", t.name);
    }
    if t.caps != Caps::default() {
        let _ = write!(title, " | {}", t.caps.describe());
        // The device can capture clear edges, but none has arrived yet
        // (typical for pps-ldisc on DCD). Say so once, in the title.
        if t.caps.clear && !t.clear_seen && !t.hist.is_empty() {
            title.push_str(" (no clear edges)");
        }
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
        // Optional fields, in display order, with a drop priority
        // (higher = dropped first when the line does not fit).
        let mut fields: Vec<(u8, String)> = vec![
            (2, format!("seq {}", last.seq)),
            (0, format!("last {}", unit.fmt(last.offset_ns as f64))),
        ];
        if let Some(iv) = last.interval_ns {
            fields.push((1, format!("intvl {:.9} s", iv as f64 / 1e9)));
        }
        if let Some(wd) = last.width_ns {
            fields.push((3, format!("width {:.3} ms", wd as f64 / 1e6)));
        }
        let len = |f: &[(u8, String)]| {
            s.chars().count() + f.iter().map(|(_, x)| 2 + x.chars().count()).sum::<usize>()
        };
        // Drop whole fields instead of cutting the line mid-word.
        while len(&fields) > inner {
            let Some(i) = (0..fields.len())
                .filter(|&i| fields[i].0 > 0)
                .max_by_key(|&i| fields[i].0)
            else {
                break;
            };
            fields.remove(i);
        }
        for (_, f) in &fields {
            let _ = write!(s, "  {f}");
        }
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
        let _ = write!(counts, "  re-baselined {}x", t.stepped);
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

/// Half-range of the sparkline: robust against outliers (median + 4 robust
/// sigma from the MAD), so one transient does not flatten the rest.
pub fn spark_range(vals: &[i64]) -> i64 {
    if vals.is_empty() {
        return 1;
    }
    let mut v = vals.to_vec();
    v.sort_unstable();
    let med = v[v.len() / 2];
    let mut dev: Vec<i64> = v.iter().map(|x| (x - med).abs()).collect();
    dev.sort_unstable();
    let mad = dev[dev.len() / 2];
    let r = if mad > 0 {
        med.abs() + (4.0 * 1.4826 * mad as f64).ceil() as i64
    } else {
        // all (but a few) identical: fall back to the largest magnitude
        v.iter().map(|x| x.abs()).max().unwrap_or(0)
    };
    r.max(1)
}

/// Per-second offsets as bars growing up (positive) or down (negative) from a
/// zero line in the middle. Values beyond the robust range are clipped and
/// drawn in yellow; seconds without a pulse show a red `!` on the zero line.
fn sparkline(c: &mut Canvas, t: &Track, unit: Unit, x: usize, y: usize, w: usize, rows: usize) {
    const LABEL: usize = 11;
    let rows = rows & !1; // need an even number: half up, half down
    if w <= LABEL + 4 || rows < 2 {
        return;
    }
    let half = rows / 2;
    let levels = (half * 8) as f64;
    let cols = w - LABEL - 1;
    let Some(last) = t.hist.back() else { return };
    let first_sec = last.sec - cols as i64 + 1;
    let oldest = t.hist.front().map_or(last.sec, |s| s.sec);
    let mut series: Vec<Option<i64>> = vec![None; cols];
    for s in t.hist.iter().filter(|s| s.sec >= first_sec) {
        series[(s.sec - first_sec) as usize] = Some(s.offset_ns);
    }
    let vals: Vec<i64> = series.iter().flatten().copied().collect();
    let range = spark_range(&vals);

    // Bottom-aligned eighths; a top-aligned k/8 cell is drawn as the
    // (8-k)/8 bottom block in reverse video.
    let blocks = [
        ' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    let ascii = [' ', '.', '.', ':', ':', '|', '|', '#', '#'];

    for r in 0..rows {
        let label = if r == 0 {
            unit.num(range as f64)
        } else if r == rows - 1 {
            unit.num(-(range as f64))
        } else if r == half - 1 {
            "0 ".to_string()
        } else {
            String::new()
        };
        c.put(x, y + r, &format!("{label:>LABEL$}"), st::DIM);
    }
    for (i, v) in series.iter().enumerate() {
        let sec = first_sec + i as i64;
        let cx = x + LABEL + 1 + i;
        let Some(v) = *v else {
            if sec >= oldest {
                c.put(cx, y + half - 1, "!", st::RED | st::BOLD);
            }
            continue;
        };
        let clipped = v.abs() > range;
        let lvl = ((v.abs().min(range) as f64 / range as f64) * levels).round() as usize;
        let lvl = lvl.max(1); // always show at least a sliver
        for k in 0..half {
            let fill = lvl.saturating_sub(k * 8).min(8);
            let outer = k == half - 1;
            let style = if clipped && outer {
                st::YELLOW | st::BOLD
            } else {
                st::CYAN
            };
            if v >= 0 {
                // rows half-1 (just above zero) up to 0
                let ch = if c.ascii { ascii[fill] } else { blocks[fill] };
                c.put(cx, y + half - 1 - k, &ch.to_string(), style);
            } else {
                // rows half (just below zero) down to rows-1, filled from the top
                let cy = y + half + k;
                if c.ascii {
                    c.put(cx, cy, &ascii[fill].to_string(), style);
                } else if fill == 8 {
                    c.put(cx, cy, &blocks[8].to_string(), style);
                } else if fill > 0 {
                    c.put(cx, cy, &blocks[8 - fill].to_string(), style | st::REV);
                }
            }
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
    use super::super::hostname;
    use super::*;

    #[test]
    fn spark_range_ignores_transients() {
        // pi4 start-up: a minute of +-300 us excursions, then +-4 us
        let mut v: Vec<i64> = (0..40)
            .map(|i| if i % 2 == 0 { 300_000 } else { -330_000 })
            .collect();
        v.extend((0..80).map(|i| (i % 9 - 4) * 1000));
        let r = spark_range(&v);
        assert!(r > 4_000 && r < 60_000, "{r}");
        assert_eq!(spark_range(&[]), 1);
        assert_eq!(spark_range(&[5, 5, 5]), 5);
        assert_eq!(spark_range(&[0, 0]), 1);
    }

    #[test]
    fn hostname_in_header() {
        let h = hostname();
        assert!(!h.is_empty() && !h.contains('.'));
        let mut app = App::new(vec!["sim:gpio".into()], true);
        app.host = "ghost".into();
        let c = render(&app, Instant::now(), SystemTime::now(), 100, 30);
        assert!(c
            .text()
            .lines()
            .next()
            .unwrap()
            .contains("metronomon @ ghost"));
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

    #[test]
    fn status_line_drops_whole_fields() {
        use super::super::track::pulse;
        let mut t = Track::new("/dev/pps0".into());
        t.name = "serial0".into();
        t.caps = Caps {
            assert: true,
            clear: true,
            can_wait: true,
        };
        t.state = State::Ok;
        let now = Instant::now();
        for i in 0..5 {
            t.feed(
                pulse(1_234_560 + i, 1_800_000_000 + i as i64, 1_200 + i as i32),
                now,
            );
        }
        for w in [40, 60, 80, 88, 120] {
            let mut c = Canvas::new(w, 10, false);
            track_box(&mut c, &t, now, 0, w, 10);
            let text = c.text();
            let mut lines = text.lines();
            let title = lines.next().unwrap();
            let status = lines.next().unwrap();
            assert!(!status.contains("clear"), "{status}");
            assert!(status.contains("last "), "w={w}: {status}");
            if status.contains("intvl") {
                assert!(status.contains(" s"), "w={w}: {status}");
            }
            if w >= 80 {
                assert!(title.contains("(no clear edges)"), "w={w}: {title}");
                assert!(status.contains("intvl 1.000000"), "w={w}: {status}");
            }
        }
    }
}
