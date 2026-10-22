//! Screen layout, loosely following gpsmon's NMEA monitor.
//!
//!  header ----------------------------------------------------------------
//!  [Satellites]        [Fix]                       [Sentences]
//!  [gpsd: version / device / TPV / PPS / TOFF]     (gpsd sources only)
//!  [Raw data .....................................................]
//!  footer: keys and counters

use crate::app::{fmt_tod, fmt_utc, App, Offset};
use crate::nmea::Sys;
use crate::source::Kind;
use crate::term::{st, Canvas};
use std::time::{Duration, Instant, SystemTime};

pub const MIN_W: usize = 80;
pub const MIN_H: usize = 24;
const STALE: Duration = Duration::from_secs(5);

fn quality_name(q: u32) -> &'static str {
    match q {
        0 => "invalid",
        1 => "GPS",
        2 => "DGPS",
        3 => "PPS",
        4 => "RTK fix",
        5 => "RTK flt",
        6 => "DR",
        7 => "manual",
        8 => "sim",
        _ => "?",
    }
}

fn posmode_name(m: &str) -> &'static str {
    match m.chars().next() {
        Some('A') => "auto",
        Some('D') => "diff",
        Some('E') => "DR",
        Some('F') => "RTKflt",
        Some('M') => "manual",
        Some('N') => "nofix",
        Some('P') => "precise",
        Some('R') => "RTKfix",
        Some('S') => "sim",
        _ => "?",
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}

fn fmt_offset(o: &Offset, unit_ns: f64, unit: &str, dec: usize) -> String {
    format!("{:+.dec$} {unit}", o.ns as f64 / unit_ns, dec = dec)
}

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

    header(&mut c, app, wall);

    let gpsd_h = if app.kind == Kind::Gpsd { 5 } else { 0 };
    let sats = app.nmea.satellites(now);
    let avail = h - 2 - gpsd_h - 5; // header, footer, minimum raw box
    let want = (sats.len() + 3).max(17);
    let top_h = want.min(avail).max(10);

    let sat_w = if w >= 100 { 34 } else { 30 };
    let snt_w = 23;
    let mid_w = w - sat_w - snt_w;
    satellites(&mut c, &sats, 0, 1, sat_w, top_h);
    fix(&mut c, app, sat_w, 1, mid_w, top_h);
    sentences(&mut c, app, now, sat_w + mid_w, 1, snt_w, top_h);

    let mut y = 1 + top_h;
    if gpsd_h > 0 {
        gpsd_box(&mut c, app, now, y, w, gpsd_h);
        y += gpsd_h;
    }
    raw_box(&mut c, app, y, w, h - 1 - y);
    footer(&mut c, app, h - 1);
    c
}

fn header(c: &mut Canvas, app: &App, wall: SystemTime) {
    c.fill(0, 0, c.w, st::REV);
    let kind = match app.kind {
        Kind::Unknown => "detecting".to_string(),
        Kind::Gpsd => "gpsd".to_string(),
        Kind::Raw => "raw NMEA/TCP".to_string(),
        Kind::Replay => "replay".to_string(),
        Kind::Serial { cdc: true, .. } => "serial USB CDC".to_string(),
        Kind::Serial { speed, .. } => format!("serial {speed} 8N1"),
    };
    let state = if app.connected { "up" } else { "down" };
    let left = format!(" gpsmon-rs  {}  [{} {}]", app.target, kind, state);
    c.put(0, 0, &left, st::REV | st::BOLD);
    let right = format!("host {} ", fmt_utc(wall));
    if c.w > left.len() + right.len() + 1 {
        c.put(c.w - right.len(), 0, &right, st::REV);
    }
}

