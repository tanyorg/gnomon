//! PPS (pulse-per-second) sources.
//!
//! * Real devices through the Linux PPS API (RFC 2783 as implemented by
//!   `linux/pps.h`): `PPS_GETCAP` + `PPS_FETCH` on `/dev/ppsN`. Fetching is
//!   non-destructive, so this runs happily next to chronyd or ntpd using the
//!   same device.
//! * Simulated sources (`sim`, `sim:gpio`, `sim:uart`, `sim:usb`, `sim:bad`)
//!   for development, demos and screenshots; these work on every OS.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One PPS event as reported by the kernel (or the simulator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pulse {
    pub assert_seq: u32,
    pub assert_sec: i64,
    pub assert_nsec: i32,
    pub clear_seq: u32,
    pub clear_sec: i64,
    pub clear_nsec: i32,
}

impl Pulse {
    pub fn assert_ns(&self) -> i128 {
        self.assert_sec as i128 * 1_000_000_000 + self.assert_nsec as i128
    }
    pub fn clear_ns(&self) -> i128 {
        self.clear_sec as i128 * 1_000_000_000 + self.clear_nsec as i128
    }
}

/// What a source can do, from `PPS_GETCAP`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub assert: bool,
    pub clear: bool,
    pub can_wait: bool,
}

impl Caps {
    pub fn describe(&self) -> String {
        let mut v = Vec::new();
        if self.assert {
            v.push("assert");
        }
        if self.clear {
            v.push("clear");
        }
        if self.can_wait {
            v.push("wait");
        }
        if v.is_empty() {
            "-".into()
        } else {
            v.join("+")
        }
    }
}

#[derive(Debug)]
pub enum Ev {
    /// Source index, description (kernel name etc.), capabilities.
    Opened(usize, String, Caps),
    Pulse(usize, Pulse),
    /// No pulse within the fetch timeout.
    Timeout(usize),
    /// Source failed (will be retried); human readable.
    Error(usize, String),
    Key(u8),
}

/// A parsed source argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Spec {
    Device(String),
    Sim(SimProfile),
}

impl Spec {
    pub fn parse(s: &str) -> Result<Spec, String> {
        if s == "sim" {
            return Ok(Spec::Sim(SimProfile::gpio()));
        }
        if let Some(p) = s.strip_prefix("sim:") {
            return SimProfile::named(p)
                .map(Spec::Sim)
                .ok_or_else(|| format!("unknown simulation profile '{p}' (gpio, uart, usb, bad)"));
        }
        if s.starts_with('/') {
            return Ok(Spec::Device(s.to_string()));
        }
        Err(format!(
            "'{s}': expected a device path such as /dev/pps0, or sim[:gpio|uart|usb|bad]"
        ))
    }

    pub fn label(&self) -> String {
        match self {
            Spec::Device(p) => p.clone(),
            Spec::Sim(p) => format!("sim:{}", p.name),
        }
    }
}

pub fn spawn(idx: usize, spec: Spec, tx: Sender<Ev>) {
    thread::spawn(move || match spec {
        Spec::Device(path) => device::run(idx, &path, &tx),
        Spec::Sim(p) => simulate(idx, p, &tx),
    });
}

// ---------------------------------------------------------------- Linux ---

#[cfg(target_os = "linux")]
pub mod device {
    use super::{Caps, Ev, Pulse};
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::sync::mpsc::Sender;
    use std::thread;
    use std::time::Duration;

