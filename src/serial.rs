//! Direct serial-port source (NMEA 0183 from a receiver on a tty).
//!
//! Line discipline is set through termios directly (see `tty.rs`), with
//! CLOCAL so the port works without carrier detect, the equivalent of
//! kermit's SET CARRIER-WATCH OFF.

use crate::nmea::{split, CkStatus};
use crate::source::{Ev, Kind, MAX_LINE};
use crate::tty;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Probe order when no speed is given: common GNSS defaults first.
pub const AUTOBAUD: [u32; 7] = [9600, 115200, 38400, 4800, 19200, 57600, 230400];
const PROBE_TIME: Duration = Duration::from_millis(2500);
const SILENCE_RECONNECT: Duration = Duration::from_secs(15);

#[cfg(any(target_os = "linux", target_os = "android"))]
mod flags {
    pub const O_NONBLOCK: i32 = 0o4000;
    pub const O_NOCTTY: i32 = 0o400;
}
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod flags {
    pub const O_NONBLOCK: i32 = 0x0004;
    pub const O_NOCTTY: i32 = 0x20000;
}
#[cfg(any(
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
mod flags {
    pub const O_NONBLOCK: i32 = 0x0004;
    pub const O_NOCTTY: i32 = 0x8000;
}

const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;

extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
}

/// Open with O_NONBLOCK so open(2) itself never waits for carrier detect
/// (macOS /dev/tty.* and Linux without CLOCAL would). Blocking mode is only
/// restored after CLOCAL has been set and verified.
fn open_dev(path: &str) -> io::Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(flags::O_NONBLOCK | flags::O_NOCTTY)
        .open(path)?;
    Ok(f)
}

