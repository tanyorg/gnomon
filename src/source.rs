//! Data sources: TCP (gpsd or raw NMEA, auto-detected) and file/stdin replay.
//! The serial source lives in `serial.rs`.
//! Each source runs on its own thread and reports through an mpsc channel.

use crate::target::{Proto, Target, GPSD_PORT};
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Unknown,
    Gpsd,
    Raw,
    Replay,
    /// Direct serial port. `cdc`: USB CDC-ACM (nominal speed, no wire timing).
    Serial {
        speed: u32,
        cdc: bool,
    },
}

#[derive(Debug)]
pub enum Ev {
    Line(SystemTime, String),
    Status(String),
    Connected(String, Kind),
    Disconnected(String),
    Key(u8),
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const SILENCE_RECONNECT: Duration = Duration::from_secs(15);
pub const MAX_LINE: usize = 8192;

fn watch_cmd(dev: Option<&str>) -> String {
    let mut s = String::from(r#"?WATCH={"enable":true,"json":true,"raw":1,"pps":true"#);
    if let Some(d) = dev {
        s.push_str(r#","device":""#);
        for c in d.chars() {
            if c == '"' || c == '\\' {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
    }
    s.push_str("};\n");
    s
}

struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    fn push(&mut self, data: &[u8], mut out: impl FnMut(String)) {
        for &b in data {
            match b {
                b'\n' => {
                    let mut l = std::mem::take(&mut self.buf);
                    if l.last() == Some(&b'\r') {
                        l.pop();
                    }
                    if !l.is_empty() {
                        out(String::from_utf8_lossy(&l).into_owned());
                    }
                }
                _ => {
                    if self.buf.len() < MAX_LINE {
                        self.buf.push(b);
                    }
                }
            }
        }
    }
}

pub fn spawn_tcp(t: Target, log: Option<File>, tx: Sender<Ev>) {
    thread::spawn(move || {
        let mut log = log;
        let mut backoff = 1u64;
        loop {
            let mut got_data = false;
            let reason = match session(&t, &mut log, &tx, &mut got_data) {
                Ok(r) => r,
                Err(e) => e.to_string(),
            };
            if got_data {
                backoff = 1;
            }
            if tx.send(Ev::Disconnected(reason.clone())).is_err() {
                return;
            }
            for left in (1..=backoff).rev() {
                let _ = tx.send(Ev::Status(format!("{reason}; retry in {left}s")));
                thread::sleep(Duration::from_secs(1));
            }
            backoff = (backoff * 2).min(8);
        }
    });
}

fn connect(t: &Target) -> std::io::Result<TcpStream> {
    let mut last = std::io::Error::new(ErrorKind::NotFound, "no addresses");
    for a in (t.host.as_str(), t.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = std::io::Error::new(e.kind(), format!("{a}: {e}")),
        }
    }
    Err(last)
}

/// One connection lifetime. Returns a human readable reason when it ends.
fn session(
    t: &Target,
    log: &mut Option<File>,
    tx: &Sender<Ev>,
    got_data: &mut bool,
) -> std::io::Result<String> {
    let _ = tx.send(Ev::Status(format!("connecting to {}", t.display())));
    let mut sock = connect(t)?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let _ = sock.set_nodelay(true);
    let peer = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();

    // Protocol decision. We only ever write to the peer once we believe it is
    // gpsd, so a raw serial-over-TCP bridge never receives junk.
    let mut kind = match t.proto {
        Proto::Gpsd => Kind::Gpsd,
        Proto::Raw => Kind::Raw,
        Proto::Auto => Kind::Unknown,
    };
    if kind == Kind::Gpsd {
        sock.write_all(watch_cmd(t.device.as_deref()).as_bytes())?;
    }
    let _ = tx.send(Ev::Connected(peer.clone(), kind));

    let started = Instant::now();
    let mut last_rx = Instant::now();
    let mut split = LineSplitter { buf: Vec::new() };
    let mut buf = [0u8; 4096];
    loop {
        match sock.read(&mut buf) {
            Ok(0) => return Ok("connection closed by peer".into()),
            Ok(n) => {
                last_rx = Instant::now();
                *got_data = true;
                let data = &buf[..n];
                if kind == Kind::Unknown {
                    if let Some(&c) = data.iter().find(|c| !c.is_ascii_whitespace()) {
                        kind = if c == b'{' { Kind::Gpsd } else { Kind::Raw };
                        if kind == Kind::Gpsd {
                            sock.write_all(watch_cmd(t.device.as_deref()).as_bytes())?;
                        }
                        let _ = tx.send(Ev::Connected(peer.clone(), kind));
                    }
                }
                if let Some(f) = log.as_mut() {
                    let _ = f.write_all(data);
                }
                let now = SystemTime::now();
                let mut gone = false;
                split.push(data, |l| gone |= tx.send(Ev::Line(now, l)).is_err());
                if gone {
                    return Ok("ui gone".into());
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                // Silent peer on the default gpsd port: assume gpsd that has
                // not greeted us (should not happen, but be tolerant).
                if kind == Kind::Unknown
                    && t.port == GPSD_PORT
                    && started.elapsed() > Duration::from_secs(2)
                {
                    kind = Kind::Gpsd;
                    sock.write_all(watch_cmd(t.device.as_deref()).as_bytes())?;
                    let _ = tx.send(Ev::Connected(peer.clone(), kind));
                }
                if last_rx.elapsed() > SILENCE_RECONNECT {
                    return Ok(format!("no data for {}s", SILENCE_RECONNECT.as_secs()));
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Replay a capture. A path of "-" reads stdin unpaced (e.g. `nc host 4001 |`);
/// a file is paced at the sentence time-of-day so it looks live.
pub fn spawn_replay(path: String, tx: Sender<Ev>) {
    thread::spawn(move || {
        let reader: Box<dyn BufRead> = if path == "-" {
            Box::new(BufReader::new(std::io::stdin()))
        } else {
            match File::open(&path) {
                Ok(f) => Box::new(BufReader::new(f)),
                Err(e) => {
                    let _ = tx.send(Ev::Disconnected(format!("{path}: {e}")));
                    return;
                }
            }
        };
        let paced = path != "-";
        // stdin is a live stream (e.g. from nc), so treat it like raw TCP.
        let kind = if paced { Kind::Replay } else { Kind::Raw };
        let _ = tx.send(Ev::Connected(path.clone(), kind));
        let mut prev: Option<f64> = None;
        for line in reader.split(b'\n') {
            let Ok(mut l) = line else { break };
            if l.last() == Some(&b'\r') {
                l.pop();
            }
            if l.is_empty() {
                continue;
            }
            let s = String::from_utf8_lossy(&l).into_owned();
            if paced {
                if let Some(t) = epoch_tod(&s) {
                    if let Some(p) = prev {
                        let mut d = t - p;
                        if d < 0.0 {
                            d += 86400.0;
                        }
                        if d > 1e-6 {
                            thread::sleep(Duration::from_secs_f64(d.min(5.0)));
                        }
                    }
                    prev = Some(t);
                }
            }
            if tx.send(Ev::Line(SystemTime::now(), s)).is_err() {
                return;
            }
        }
        let _ = tx.send(Ev::Disconnected("end of input".into()));
    });
}

fn epoch_tod(line: &str) -> Option<f64> {
    let s = crate::nmea::split(line)?;
    let idx = match s.kind {
        "RMC" | "GGA" | "GNS" | "ZDA" => 0,
        "GLL" => 4,
        _ => return None,
    };
    crate::nmea::tod(s.fields.get(idx).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter() {
        let mut sp = LineSplitter { buf: Vec::new() };
        let mut v = Vec::new();
        sp.push(b"$GPA*00\r\n$GP", |l| v.push(l));
        sp.push(b"B*00\r\n\r\n", |l| v.push(l));
        assert_eq!(v, vec!["$GPA*00", "$GPB*00"]);
    }

    #[test]
    fn watch() {
        assert_eq!(
            watch_cmd(Some("/dev/ttyAMA0")),
            "?WATCH={\"enable\":true,\"json\":true,\"raw\":1,\"pps\":true,\"device\":\"/dev/ttyAMA0\"};\n"
        );
    }
}
