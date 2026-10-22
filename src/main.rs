//! gpsmon-rs: NMEA 0183 GNSS monitor modelled on gpsd's gpsmon.

mod app;
mod json;
mod nmea;
mod serial;
mod source;
mod target;
mod term;
mod tty;
mod ui;

use source::Ev;
use std::fs::OpenOptions;
use std::io::{IsTerminal, Read};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const USAGE: &str = "\
usage: gpsmon-rs [options] [target]

target (default localhost:2947):
  host[:port]            connect and auto-detect gpsd JSON or raw NMEA
  host:port:/dev/ttyX    gpsd, watch only that device
  [ipv6]:port            IPv6 literal
  tcp://host:port        raw NMEA over TCP (never writes to the peer)
  gpsd://host[:port]     force gpsd protocol
  /dev/ttyXXX            serial port (speed from -s, else auto-detected)

options:
  -r, --raw              treat the target as raw NMEA (same as tcp://)
  -g, --gpsd             treat the target as gpsd
  -s, --speed BPS        serial speed (default: probe 9600,115200,38400,...)
  -l, --log FILE         append every received byte to FILE
  -f, --replay FILE      play back a capture file ('-' = stdin, unpaced)
  -a, --ascii            ASCII box drawing (for ambiguous-width terminals)
  -j, --json             show gpsd JSON in the raw window at start
      --snapshot SECS    no TUI: run SECS seconds, print one screen, exit
      --size WxH         canvas size for --snapshot (default 100x40)
  -h, --help             this help
  -V, --version          version

keys: q quit, p pause raw window, c clear, j toggle JSON lines";

struct Opts {
    target: Option<String>,
    force: Option<target::Proto>,
    speed: Option<u32>,
    log: Option<String>,
    replay: Option<String>,
    ascii: bool,
    json: bool,
    snapshot: Option<f64>,
    size: (usize, usize),
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        target: None,
        force: None,
        speed: None,
        log: None,
        replay: None,
        ascii: false,
        json: false,
        snapshot: None,
        size: (100, 40),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} needs an argument"));
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("gpsmon-rs {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-r" | "--raw" => o.force = Some(target::Proto::Raw),
            "-g" | "--gpsd" => o.force = Some(target::Proto::Gpsd),
            "-a" | "--ascii" => o.ascii = true,
            "-s" | "--speed" => {
                let v = val("--speed")?;
                let b: u32 = v.parse().map_err(|_| format!("bad speed: {v}"))?;
                if b < 300 {
                    return Err(format!("bad speed: {v}"));
                }
                o.speed = Some(b);
            }
            "-j" | "--json" => o.json = true,
            "-l" | "--log" => o.log = Some(val("--log")?),
            "-f" | "--replay" => o.replay = Some(val("--replay")?),
            "--snapshot" => {
                o.snapshot = Some(
                    val("--snapshot")?
                        .parse()
                        .map_err(|_| "bad --snapshot seconds")?,
                )
            }
            "--size" => {
                let v = val("--size")?;
                let (w, h) = v.split_once(['x', 'X']).ok_or("--size wants WxH")?;
                o.size = (
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                );
            }
            s if s.starts_with('-') && s != "-" => return Err(format!("unknown option {s}")),
            s => {
                if o.target.replace(s.to_string()).is_some() {
                    return Err("only one target may be given".into());
                }
            }
        }
    }
    Ok(o)
}

fn main() {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("gpsmon-rs: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(o) {
        eprintln!("gpsmon-rs: {e}");
        std::process::exit(1);
    }
}

fn run(o: Opts) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<Ev>();

    let label = if let Some(path) = &o.replay {
        if o.target.is_some() {
            return Err("--replay and a target are mutually exclusive".into());
        }
        source::spawn_replay(path.clone(), tx.clone());
        format!("replay:{path}")
    } else {
        let log = match &o.log {
            Some(p) => Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .map_err(|e| format!("{p}: {e}"))?,
            ),
            None => None,
        };
        let spec = o.target.as_deref().unwrap_or("localhost");
        if spec.starts_with('/') {
            if o.force.is_some() {
                return Err("--raw/--gpsd do not apply to a serial device".into());
            }
            serial::spawn(spec.to_string(), o.speed, log, tx.clone());
            spec.to_string()
        } else {
            if o.speed.is_some() {
                return Err("--speed only applies to a serial device".into());
            }
            let mut t = target::parse(spec)?;
            if let Some(p) = o.force {
                if p == target::Proto::Raw && t.device.is_some() {
                    return Err("--raw conflicts with a gpsd device path".into());
                }
                t.proto = p;
            }
            let label = t.display();
            source::spawn_tcp(t, log, tx.clone());
            label
        }
    };

    let mut app = app::App::new(label, o.ascii);
    app.show_json = o.json;

    if let Some(secs) = o.snapshot {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Ok(ev) = rx.recv_timeout(left) {
                handle(&mut app, ev);
            }
        }
        let c = ui::render(&app, Instant::now(), SystemTime::now(), o.size.0, o.size.1);
        print!("{}", c.text());
        return Ok(());
    }

    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        return Err(
            "stdin/stdout must be a terminal (use --snapshot for non-interactive use)".into(),
        );
    }
    if o.replay.as_deref() == Some("-") {
        return Err("--replay - needs stdin, which is also the keyboard; use --snapshot".into());
    }

    term::enter().map_err(|e| e.to_string())?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::leave();
        hook(info);
    }));

    // Keyboard reader. stty raw makes read() return per keystroke.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 16];
            let mut stdin = std::io::stdin();
            loop {
                match stdin.read(&mut b) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        for &k in &b[..n] {
                            if tx.send(Ev::Key(k)).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
    }

    let result = ui_loop(&mut app, &rx);
    term::leave();
    result
}

/// Returns false when the user asked to quit.
fn handle(app: &mut app::App, ev: Ev) -> bool {
    match ev {
        Ev::Line(ts, l) => app.line(ts, l, Instant::now()),
        Ev::Status(s) => app.status = s,
        Ev::Connected(peer, kind) => {
            app.connected = true;
            app.kind = kind;
            app.status = format!("connected {peer}");
        }
        Ev::Disconnected(why) => {
            if app.connected {
                app.reconnects += 1;
            }
            app.connected = false;
            app.status = why;
        }
        Ev::Key(k) => match k {
            b'q' | b'Q' | 3 | 4 => return false, // q, ^C, ^D
            b'p' | b'P' | b' ' => app.toggle_pause(),
            b'c' | b'C' => app.clear(),
            b'j' | b'J' => app.show_json = !app.show_json,
            _ => {}
        },
    }
    true
}

fn ui_loop(app: &mut app::App, rx: &mpsc::Receiver<Ev>) -> Result<(), String> {
    let mut screen = term::Screen::default();
    let frame = Duration::from_millis(100);
    let mut last_draw = Instant::now() - frame;
    loop {
        let wait = frame
            .saturating_sub(last_draw.elapsed())
            .max(Duration::from_millis(10));
        match rx.recv_timeout(wait) {
            Ok(ev) => {
                if !handle(app, ev) {
                    return Ok(());
                }
                // Drain whatever else is queued before drawing.
                while let Ok(ev) = rx.try_recv() {
                    if !handle(app, ev) {
                        return Ok(());
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if last_draw.elapsed() >= frame {
            let (w, h) = term::size();
            let c = ui::render(app, Instant::now(), SystemTime::now(), w, h);
            screen.draw(&c).map_err(|e| e.to_string())?;
            last_draw = Instant::now();
        }
    }
}
