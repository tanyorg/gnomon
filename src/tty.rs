//! Minimal termios access for serial ports (no libc crate).
//!
//! Struct layouts and constants follow the system headers:
//!   Linux/glibc+musl  <asm-generic/termbits.h> + <bits/termios-struct.h>
//!   macOS             <sys/termios.h> (tcflag_t/speed_t are unsigned long)
//!   FreeBSD/NetBSD/OpenBSD/DragonFly <sys/termios.h> (32-bit fields)

use std::io;
use std::os::fd::RawFd;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod os {
    pub type Flag = u32;
    pub type Speed = u32;
    pub const NCCS: usize = 32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Termios {
        pub c_iflag: Flag,
        pub c_oflag: Flag,
        pub c_cflag: Flag,
        pub c_lflag: Flag,
        pub c_line: u8,
        pub c_cc: [u8; NCCS],
        pub c_ispeed: Speed,
        pub c_ospeed: Speed,
    }

    pub const CREAD: Flag = 0o200;
    pub const HUPCL: Flag = 0o2000;
    pub const CLOCAL: Flag = 0o4000;
    pub const CRTSCTS: Flag = 0o20000000000;
    /// All hardware flow-control / carrier-flow bits to clear.
    pub const FLOW_BITS: Flag = CRTSCTS;
    pub const VTIME: usize = 5;
    pub const VMIN: usize = 6;
    pub const TCSANOW: i32 = 0;
    pub const TCIFLUSH: i32 = 0;

    pub const TIOCEXCL: u64 = 0x540C;
    pub const TIOCNXCL: u64 = 0x540D;
    pub const TIOCMGET: u64 = 0x5415;

    /// Linux speed_t values are B-constants, not bit rates.
    pub fn speed_const(bps: u32) -> Option<Speed> {
        Some(match bps {
            1200 => 0o11,
            2400 => 0o13,
            4800 => 0o14,
            9600 => 0o15,
            19200 => 0o16,
            38400 => 0o17,
            57600 => 0o10001,
            115200 => 0o10002,
            230400 => 0o10003,
            460800 => 0o10004,
            500000 => 0o10005,
            576000 => 0o10006,
            921600 => 0o10007,
            1000000 => 0o10010,
            _ => return None,
        })
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod os {
    pub type Flag = std::ffi::c_ulong;
    pub type Speed = std::ffi::c_ulong;
    pub const NCCS: usize = 20;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Termios {
        pub c_iflag: Flag,
        pub c_oflag: Flag,
        pub c_cflag: Flag,
        pub c_lflag: Flag,
        pub c_cc: [u8; NCCS],
        pub c_ispeed: Speed,
        pub c_ospeed: Speed,
    }

    pub const CREAD: Flag = 0x0000_0800;
    pub const HUPCL: Flag = 0x0000_4000;
    pub const CLOCAL: Flag = 0x0000_8000;
    // CCTS_OFLOW | CRTS_IFLOW | CDTR_IFLOW | CDSR_OFLOW | CCAR_OFLOW
    pub const FLOW_BITS: Flag = 0x0001_0000 | 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
    pub const VMIN: usize = 16;
    pub const VTIME: usize = 17;
    pub const TCSANOW: i32 = 0;
    pub const TCIFLUSH: i32 = 1;

    pub const TIOCEXCL: u64 = 0x2000_740D; // _IO('t', 13)
    pub const TIOCNXCL: u64 = 0x2000_740E; // _IO('t', 14)
    pub const TIOCMGET: u64 = 0x4004_746A; // _IOR('t', 106, int)

    /// BSD-derived systems use the numeric bit rate as speed_t.
    pub fn speed_const(bps: u32) -> Option<Speed> {
        Some(bps as Speed)
    }
}

#[cfg(any(
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
mod os {
    pub type Flag = u32;
    pub type Speed = u32;
    pub const NCCS: usize = 20;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Termios {
        pub c_iflag: Flag,
        pub c_oflag: Flag,
        pub c_cflag: Flag,
        pub c_lflag: Flag,
        pub c_cc: [u8; NCCS],
        pub c_ispeed: Speed,
        pub c_ospeed: Speed,
    }

    pub const CREAD: Flag = 0x0000_0800;
    pub const HUPCL: Flag = 0x0000_4000;
    pub const CLOCAL: Flag = 0x0000_8000;
    // CCTS_OFLOW | CRTS_IFLOW | CDTR_IFLOW | CDSR_OFLOW | CCAR_OFLOW (MDMBUF)
    pub const FLOW_BITS: Flag = 0x0001_0000 | 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
    pub const VMIN: usize = 16;
    pub const VTIME: usize = 17;
    pub const TCSANOW: i32 = 0;
    pub const TCIFLUSH: i32 = 1;

    pub const TIOCEXCL: u64 = 0x2000_740D;
    pub const TIOCNXCL: u64 = 0x2000_740E;
    pub const TIOCMGET: u64 = 0x4004_746A;

    pub fn speed_const(bps: u32) -> Option<Speed> {
        Some(bps as Speed)
    }
}

use os::*;

/// Modem status bit for Data Carrier Detect (same value everywhere).
const TIOCM_CAR: i32 = 0o100;

extern "C" {
    fn tcgetattr(fd: i32, t: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, act: i32, t: *const Termios) -> i32;
    fn cfmakeraw(t: *mut Termios);
    fn cfsetispeed(t: *mut Termios, s: Speed) -> i32;
    fn cfsetospeed(t: *mut Termios, s: Speed) -> i32;
    fn cfgetispeed(t: *const Termios) -> Speed;
    fn tcflush(fd: i32, queue: i32) -> i32;
    fn ioctl(fd: i32, req: std::ffi::c_ulong, ...) -> i32;
}

fn check(r: i32, what: &str) -> io::Result<()> {
    if r < 0 {
        let e = io::Error::last_os_error();
        Err(io::Error::new(e.kind(), format!("{what}: {e}")))
    } else {
        Ok(())
    }
}

fn get(fd: RawFd) -> io::Result<Termios> {
    // SAFETY: Termios is plain old data matching the C layout; tcgetattr fills it.
    unsafe {
        let mut t: Termios = std::mem::zeroed();
        check(tcgetattr(fd, &mut t), "tcgetattr")?;
        Ok(t)
    }
}

/// 8N1, raw, no flow control, **CLOCAL** (ignore DCD, like kermit's
/// SET CARRIER-WATCH OFF), HUPCL off, VMIN=0/VTIME=5 (0.5 s read timeout).
/// The result is read back and verified.
pub fn configure(fd: RawFd, bps: u32) -> io::Result<()> {
    let sp = speed_const(bps).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported speed {bps}"),
        )
    })?;
    let mut t = get(fd)?;
    // SAFETY: valid, initialised Termios.
    unsafe { cfmakeraw(&mut t) };
    t.c_cflag |= CLOCAL | CREAD;
    t.c_cflag &= !(HUPCL | FLOW_BITS);
    t.c_cc[VMIN] = 0;
    t.c_cc[VTIME] = 5;
    // SAFETY: as above.
    unsafe {
        check(cfsetispeed(&mut t, sp), "cfsetispeed")?;
        check(cfsetospeed(&mut t, sp), "cfsetospeed")?;
        check(tcsetattr(fd, TCSANOW, &t), "tcsetattr")?;
    }
    // tcsetattr succeeds if *any* change was applied, so verify.
    let r = get(fd)?;
    if r.c_cflag & CLOCAL == 0 {
        return Err(io::Error::other(
            "driver refused CLOCAL (carrier watch still on)",
        ));
    }
    // SAFETY: valid Termios.
    let got = unsafe { cfgetispeed(&r) };
    if got != sp {
        return Err(io::Error::other(format!("driver refused speed {bps}")));
    }
    Ok(())
}

