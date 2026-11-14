//! Shared code for the gnomon family of tools.
//!
//! * `gnomon`     — NMEA 0183 monitor (gpsd, raw NMEA over TCP, serial)
//! * `metronomon` — PPS (pulse-per-second) monitor for the Linux PPS API
//!
//! The library is an implementation detail of the two binaries; its API is
//! not stable.

// On Linux the ioctl numbers (termios2, PPS API, TIOCGWINSZ) and struct
// layouts are written out for the asm-generic encoding. Alpha, MIPS,
// PowerPC, SPARC and others encode them differently; refuse to build there
// rather than issue wrong ioctls at run time.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    not(any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    ))
))]
compile_error!("Linux on this architecture uses a non-generic ioctl/termios layout; see src/tty.rs and src/pps.rs");

pub mod app;
pub mod json;
pub mod metro;
pub mod nmea;
pub mod pps;
pub mod serial;
pub mod source;
pub mod target;
pub mod term;
pub mod tty;
pub mod ui;