    // linux/pps.h
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PpsKtime {
        sec: i64,
        nsec: i32,
        flags: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PpsKinfo {
        assert_sequence: u32,
        clear_sequence: u32,
        assert_tu: PpsKtime,
        clear_tu: PpsKtime,
        current_mode: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PpsFdata {
        info: PpsKinfo,
        timeout: PpsKtime,
    }

    const PPS_CAPTUREASSERT: i32 = 0x01;
    const PPS_CAPTURECLEAR: i32 = 0x02;
    const PPS_CANWAIT: i32 = 0x100;

    /// _IOC(dir, type, nr, size) for the asm-generic encoding used by x86,
    /// arm and arm64. linux/pps.h declares the ioctl argument types as
    /// *pointers* (`struct pps_fdata *`, `int *`), so the encoded size is
    /// the pointer size, not the struct size.
    const fn ioc(dir: u64, ty: u8, nr: u8, size: usize) -> u64 {
        (dir << 30) | ((size as u64) << 16) | ((ty as u64) << 8) | nr as u64
    }
    const IOC_WRITE: u64 = 1;
    const IOC_READ: u64 = 2;
    const PTR: usize = std::mem::size_of::<usize>();
    pub(super) const PPS_GETCAP: u64 = ioc(IOC_READ, b'p', 0xa3, PTR);
    pub(super) const PPS_FETCH: u64 = ioc(IOC_READ | IOC_WRITE, b'p', 0xa4, PTR);

    extern "C" {
        fn ioctl(fd: i32, req: std::ffi::c_ulong, ...) -> i32;
    }

    const FETCH_TIMEOUT_S: i64 = 2;

    fn getcap(f: &File) -> io::Result<Caps> {
        let mut mode: i32 = 0;
        // SAFETY: PPS_GETCAP writes an int.
        let r = unsafe {
            ioctl(
                f.as_raw_fd(),
                PPS_GETCAP as std::ffi::c_ulong,
                std::ptr::addr_of_mut!(mode),
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Caps {
            assert: mode & PPS_CAPTUREASSERT != 0,
            clear: mode & PPS_CAPTURECLEAR != 0,
            can_wait: mode & PPS_CANWAIT != 0,
        })
    }

    /// Wait (up to the timeout) for the next event and return it.
    fn fetch(f: &File) -> io::Result<Option<Pulse>> {
        let mut d = PpsFdata {
            timeout: PpsKtime {
                sec: FETCH_TIMEOUT_S,
                nsec: 0,
                flags: 0,
            },
            ..Default::default()
        };
        // SAFETY: PPS_FETCH reads the timeout and writes info in a pps_fdata
        // whose layout matches linux/pps.h (checked by a unit test).
        let r = unsafe {
            ioctl(
                f.as_raw_fd(),
                PPS_FETCH as std::ffi::c_ulong,
                std::ptr::addr_of_mut!(d),
            )
        };
        if r < 0 {
            let e = io::Error::last_os_error();
            return match e.raw_os_error() {
                Some(110) => Ok(None), // ETIMEDOUT
                Some(4) => Ok(None),   // EINTR
                _ => Err(e),
            };
        }
        let i = d.info;
        Ok(Some(Pulse {
            assert_seq: i.assert_sequence,
            assert_sec: i.assert_tu.sec,
            assert_nsec: i.assert_tu.nsec,
            clear_seq: i.clear_sequence,
            clear_sec: i.clear_tu.sec,
            clear_nsec: i.clear_tu.nsec,
        }))
    }

    /// `/sys/class/pps/ppsN/name`, e.g. "pps@12.-1" (pps-gpio) or "serial0".
    fn kernel_name(path: &str) -> Option<String> {
        let real = std::fs::canonicalize(path).ok()?;
        let base = real.file_name()?.to_str()?.to_string();
        let n = std::fs::read_to_string(format!("/sys/class/pps/{base}/name")).ok()?;
        let n = n.trim();
        Some(if real.to_str() != Some(path) {
            format!("{base} {n}") // path was a symlink such as /dev/gpspps0
        } else {
            n.to_string()
        })
    }

    pub fn permission_hint(path: &str) -> String {
        use std::os::unix::fs::MetadataExt;
        let m = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return String::new(),
        };
        let mode = m.mode() & 0o777;
        let base = std::fs::canonicalize(path)
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "pps0".into());
        if mode & 0o060 == 0 {
            format!(
                " ({path} is mode {mode:03o}, root only: run with sudo, or add a udev rule such as \
                 KERNEL==\"{base}\", GROUP=\"dialout\", MODE=\"0660\")"
            )
        } else {
            let gid = m.gid().to_string();
            let group = std::fs::read_to_string("/etc/group")
                .ok()
                .and_then(|g| {
                    g.lines().find_map(|l| {
                        let f: Vec<&str> = l.split(':').collect();
                        (f.len() > 2 && f[2] == gid).then(|| f[0].to_string())
                    })
                })
                .unwrap_or_else(|| format!("gid {gid}"));
            format!(
                " (mode {mode:03o}, group {group}: sudo usermod -aG {group} $USER, then log in again)"
            )
        }
    }

    pub fn run(idx: usize, path: &str, tx: &Sender<Ev>) {
        loop {
            let err = match File::open(path) {
                Err(e) => {
                    let hint = if e.kind() == io::ErrorKind::PermissionDenied {
                        permission_hint(path)
                    } else {
                        String::new()
                    };
                    format!("{e}{hint}")
                }
                Ok(f) => match getcap(&f) {
                    Err(e) => format!("PPS_GETCAP: {e} (not a PPS device?)"),
                    Ok(caps) => {
                        let name = kernel_name(path).unwrap_or_default();
                        if tx.send(Ev::Opened(idx, name, caps)).is_err() {
                            return;
                        }
                        loop {
                            match fetch(&f) {
                                Ok(Some(p)) => {
                                    if tx.send(Ev::Pulse(idx, p)).is_err() {
                                        return;
                                    }
                                }
                                Ok(None) => {
                                    if tx.send(Ev::Timeout(idx)).is_err() {
                                        return;
                                    }
                                }
                                Err(e) => break format!("PPS_FETCH: {e}"),
                            }
                        }
                    }
                },
            };
            if tx.send(Ev::Error(idx, err)).is_err() {
                return;
            }
            thread::sleep(Duration::from_secs(2));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn layout_and_ioctl_numbers() {
            assert_eq!(std::mem::size_of::<PpsKtime>(), 16);
            assert_eq!(std::mem::size_of::<PpsKinfo>(), 48);
            assert_eq!(std::mem::size_of::<PpsFdata>(), 64);
            #[cfg(target_pointer_width = "64")]
            {
                // Values as seen in strace of ppstest on x86_64 / aarch64.
                assert_eq!(PPS_FETCH, 0xc008_70a4);
                assert_eq!(PPS_GETCAP, 0x8008_70a3);
            }
            #[cfg(target_pointer_width = "32")]
            {
                assert_eq!(PPS_FETCH, 0xc004_70a4);
                assert_eq!(PPS_GETCAP, 0x8004_70a3);
            }
        }

        #[test]
        fn non_pps_device_is_rejected() {
            let f = File::open("/dev/null").unwrap();
            assert!(getcap(&f).is_err());
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub mod device {
    use super::Ev;
    use std::sync::mpsc::Sender;

    pub fn run(idx: usize, path: &str, tx: &Sender<Ev>) {
        let _ = tx.send(Ev::Error(
            idx,
            format!(
                "{path}: the PPS API (/dev/ppsN) is only supported on Linux; \
                 try sim:gpio for a demo"
            ),
        ));
    }
}

// ------------------------------------------------------------ simulator ---

/// Statistical model of a PPS source.
#[derive(Debug, Clone, PartialEq)]
pub struct SimProfile {
    pub name: &'static str,
    /// Constant offset of the system clock against the pulse.
    pub bias_ns: f64,
    /// White timestamp noise (interrupt latency etc.).
    pub sigma_ns: f64,
    /// Random-walk step per second (residual clock wander).
    pub walk_ns: f64,
    /// Probability per second of a missing pulse.
    pub p_miss: f64,
    /// Probability per second of an extra (glitch) pulse.
    pub p_extra: f64,
    /// Pulse width; None = clear edge not captured.
    pub width_ns: Option<f64>,
}

impl SimProfile {
    pub fn gpio() -> Self {
        SimProfile {
            name: "gpio",
            bias_ns: -300.0,
            sigma_ns: 3000.0,
            walk_ns: 80.0,
            p_miss: 0.0,
            p_extra: 0.0,
            width_ns: None,
        }
    }
    pub fn uart() -> Self {
        SimProfile {
            name: "uart",
            bias_ns: 700.0,
            sigma_ns: 350.0,
            walk_ns: 30.0,
            p_miss: 0.0,
            p_extra: 0.0,
            width_ns: Some(100e6),
        }
    }
    pub fn usb() -> Self {
        SimProfile {
            name: "usb",
            bias_ns: 450e3,
            sigma_ns: 280e3,
            walk_ns: 2e3,
            p_miss: 0.002,
            p_extra: 0.0,
            width_ns: None,
        }
    }
    pub fn bad() -> Self {
        SimProfile {
            name: "bad",
            bias_ns: 2e3,
            sigma_ns: 5e3,
            walk_ns: 300.0,
            p_miss: 0.03,
            p_extra: 0.02,
            width_ns: Some(100e6),
        }
    }
    pub fn named(n: &str) -> Option<Self> {
        Some(match n {
            "gpio" => Self::gpio(),
            "uart" => Self::uart(),
            "usb" => Self::usb(),
            "bad" => Self::bad(),
            _ => return None,
        })
    }
}

/// xorshift64* with Box-Muller normals; deterministic per seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn normal(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-300);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Generates the pulse sequence of a profile, independent of wall time.
pub struct SimState {
    p: SimProfile,
    rng: Rng,
    walk: f64,
    pub seq: u32,
    clear_seq: u32,
    clear: (i64, i32),
}

impl SimState {
    pub fn new(p: SimProfile, seed: u64) -> Self {
        SimState {
            p,
            rng: Rng::new(seed),
            walk: 0.0,
            seq: 0,
            clear_seq: 0,
            clear: (0, 0),
        }
    }

    fn stamp(sec: i64, off_ns: f64) -> (i64, i32) {
        let total = sec as i128 * 1_000_000_000 + off_ns.round() as i128;
        (
            total.div_euclid(1_000_000_000) as i64,
            total.rem_euclid(1_000_000_000) as i32,
        )
    }

    /// Pulses for the UTC second `sec` (zero, one or two of them).
    pub fn second(&mut self, sec: i64) -> Vec<Pulse> {
        let mut out = Vec::new();
        self.walk += self.rng.normal() * self.p.walk_ns;
        self.walk *= 0.995; // keep the wander bounded, like a disciplined clock
        if self.rng.uniform() < self.p.p_miss {
            return out;
        }
        let off = self.p.bias_ns + self.walk + self.rng.normal() * self.p.sigma_ns;
        out.push(self.make(sec, off));
        if self.rng.uniform() < self.p.p_extra {
            // A glitch somewhere inside the second.
            let g = 0.2e9 + self.rng.uniform() * 0.6e9;
            out.push(self.make(sec, g));
        }
        out
    }

    fn make(&mut self, sec: i64, off: f64) -> Pulse {
        self.seq = self.seq.wrapping_add(1);
        let a = Self::stamp(sec, off);
        if let Some(w) = self.p.width_ns {
            self.clear_seq = self.clear_seq.wrapping_add(1);
            self.clear = Self::stamp(sec, off + w);
        }
        Pulse {
            assert_seq: self.seq,
            assert_sec: a.0,
            assert_nsec: a.1,
            clear_seq: self.clear_seq,
            clear_sec: self.clear.0,
            clear_nsec: self.clear.1,
        }
    }
}

fn simulate(idx: usize, p: SimProfile, tx: &Sender<Ev>) {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        ^ (idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let caps = Caps {
        assert: true,
        clear: p.width_ns.is_some(),
        can_wait: true,
    };
    let mut st = SimState::new(p.clone(), seed);
    st.seq = 1_000_000 + (seed % 1_000_000) as u32; // look like a long-running source
    if tx
        .send(Ev::Opened(idx, format!("simulated {}", p.name), caps))
        .is_err()
    {
        return;
    }
    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let next = now.as_secs() as i64 + 1;
        let pulses = st.second(next);
        // Deliver each pulse at (about) its own timestamp; report a timeout
        // for seconds without a pulse, as the kernel API would.
        if pulses.is_empty() {
            let wait = Duration::from_secs(next as u64) + Duration::from_millis(5);
            thread::sleep(wait.saturating_sub(now));
            if tx.send(Ev::Timeout(idx)).is_err() {
                return;
            }
            continue;
        }
        for pl in pulses {
            let at = Duration::new(pl.assert_sec.max(0) as u64, pl.assert_nsec.max(0) as u32);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            thread::sleep(at.saturating_sub(now));
            if tx.send(Ev::Pulse(idx, pl)).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parsing() {
        assert_eq!(
            Spec::parse("/dev/pps0").unwrap(),
            Spec::Device("/dev/pps0".into())
        );
        assert_eq!(Spec::parse("sim").unwrap().label(), "sim:gpio");
        assert_eq!(Spec::parse("sim:uart").unwrap().label(), "sim:uart");
        assert!(Spec::parse("sim:nope").is_err());
        assert!(Spec::parse("pps0").is_err());
    }

    #[test]
    fn simulator_statistics() {
        let mut s = SimState::new(SimProfile::uart(), 42);
        let mut offs = Vec::new();
        for sec in 1_000_000..1_002_000i64 {
            for p in s.second(sec) {
                // noise can put the pulse just before the second boundary
                let off = p.assert_ns() - sec as i128 * 1_000_000_000;
                assert!(off.abs() < 10_000, "{off}");
                offs.push(off as f64);
                // clear edge 100 ms after assert
                let w = p.clear_ns() - p.assert_ns();
                assert!((w - 100_000_000).abs() < 2);
            }
        }
        assert_eq!(offs.len(), 2000);
        let mean = offs.iter().sum::<f64>() / offs.len() as f64;
        assert!((mean - 700.0).abs() < 400.0, "mean {mean}");
        // negative offsets land in the previous second
        let mut g = SimState::new(
            SimProfile {
                bias_ns: -5000.0,
                sigma_ns: 1.0,
                walk_ns: 0.0,
                ..SimProfile::gpio()
            },
            1,
        );
        let p = g.second(100)[0];
        assert_eq!(p.assert_sec, 99);
        assert!(
            (p.assert_nsec - 999_995_000).abs() <= 5,
            "{}",
            p.assert_nsec
        );
    }

    #[test]
    fn simulator_anomalies() {
        let mut s = SimState::new(SimProfile::bad(), 7);
        let (mut missing, mut extra) = (0, 0);
        for sec in 0..5000i64 {
            match s.second(sec).len() {
                0 => missing += 1,
                2 => extra += 1,
                _ => {}
            }
        }
        assert!(missing > 50 && extra > 30, "{missing} {extra}");
    }
}