fn satellites(
    c: &mut Canvas,
    sats: &[crate::nmea::SatView],
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) {
    let used = sats.iter().filter(|s| s.used).count();
    c.boxed(x, y, w, h, &format!("Satellites {used}/{}", sats.len()));
    let snr_w = w - 2 - 19;
    c.put(
        x + 1,
        y + 1,
        &format!("Sy PRN  El  Az {:>snr_w$} U", "SNR", snr_w = snr_w - 2),
        st::BOLD,
    );
    let rows = h - 3;
    let overflow = sats.len() > rows;
    let shown = if overflow { rows - 1 } else { sats.len() };
    for (i, s) in sats.iter().take(shown).enumerate() {
        let snr: Vec<String> = s.snr.values().map(|v| opt(*v)).collect();
        let mut snr = snr.join("/");
        if snr.is_empty() {
            snr = "-".into();
        }
        if snr.len() > snr_w - 2 {
            snr.truncate(snr_w - 2);
        }
        let line = format!(
            "{} {:>3} {:>3} {:>3} {:>sw$} {}",
            s.sys.label(),
            s.prn,
            opt(s.el),
            opt(s.az),
            snr,
            if s.used { "Y" } else { " " },
            sw = snr_w - 2
        );
        let style = if s.used {
            st::GREEN | st::BOLD
        } else if s.snr.values().all(|v| v.is_none()) {
            st::DIM
        } else if s.sys == Sys::Sbas {
            st::CYAN
        } else {
            0
        };
        c.put(x + 1, y + 2 + i, &line, style);
    }
    if overflow {
        c.put(
            x + 1,
            y + 2 + shown,
            &format!("... +{} more", sats.len() - shown),
            st::DIM,
        );
    }
}

fn fix(c: &mut Canvas, app: &App, x: usize, y: usize, w: usize, h: usize) {
    c.boxed(x, y, w, h, "Fix");
    let f = &app.nmea.fix;
    let mut lines: Vec<(String, u8)> = Vec::new();
    let mut push = |s: String, style: u8| lines.push((s, style));

    push(
        format!(
            "Time {}",
            f.tod
                .map(|t| format!("{}Z", fmt_tod(t, 3)))
                .unwrap_or("-".into())
        ),
        st::BOLD,
    );
    push(
        format!(
            "Date {}",
            f.date
                .map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
                .unwrap_or("-".into())
        ),
        0,
    );
    let ll = |v: Option<f64>, p: char, n: char, wdt: usize| {
        v.map(|v| {
            format!(
                "{:>wdt$.7} {}",
                v.abs(),
                if v >= 0.0 { p } else { n },
                wdt = wdt
            )
        })
        .unwrap_or("-".into())
    };
    push(format!("Lat  {}", ll(f.lat, 'N', 'S', 12)), 0);
    push(format!("Lon  {}", ll(f.lon, 'E', 'W', 12)), 0);
    push(
        format!(
            "Alt  {} m  Sep {}",
            f.alt.map(|a| format!("{a:.1}")).unwrap_or("-".into()),
            f.sep.map(|a| format!("{a:.1}")).unwrap_or("-".into())
        ),
        0,
    );
    let nav = match f.nav_mode {
        Some(1) => "no fix",
        Some(2) => "2D",
        Some(3) => "3D",
        _ => "-",
    };
    let rmc = match f.rmc_status {
        Some('A') => "A(valid)",
        Some('V') => "V(warn)",
        Some(_) => "?",
        None => "-",
    };
    let fixstyle = if f.rmc_status == Some('A') && f.nav_mode == Some(3) {
        st::GREEN
    } else if f.rmc_status == Some('V') || f.nav_mode == Some(1) {
        st::RED
    } else {
        st::YELLOW
    };
    push(format!("Fix  {nav}  RMC {rmc}"), fixstyle);
    push(
        format!(
            "Qual {}  Mode {}",
            f.quality
                .map(|q| format!("{q} {}", quality_name(q)))
                .unwrap_or("-".into()),
            f.pos_mode.as_deref().map(posmode_name).unwrap_or("-")
        ),
        0,
    );
    push(
        format!(
            "DOP  P{} H{} V{}",
            f.pdop.map(|v| format!("{v:.2}")).unwrap_or("-".into()),
            f.hdop.map(|v| format!("{v:.2}")).unwrap_or("-".into()),
            f.vdop.map(|v| format!("{v:.2}")).unwrap_or("-".into())
        ),
        0,
    );
    push(format!("Used {} (GGA)", opt(f.sats_used)), 0);
    let lag_style = match app.nmea.lag {
        Some(l) if l.abs() > 1.0 => st::RED,
        _ => 0,
    };
    if app.kind == Kind::Replay {
        push("Lag  n/a (replay)".into(), st::DIM);
    } else {
        push(
            format!(
                "Lag  {} ({})",
                app.nmea
                    .lag
                    .map(|l| format!("{:+.1} ms", l * 1e3))
                    .unwrap_or("-".into()),
                if matches!(app.kind, Kind::Serial { cdc: false, .. }) {
                    "tx start"
                } else {
                    "host-fix"
                }
            ),
            lag_style,
        );
    }
    push(
        format!(
            "Spd  {} kn  Trk {}",
            f.speed_kn.map(|v| format!("{v:.3}")).unwrap_or("-".into()),
            f.track.map(|v| format!("{v:.1}")).unwrap_or("-".into())
        ),
        0,
    );
    if f.gst_lat.is_some() || f.gst_alt.is_some() {
        push(
            format!(
                "GST  {}/{}/{} m",
                f.gst_lat.map(|v| format!("{v:.1}")).unwrap_or("-".into()),
                f.gst_lon.map(|v| format!("{v:.1}")).unwrap_or("-".into()),
                f.gst_alt.map(|v| format!("{v:.1}")).unwrap_or("-".into())
            ),
            0,
        );
    }
    if let Some(m) = f.magvar {
        push(format!("MagV {m:+.1}"), 0);
    }
    if f.diff_age.is_some() || f.diff_station.is_some() {
        push(
            format!(
                "Diff age {} stn {}",
                opt(f.diff_age),
                opt(f.diff_station.clone())
            ),
            0,
        );
    }
    if let Some(t) = &f.last_txt {
        push(format!("TXT  {t}"), st::DIM);
    }
    for (i, (l, s)) in lines.iter().take(h - 2).enumerate() {
        let l: String = l.chars().take(w - 2).collect();
        c.put(x + 1, y + 1 + i, &l, *s);
    }
}

