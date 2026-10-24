//! Shared code for the gnomon family of tools.
//!
//! * `gnomon`     — NMEA 0183 monitor (gpsd, raw NMEA over TCP, serial)
//! * `metronomon` — PPS (pulse-per-second) monitor for the Linux PPS API
//!
//! The library is an implementation detail of the two binaries; its API is
//! not stable.

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