fn set_blocking(f: &File) -> io::Result<()> {
    let fd = f.as_raw_fd();
    // SAFETY: plain fcntl on a descriptor we own.
    unsafe {
        let fl = fcntl(fd, F_GETFL);
        if fl < 0 || fcntl(fd, F_SETFL, fl & !flags::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn configure(f: &File, speed: u32) -> io::Result<()> {
    tty::configure(f.as_raw_fd(), speed)?;
    // Bytes received at the previous speed are garbage now.
    tty::flush_input(f.as_raw_fd());
    Ok(())
}

/// Splits serial input into lines and estimates when each line *started*
/// on the wire: the chunk timestamp is when its last byte was available, so
/// every byte after a given point took `bits_per_char / speed` seconds.
struct Framer {
    buf: Vec<u8>,
    char_time: f64,
    last_start: Option<SystemTime>,
}

impl Framer {
    /// `wire_timed`: the bytes really crossed a UART at `speed`. False for
    /// USB CDC-ACM devices, whose nominal speed is fictitious and whose data
    /// arrives in USB transfers, so back-dating would only add error.
    fn new(speed: u32, wire_timed: bool) -> Self {
        Framer {
            buf: Vec::new(),
            char_time: if wire_timed { 10.0 / speed as f64 } else { 0.0 },
            last_start: None,
        }
    }

    /// Returns (estimated line start time, line text).
    fn push(&mut self, data: &[u8], arrived: SystemTime) -> Vec<(SystemTime, String)> {
        let mut out = Vec::new();
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                let mut l = std::mem::take(&mut self.buf);
                let wire_len = l.len() + 1; // including '\n'
                if l.last() == Some(&b'\r') {
                    l.pop();
                }
                if l.is_empty() {
                    continue;
                }
                let after = data.len() - 1 - i; // bytes received after this '\n'
                let back = (after + wire_len) as f64 * self.char_time;
                let mut start = arrived
                    .checked_sub(Duration::from_secs_f64(back))
                    .unwrap_or(arrived);
                // Never step backwards: lines were sent in order.
                if let Some(prev) = self.last_start {
                    if start < prev {
                        start = prev;
                    }
                }
                self.last_start = Some(start);
                out.push((start, String::from_utf8_lossy(&l).into_owned()));
            } else if self.buf.len() < MAX_LINE {
                self.buf.push(b);
            } else {
                self.buf.clear(); // runaway garbage (wrong speed): drop it
            }
        }
        out
    }
}

/// USB CDC-ACM devices (u-blox and other native-USB receivers):
/// macOS `cu.usbmodem*` / `tty.usbmodem*`, Linux `ttyACM*`.
pub fn is_usb_cdc(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.contains("usbmodem") || name.starts_with("ttyACM")
}

fn good_nmea(l: &str) -> bool {
    matches!(split(l), Some(s) if s.ck == CkStatus::Ok)
}

/// Try one speed: true when at least two checksum-valid sentences arrived
/// within the probe window.
fn probe(f: &mut File, speed: u32) -> io::Result<bool> {
    configure(f, speed)?;
    let mut fr = Framer::new(speed, false);
    let mut good = 0;
    let end = Instant::now() + PROBE_TIME;
    let mut buf = [0u8; 2048];
    while Instant::now() < end {
        let n = match f.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            continue;
        }
        for (_, l) in fr.push(&buf[..n], SystemTime::now()) {
            if good_nmea(&l) {
                good += 1;
            }
        }
        if good >= 2 {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn spawn(path: String, speed: Option<u32>, log: Option<File>, tx: Sender<Ev>) {
    thread::spawn(move || {
        let mut log = log;
        let mut last_good = speed;
        let mut backoff = 1u64;
        loop {
            let mut got_data = false;
            let reason = match session(&path, speed, &mut last_good, &mut log, &tx, &mut got_data) {
                Ok(r) => r,
                Err(e) => format!("{path}: {e}"),
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

fn session(
    path: &str,
    fixed: Option<u32>,
    last_good: &mut Option<u32>,
    log: &mut Option<File>,
    tx: &Sender<Ev>,
    got_data: &mut bool,
) -> io::Result<String> {
    let _ = tx.send(Ev::Status(format!("opening {path}")));
    let mut f = open_dev(path)?;
    tty::set_exclusive(f.as_raw_fd(), true);
    // CLOCAL first, then blocking reads.
    configure(&f, fixed.or(*last_good).unwrap_or(AUTOBAUD[0]))?;
    set_blocking(&f)?;

    let speed = match fixed {
        Some(s) => {
            configure(&f, s)?;
            s
        }
        None => {
            // Last known good speed first, then the usual list.
            let mut order: Vec<u32> = last_good.iter().copied().collect();
            order.extend(AUTOBAUD.iter().copied().filter(|s| Some(*s) != *last_good));
            let mut found = None;
            for s in order {
                let _ = tx.send(Ev::Status(format!("{path}: probing {s} bps")));
                // Probe lines are dropped: they were buffered while probing,
                // so their arrival times are useless for timing statistics.
                if probe(&mut f, s)? {
                    found = Some(s);
                    break;
                }
            }
            match found {
                Some(s) => s,
                None => return Ok("no valid NMEA at any speed".into()),
            }
        }
    };
    *last_good = Some(speed);
    let dcd = match tty::carrier(f.as_raw_fd()) {
        Some(true) => ", DCD on",
        Some(false) => ", DCD off (ignored)",
        None => "",
    };
    let cdc = is_usb_cdc(path);
    let desc = if cdc {
        format!("{path} USB CDC{dcd}")
    } else {
        format!("{path} {speed} 8N1{dcd}")
    };
    let _ = tx.send(Ev::Connected(desc, Kind::Serial { speed, cdc }));
    *got_data = true;

    let mut fr = Framer::new(speed, !cdc);
    let mut last_rx = Instant::now();
    let mut buf = [0u8; 4096];
    loop {
        match f.read(&mut buf) {
            // VMIN=0/VTIME timeout: nothing arrived.
            Ok(0) => {
                if last_rx.elapsed() > SILENCE_RECONNECT {
                    return Ok(format!("no data for {}s", SILENCE_RECONNECT.as_secs()));
                }
            }
            Ok(n) => {
                let now = SystemTime::now();
                last_rx = Instant::now();
                *got_data = true;
                if let Some(lf) = log.as_mut() {
                    let _ = lf.write_all(&buf[..n]);
                }
                for (t, l) in fr.push(&buf[..n], now) {
                    if tx.send(Ev::Line(t, l)).is_err() {
                        return Ok("ui gone".into());
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(50)),
            // Typically EIO/ENXIO when a USB adapter is unplugged.
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framer_back_dates_lines() {
        let mut fr = Framer::new(9600, true); // 1.0416 ms per char
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        // "$A*00\r\n" (7 bytes) followed by "$BB*00\r\n" (8 bytes) in one chunk
        let v = fr.push(b"$A*00\r\n$BB*00\r\n", t);
        assert_eq!(v.len(), 2);
        let ms = |s: SystemTime| t.duration_since(s).unwrap().as_secs_f64() * 1e3;
        assert!(
            (ms(v[0].0) - 15.0 * 10.0 / 9.6).abs() < 1e-6,
            "{}",
            ms(v[0].0)
        );
        assert!((ms(v[1].0) - 8.0 * 10.0 / 9.6).abs() < 1e-6);
        assert_eq!(v[1].1, "$BB*00");
        // partial line carried over to the next chunk
        assert!(fr.push(b"$CC", t).is_empty());
        let v = fr.push(b"*00\r\n", t);
        assert_eq!(v[0].1, "$CC*00");
    }

    #[test]
    fn framer_monotonic_and_cdc() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        // UART timing: a later chunk must not produce an earlier start.
        let mut fr = Framer::new(9600, true);
        let a = fr.push(b"$A*00\r\n", t);
        let b = fr.push(
            b"$BBBBBBBBBBBBBBBBBBBBBBBB*00\r\n",
            t + Duration::from_millis(1),
        );
        assert!(b[0].0 >= a[0].0);
        // CDC: timestamps are taken as-is.
        let mut fr = Framer::new(9600, false);
        let v = fr.push(b"$A*00\r\n$B*00\r\n", t);
        assert!(v.iter().all(|(s, _)| *s == t));
        assert!(is_usb_cdc("/dev/cu.usbmodem1101"));
        assert!(is_usb_cdc("/dev/ttyACM0"));
        assert!(!is_usb_cdc("/dev/cu.usbserial-110"));
        assert!(!is_usb_cdc("/dev/ttyUSB0"));
    }
}