fn sentences(c: &mut Canvas, app: &App, now: Instant, x: usize, y: usize, w: usize, h: usize) {
    c.boxed(x, y, w, h, "Sentences");
    c.put(x + 1, y + 1, "Type      Count Intvl", st::BOLD);
    let rows = h - 3;
    let n = app.nmea.stats.len();
    for (i, (addr, s)) in app.nmea.stats.iter().enumerate() {
        if i >= rows {
            break;
        }
        if i == rows - 1 && n > rows {
            c.put(x + 1, y + 2 + i, &format!("... +{} more", n - i), st::DIM);
            break;
        }
        // Show the (stable) output interval; switch to elapsed time only
        // when the sentence has gone missing, so the column does not churn.
        let age = s.last.map(|t| now.duration_since(t));
        // Only periodic sentences can go missing; one-shot ones (start-up
        // GPTXT banners etc.) never established a period.
        let missing = s.period.is_some() && age.map_or(true, |a| a > s.stale_after());
        let col = match (missing, age, s.period) {
            (true, Some(a), _) if a.as_secs() > 99 => ">99s!".to_string(),
            (true, Some(a), _) => format!("{}s!", a.as_secs()),
            (false, _, Some(p)) if p < 0.095 => format!("{p:.2}s"),
            (false, _, Some(p)) => format!("{p:.1}s"),
            _ => "-".to_string(),
        };
        let a: String = addr.chars().take(7).collect();
        let line = format!("{:<7}{:>8} {:>5}", a, s.count, col);
        let style = if s.bad > 0 {
            st::RED
        } else if missing && age.is_some() {
            st::YELLOW
        } else {
            0
        };
        c.put(x + 1, y + 2 + i, &line, style);
    }
}

