//! Minimal termios access for serial ports (no libc crate).
//!
//! Linux talks to the kernel directly with `TCGETS2`/`TCSETS2` and
//! `struct termios2` (<asm-generic/termbits.h>, <asm-generic/ioctls.h>), so
//! the speed is a plain bit rate (`BOTHER`) whatever the C library thinks
//! `speed_t` means: glibc 2.42 changed it from B-codes to bit rates, and a
//! binary built against either ABI must still set the right speed.
//!
//! The other systems go through the C library, where `speed_t` has always
//! been the bit rate. Layouts and constants follow <sys/termios.h>:
//!   macOS             tcflag_t/speed_t are unsigned long
//!   FreeBSD/NetBSD/OpenBSD/DragonFly  32-bit fields

use std::io;
use std::os::fd::RawFd;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod os {
    use std::io;
    use std::os::fd::RawFd;

    pub type Flag = u32;
    pub type Speed = u32;
    /// NCCS of the kernel's struct termios2 (not the C library's 32).
    pub const NCCS: usize = 19;

    /// The kernel's `struct termios2`.
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

    // c_iflag
    const IGNBRK: Flag = 0o1;
    const BRKINT: Flag = 0o2;
    const PARMRK: Flag = 0o10;
    const ISTRIP: Flag = 0o40;
    const INLCR: Flag = 0o100;
    const IGNCR: Flag = 0o200;
    const ICRNL: Flag = 0o400;
    const IXON: Flag = 0o2000;
    // c_oflag
    const OPOST: Flag = 0o1;
    // c_lflag
    const ISIG: Flag = 0o1;
    const ICANON: Flag = 0o2;
    const ECHO: Flag = 0o10;
    const ECHONL: Flag = 0o100;
    const IEXTEN: Flag = 0o100000;
    // c_cflag
    const CSIZE: Flag = 0o60;
    const CS8: Flag = 0o60;
    const PARENB: Flag = 0o400;
    const CBAUD: Flag = 0o10017;
    const BOTHER: Flag = 0o10000;
    const IBSHIFT: u32 = 16;
    const CIBAUD: Flag = CBAUD << IBSHIFT;

    pub const CREAD: Flag = 0o200;
    pub const HUPCL: Flag = 0o2000;
    pub const CLOCAL: Flag = 0o4000;
    pub const CRTSCTS: Flag = 0o20000000000;
    /// All hardware flow-control / carrier-flow bits to clear.
    pub const FLOW_BITS: Flag = CRTSCTS;
    pub const VTIME: usize = 5;
    pub const VMIN: usize = 6;

    const TCGETS2: u64 = 0x802C_542A; // _IOR('T', 0x2A, struct termios2)
    const TCSETS2: u64 = 0x402C_542B; // _IOW('T', 0x2B, struct termios2)
    pub const TCFLSH: u64 = 0x540B;
    pub const TCIFLUSH: i32 = 0;

    pub const TIOCEXCL: u64 = 0x540C;
    pub const TIOCNXCL: u64 = 0x540D;
    pub const TIOCMGET: u64 = 0x5415;

    /// The kernel's own code for a standard rate (asm-generic termbits.h).
    /// This is kernel ABI, unrelated to what the C library calls speed_t.
    fn kernel_code(bps: u32) -> Option<Flag> {
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

    /// Speeds offered on the command line: the standard rates.
    pub fn speed_const(bps: u32) -> Option<Speed> {
        kernel_code(bps).map(|_| bps)
    }

    pub fn get(fd: RawFd) -> io::Result<Termios> {
        // SAFETY: Termios matches the kernel's struct termios2 (size checked
        // by a unit test); TCGETS2 fills it.
        unsafe {
            let mut t: Termios = std::mem::zeroed();
            super::check(
                super::ioctl(fd, TCGETS2 as std::ffi::c_ulong, std::ptr::addr_of_mut!(t)),
                "TCGETS2",
            )?;
            Ok(t)
        }
    }

    pub fn set(fd: RawFd, t: &Termios) -> io::Result<()> {
        // SAFETY: as above; TCSETS2 only reads the struct.
        super::check(
            unsafe { super::ioctl(fd, TCSETS2 as std::ffi::c_ulong, std::ptr::from_ref(t)) },
            "TCSETS2",
        )
    }

    /// What glibc's and musl's cfmakeraw do.
    pub fn make_raw(t: &mut Termios) {
        t.c_iflag &= !(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON);
        t.c_oflag &= !OPOST;
        t.c_lflag &= !(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
        t.c_cflag &= !(CSIZE | PARENB);
        t.c_cflag |= CS8;
    }

    /// Both directions at `sp` bits per second. Standard rates use their
    /// kernel code, so programs reading the legacy struct termios (stty,
    /// gpsd) still see e.g. B9600; anything else is sent as a plain rate
    /// with BOTHER. CIBAUD = 0 means "input speed = output speed".
    pub fn set_speed(t: &mut Termios, sp: Speed) -> io::Result<()> {
        t.c_cflag &= !(CBAUD | CIBAUD);
        t.c_cflag |= kernel_code(sp).unwrap_or(BOTHER);
        t.c_ispeed = sp;
        t.c_ospeed = sp;
        Ok(())
    }

    /// The rate the driver accepted (it may have rounded the request).
    pub fn speed(t: &Termios) -> Speed {
        t.c_ospeed
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

    pub const TIOCEXCL: u64 = 0x2000_740D; // _IO('t', 13)
    pub const TIOCNXCL: u64 = 0x2000_740E; // _IO('t', 14)
    pub const TIOCMGET: u64 = 0x4004_746A; // _IOR('t', 106, int)

    /// BSD-derived systems use the numeric bit rate as speed_t.
    pub fn speed_const(bps: u32) -> Option<Speed> {
        Some(bps as Speed)
    }

    pub use super::libc_termios::*;
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

    pub const TIOCEXCL: u64 = 0x2000_740D;
    pub const TIOCNXCL: u64 = 0x2000_740E;
    pub const TIOCMGET: u64 = 0x4004_746A;

    pub fn speed_const(bps: u32) -> Option<Speed> {
        Some(bps as Speed)
    }

    pub use super::libc_termios::*;
}

/// termios through the C library, for systems where speed_t is a bit rate.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod libc_termios {
    use super::os::{Speed, Termios};
    use std::io;
    use std::os::fd::RawFd;

    pub const TCSANOW: i32 = 0;
    pub const TCIFLUSH: i32 = 1;

    extern "C" {
        fn tcgetattr(fd: i32, t: *mut Termios) -> i32;
        fn tcsetattr(fd: i32, act: i32, t: *const Termios) -> i32;
        fn cfmakeraw(t: *mut Termios);
        fn cfsetispeed(t: *mut Termios, s: Speed) -> i32;
        fn cfsetospeed(t: *mut Termios, s: Speed) -> i32;
        fn cfgetispeed(t: *const Termios) -> Speed;
        pub fn tcflush(fd: i32, queue: i32) -> i32;
    }

    pub fn get(fd: RawFd) -> io::Result<Termios> {
        // SAFETY: Termios is plain old data matching the C layout; tcgetattr fills it.
        unsafe {
            let mut t: Termios = std::mem::zeroed();
            super::check(tcgetattr(fd, &mut t), "tcgetattr")?;
            Ok(t)
        }
    }

    pub fn set(fd: RawFd, t: &Termios) -> io::Result<()> {
        // SAFETY: valid, initialised Termios.
        super::check(unsafe { tcsetattr(fd, TCSANOW, t) }, "tcsetattr")
    }

    pub fn make_raw(t: &mut Termios) {
        // SAFETY: valid, initialised Termios.
        unsafe { cfmakeraw(t) };
    }

    pub fn set_speed(t: &mut Termios, sp: Speed) -> io::Result<()> {
        // SAFETY: valid, initialised Termios.
        unsafe {
            super::check(cfsetispeed(t, sp), "cfsetispeed")?;
            super::check(cfsetospeed(t, sp), "cfsetospeed")
        }
    }

    pub fn speed(t: &Termios) -> Speed {
        // SAFETY: valid Termios.
        unsafe { cfgetispeed(t) }
    }
}

use os::*;

/// Modem status bit for Data Carrier Detect (same value everywhere).
const TIOCM_CAR: i32 = 0o100;

extern "C" {
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
    make_raw(&mut t);
    t.c_cflag |= CLOCAL | CREAD;
    t.c_cflag &= !(HUPCL | FLOW_BITS);
    t.c_cc[VMIN] = 0;
    t.c_cc[VTIME] = 5;
    set_speed(&mut t, sp)?;
    set(fd, &t)?;
    // tcsetattr succeeds if *any* change was applied, so verify.
    let r = get(fd)?;
    if r.c_cflag & CLOCAL == 0 {
        return Err(io::Error::other(
            "driver refused CLOCAL (carrier watch still on)",
        ));
    }
    if speed(&r) != sp {
        return Err(io::Error::other(format!("driver refused speed {bps}")));
    }
    Ok(())
}

/// Discard input that accumulated while nobody was reading.
pub fn flush_input(fd: RawFd) {
    // SAFETY: flushing a tty descriptor; failure is harmless.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        ioctl(fd, TCFLSH as std::ffi::c_ulong, TCIFLUSH);
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
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
        // Linux: the kernel's struct termios2.
        #[cfg(target_os = "linux")]
        assert_eq!(std::mem::size_of::<Termios>(), 44);
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