/// Discard input that accumulated while nobody was reading.
pub fn flush_input(fd: RawFd) {
    // SAFETY: tcflush on a tty descriptor; failure is harmless.
    unsafe {
        tcflush(fd, TCIFLUSH);
    }
}

/// Ask for exclusive use so gpsd (or another monitor) cannot open the port
/// while we hold it. Not effective against root on Linux, and does not evict
/// a process that already has it open. Best effort.
pub fn set_exclusive(fd: RawFd, on: bool) {
    let req = if on { TIOCEXCL } else { TIOCNXCL };
    // SAFETY: argument-less ioctl.
    unsafe {
        ioctl(fd, req as std::ffi::c_ulong);
    }
}

/// Data Carrier Detect state, None if the driver has no modem lines (pty).
pub fn carrier(fd: RawFd) -> Option<bool> {
    let mut bits: i32 = 0;
    // SAFETY: TIOCMGET writes an int.
    let r = unsafe {
        ioctl(
            fd,
            TIOCMGET as std::ffi::c_ulong,
            std::ptr::addr_of_mut!(bits),
        )
    };
    (r == 0).then_some(bits & TIOCM_CAR != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        #[cfg(target_os = "linux")]
        assert_eq!(std::mem::size_of::<Termios>(), 60);
        #[cfg(all(target_os = "macos", target_pointer_width = "64"))]
        assert_eq!(std::mem::size_of::<Termios>(), 72);
        #[cfg(target_os = "freebsd")]
        assert_eq!(std::mem::size_of::<Termios>(), 44);
        assert!(speed_const(9600).is_some());
        assert!(speed_const(115200).is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn configure_pty() {
        use std::os::fd::AsRawFd;
        extern "C" {
            fn openpty(m: *mut i32, s: *mut i32, n: *mut i8, t: *const u8, w: *const u8) -> i32;
        }
        let (mut m, mut s) = (0, 0);
        // SAFETY: openpty fills two fds.
        let r = unsafe {
            openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if r != 0 {
            return; // no pty support in this environment
        }
        // SAFETY: we own both fds.
        let (mf, sf) = unsafe {
            use std::os::fd::FromRawFd;
            (std::fs::File::from_raw_fd(m), std::fs::File::from_raw_fd(s))
        };
        configure(sf.as_raw_fd(), 115200).unwrap();
        let t = get(sf.as_raw_fd()).unwrap();
        assert_ne!(t.c_cflag & CLOCAL, 0);
        assert_eq!(t.c_cflag & (HUPCL | FLOW_BITS), 0);
        assert_eq!((t.c_cc[VMIN], t.c_cc[VTIME]), (0, 5));
        assert!(configure(sf.as_raw_fd(), 12345).is_err());
        assert_eq!(carrier(sf.as_raw_fd()), None); // pty: no modem lines
        drop(mf);
    }
}