fn gpsd_box(c: &mut Canvas, app: &App, now: Instant, y: usize, w: usize, h: usize) {
    let g = &app.gpsd;
    let title = format!(
        "gpsd {} proto {}",
        g.release.as_deref().unwrap_or("?"),
        g.proto.as_deref().unwrap_or("?")
    );
    c.boxed(0, y, w, h, &title);
    let dev = match g.devices.first() {
        Some(d) => {
            let more = if g.devices.len() > 1 {
                format!("  (+{} more)", g.devices.len() - 1)
            } else {
                String::new()
            };
            format!(
                "Dev  {}  {}  {}{}",
                d.path,
                if d.driver.is_empty() { "-" } else { &d.driver },
                d.bps.map(|b| format!("{b} bps")).unwrap_or_default(),
                more
            )
        }
        None => "Dev  (none reported)".into(),
    };
    c.put(1, y + 1, &dev, 0);
    if let Some(e) = &g.error {
        let x = (dev.len() + 4).max(w / 2);
        c.put(x, y + 1, &format!("ERROR {e}"), st::RED);
    }
    let mode = match g.tpv_mode {
        Some(0) => "unknown",
        Some(1) => "no fix",
        Some(2) => "2D",
        Some(3) => "3D",
        _ => "-",
    };
    let tpv_style = if g.tpv_at.map_or(true, |t| now.duration_since(t) > STALE) {
        st::DIM
    } else {
        0
    };
    c.put(
        1,
        y + 2,
        &format!(
            "TPV  {}  mode {}  ept {}",
            g.tpv_time.as_deref().unwrap_or("-"),
            mode,
            g.tpv_ept.map(|e| format!("{e:.3} s")).unwrap_or("-".into())
        ),
        tpv_style,
    );
    let age = |o: &Offset| format!("{:.1}s ago", now.duration_since(o.at).as_secs_f64());
    let pps = match &g.pps {
        Some(o) => format!(
            "PPS  real-clock {}  prec {}  {}",
            fmt_offset(o, 1e3, "us", 3),
            opt(o.precision),
            age(o)
        ),
        None => "PPS  -".into(),
    };
    let pps_style = match &g.pps {
        Some(o) if now.duration_since(o.at) <= Duration::from_secs(3) => st::GREEN,
        Some(_) => st::DIM,
        None => st::DIM,
    };
    let end = c.put(1, y + 3, &pps, pps_style);
    let toff = match &g.toff {
        Some(o) => format!(
            "TOFF real-clock {}  {}",
            fmt_offset(o, 1e6, "ms", 3),
            age(o)
        ),
        None => "TOFF -".into(),
    };
    c.put((end + 4).max(w / 2), y + 3, &toff, 0);
}

fn raw_box(c: &mut Canvas, app: &App, y: usize, w: usize, h: usize) {
    let paused = app.frozen.is_some();
    let title = format!(
        "Raw data{}{}",
        if app.show_json { " +json" } else { "" },
        if paused { "  [PAUSED]" } else { "" }
    );
    c.boxed(0, y, w, h, &title);
    let rows = h - 2;
    let src: Vec<&crate::app::RawLine> = match &app.frozen {
        Some(v) => v.iter().filter(|l| app.show_json || !l.json).collect(),
        None => app
            .raw
            .iter()
            .filter(|l| app.show_json || !l.json)
            .collect(),
    };
    let start = src.len().saturating_sub(rows);
    for (i, l) in src[start..].iter().enumerate() {
        let x = c.put(1, y + 1 + i, &l.ts, st::DIM);
        let text: String = l.text.chars().take(w - 2 - (x - 1) - 1).collect();
        c.put(x + 1, y + 1 + i, &text, l.style);
    }
}

fn footer(c: &mut Canvas, app: &App, y: usize) {
    c.fill(0, y, c.w, st::REV);
    let keys = format!(
        " q:quit p:pause c:clear j:json[{}] | rx {} bad-ck {} no-ck {} reconn {} | ",
        if app.show_json { "on" } else { "off" },
        app.rx_lines,
        app.nmea.bad_ck,
        app.nmea.no_ck,
        app.reconnects
    );
    let x = c.put(0, y, &keys, st::REV);
    let style = if app.connected {
        st::REV
    } else {
        st::REV | st::BOLD
    };
    c.put(x, y, &app.status, style);
}
