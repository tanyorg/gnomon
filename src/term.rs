//! Tiny terminal layer: raw mode via stty(1), size via TIOCGWINSZ, and a
//! cell canvas rendered with line-level diffing using ANSI/VT100 sequences.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

static SAVED_STTY: OnceLock<String> = OnceLock::new();

pub fn enter() -> io::Result<()> {
    let out = Command::new("stty")
        .arg("-g")
        .stdin(Stdio::inherit())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("stty -g failed (not a tty?)"));
    }
    let saved = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let _ = SAVED_STTY.set(saved);
    let st = Command::new("stty")
        .args(["raw", "-echo"])
        .stdin(Stdio::inherit())
        .status()?;
    if !st.success() {
        return Err(io::Error::other("stty raw failed"));
    }
    let mut o = io::stdout().lock();
    // alt screen, hide cursor, no autowrap, clear
    o.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[2J")?;
    o.flush()
}

pub fn leave() {
    let mut o = io::stdout().lock();
    let _ = o.write_all(b"\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l");
    let _ = o.flush();
    if let Some(s) = SAVED_STTY.get() {
        let _ = Command::new("stty").arg(s).stdin(Stdio::inherit()).status();
    }
}

#[repr(C)]
#[derive(Default)]
struct Winsize {
    row: u16,
    col: u16,
    xpix: u16,
    ypix: u16,
}

extern "C" {
    fn ioctl(fd: i32, req: std::ffi::c_ulong, ...) -> i32;
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const TIOCGWINSZ: std::ffi::c_ulong = 0x5413;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const TIOCGWINSZ: std::ffi::c_ulong = 0x4008_7468; // macOS and the BSDs

pub fn size() -> (usize, usize) {
    let mut ws = Winsize::default();
    // SAFETY: TIOCGWINSZ writes a struct winsize into the pointer we pass.
    let r = unsafe { ioctl(1, TIOCGWINSZ, &mut ws as *mut Winsize) };
    if r == 0 && ws.col > 0 && ws.row > 0 {
        return (ws.col as usize, ws.row as usize);
    }
    let env = |k: &str, d| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    (env("COLUMNS", 80), env("LINES", 24))
}

pub mod st {
    pub const BOLD: u8 = 1;
    pub const DIM: u8 = 2;
    pub const REV: u8 = 4;
    pub const GREEN: u8 = 8;
    pub const RED: u8 = 16;
    pub const YELLOW: u8 = 32;
    pub const CYAN: u8 = 64;
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    style: u8,
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    cells: Vec<Cell>,
    pub ascii: bool,
}

impl Canvas {
    pub fn new(w: usize, h: usize, ascii: bool) -> Self {
        Canvas {
            w,
            h,
            cells: vec![Cell { ch: ' ', style: 0 }; w * h],
            ascii,
        }
    }

    pub fn put(&mut self, x: usize, y: usize, s: &str, style: u8) -> usize {
        if y >= self.h {
            return x;
        }
        let mut cx = x;
        for ch in s.chars() {
            if cx >= self.w {
                break;
            }
            // Keep everything single-width: replace control and non-ASCII
            // characters that could have ambiguous width in CJK locales.
            let ch = if ch.is_ascii_graphic() || ch == ' ' || is_box(ch) {
                ch
            } else {
                '?'
            };
            self.cells[y * self.w + cx] = Cell { ch, style };
            cx += 1;
        }
        cx
    }

    pub fn fill(&mut self, x: usize, y: usize, w: usize, style: u8) {
        for cx in x..(x + w).min(self.w) {
            if y < self.h {
                self.cells[y * self.w + cx] = Cell { ch: ' ', style };
            }
        }
    }

    pub fn boxed(&mut self, x: usize, y: usize, w: usize, h: usize, title: &str) {
        if w < 2 || h < 2 {
            return;
        }
        let (hz, vt, tl, tr, bl, br) = if self.ascii {
            ('-', '|', '+', '+', '+', '+')
        } else {
            (
                '\u{2500}', '\u{2502}', '\u{250C}', '\u{2510}', '\u{2514}', '\u{2518}',
            )
        };
        let line = |c: &mut Canvas, yy: usize, l: char, r: char| {
            let mut s = String::new();
            s.push(l);
            for _ in 0..w - 2 {
                s.push(hz);
            }
            s.push(r);
            c.put(x, yy, &s, st::DIM);
        };
        line(self, y, tl, tr);
        line(self, y + h - 1, bl, br);
        for yy in y + 1..y + h - 1 {
            self.put(x, yy, &vt.to_string(), st::DIM);
            self.put(x + w - 1, yy, &vt.to_string(), st::DIM);
        }
        if !title.is_empty() && w > 4 {
            let t: String = title.chars().take(w - 4).collect();
            self.put(x + 1, y, " ", 0);
            let e = self.put(x + 2, y, &t, st::BOLD);
            self.put(e, y, " ", 0);
        }
    }

    fn line_sgr(&self, y: usize) -> String {
        let mut out = String::with_capacity(self.w + 16);
        let mut cur = 255u8;
        for c in &self.cells[y * self.w..(y + 1) * self.w] {
            if c.style != cur {
                out.push_str("\x1b[0");
                if c.style & st::BOLD != 0 {
                    out.push_str(";1");
                }
                if c.style & st::DIM != 0 {
                    out.push_str(";2");
                }
                if c.style & st::REV != 0 {
                    out.push_str(";7");
                }
                if c.style & st::GREEN != 0 {
                    out.push_str(";32");
                }
                if c.style & st::RED != 0 {
                    out.push_str(";31");
                }
                if c.style & st::YELLOW != 0 {
                    out.push_str(";33");
                }
                if c.style & st::CYAN != 0 {
                    out.push_str(";36");
                }
                out.push('m');
                cur = c.style;
            }
            out.push(c.ch);
        }
        out.push_str("\x1b[0m");
        out
    }

    /// Plain text dump (used by --snapshot and tests).
    pub fn text(&self) -> String {
        let mut s = String::new();
        for y in 0..self.h {
            let l: String = self.cells[y * self.w..(y + 1) * self.w]
                .iter()
                .map(|c| c.ch)
                .collect();
            s.push_str(l.trim_end());
            s.push('\n');
        }
        s
    }
}

fn is_box(c: char) -> bool {
    ('\u{2500}'..='\u{257F}').contains(&c)
}

/// Holds the previously drawn lines so only changes are written.
#[derive(Default)]
pub struct Screen {
    prev: Vec<String>,
    dims: (usize, usize),
}

impl Screen {
    pub fn draw(&mut self, c: &Canvas) -> io::Result<()> {
        let mut out = String::new();
        if self.dims != (c.w, c.h) {
            self.prev.clear();
            self.dims = (c.w, c.h);
            out.push_str("\x1b[2J");
        }
        self.prev.resize(c.h, String::new());
        for y in 0..c.h {
            let l = c.line_sgr(y);
            if self.prev[y] != l {
                out.push_str(&format!("\x1b[{};1H", y + 1));
                out.push_str(&l);
                self.prev[y] = l;
            }
        }
        if !out.is_empty() {
            let mut o = io::stdout().lock();
            o.write_all(out.as_bytes())?;
            o.flush()?;
        }
        Ok(())
    }
}
