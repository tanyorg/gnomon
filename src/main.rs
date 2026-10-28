//! gnomon: NMEA 0183 GNSS monitor modelled on gpsd's gpsmon.

use gnomon_gnss::source::Ev;
use gnomon_gnss::{app, serial, source, target, term, ui};
use std::fs::OpenOptions;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const USAGE: &str = "\
usage: gnomon [options] [target]

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
                         (0 < SECS <= 86400)
      --size WxH         canvas size for --snapshot (default 100x40,
                         80x24 to 1000x1000)
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
    snapshot: Option<Duration>,
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
                println!("gnomon {}", env!("CARGO_PKG_VERSION"));
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
            "--snapshot" => o.snapshot = Some(term::parse_snapshot(&val("--snapshot")?)?),
            "--size" => o.size = term::parse_size(&val("--size")?, (ui::MIN_W, ui::MIN_H))?,
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
            eprintln!("gnomon: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(o) {
        eprintln!("gnomon: {e}");
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

    if let Some(d) = o.snapshot {
        term::drain_for(&rx, d, |ev| {
            handle(&mut app, ev);
        });
        let c = ui::render(&app, Instant::now(), SystemTime::now(), o.size.0, o.size.1);
        print!("{}", c.text());
        return Ok(());
    }

    term::require_tty()?;
    if o.replay.as_deref() == Some("-") {
        return Err("--replay - needs stdin, which is also the keyboard; use --snapshot".into());
    }
    // `app` is used by both closures; the loop calls them one at a time.
    let app = std::cell::RefCell::new(app);
    term::run_tui(
        &tx,
        &rx,
        Duration::from_millis(100),
        Ev::Key,
        |ev| handle(&mut app.borrow_mut(), ev),
        |w, h| ui::render(&app.borrow(), Instant::now(), SystemTime::now(), w, h),
    )
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
