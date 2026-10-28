//! metronomon: PPS (pulse-per-second) monitor for the Linux PPS API.

use gnomon_gnss::metro::{self, App};
use gnomon_gnss::pps::{self, Ev, Spec};
use gnomon_gnss::term;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

const USAGE: &str = "\
usage: metronomon [options] [source ...]

source (default /dev/pps0):
  /dev/ppsN              Linux PPS API device (RFC 2783); read-only and
                         non-destructive, safe to run next to chronyd/ntpd
  sim[:PROFILE]          simulated source: gpio (default), uart, usb, bad

options:
  -a, --ascii            ASCII drawing (for ambiguous-width terminals)
      --snapshot SECS    no TUI: run SECS seconds, print one screen, exit
      --size WxH         canvas size for --snapshot (default 100x40)
  -h, --help             this help
  -V, --version          version

keys: q quit, c clear statistics";

struct Opts {
    sources: Vec<String>,
    ascii: bool,
    snapshot: Option<Duration>,
    size: (usize, usize),
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        sources: Vec::new(),
        ascii: false,
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
                println!("metronomon {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-a" | "--ascii" => o.ascii = true,
            "--snapshot" => o.snapshot = Some(term::parse_snapshot(&val("--snapshot")?)?),
            "--size" => o.size = term::parse_size(&val("--size")?, (metro::MIN_W, metro::MIN_H))?,
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => o.sources.push(s.to_string()),
        }
    }
    if o.sources.is_empty() {
        o.sources.push("/dev/pps0".into());
    }
    if o.sources.len() > 8 {
        return Err("at most 8 sources".into());
    }
    Ok(o)
}

fn main() {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("metronomon: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(o) {
        eprintln!("metronomon: {e}");
        std::process::exit(1);
    }
}

fn run(o: Opts) -> Result<(), String> {
    let specs: Vec<Spec> = o
        .sources
        .iter()
        .map(|s| Spec::parse(s))
        .collect::<Result<_, _>>()?;
    let (tx, rx) = mpsc::channel::<Ev>();
    let mut app = App::new(specs.iter().map(Spec::label).collect(), o.ascii);
    for (i, s) in specs.into_iter().enumerate() {
        pps::spawn(i, s, tx.clone());
    }

    if let Some(d) = o.snapshot {
        term::drain_for(&rx, d, |ev| {
            app.handle(ev, Instant::now());
        });
        let c = metro::render(&app, Instant::now(), SystemTime::now(), o.size.0, o.size.1);
        print!("{}", c.text());
        return Ok(());
    }

    // `app` is used by both closures; the loop calls them one at a time.
    let app = std::cell::RefCell::new(app);
    term::run_tui(
        &tx,
        &rx,
        Duration::from_millis(200),
        Ev::Key,
        |ev| {
            let mut app = app.borrow_mut();
            match ev {
                Ev::Key(b'q' | b'Q' | 3 | 4) => return false, // q, ^C, ^D
                Ev::Key(b'c' | b'C') => app.clear(),
                ev => app.handle(ev, Instant::now()),
            }
            true
        },
        |w, h| metro::render(&app.borrow(), Instant::now(), SystemTime::now(), w, h),
    )
}
