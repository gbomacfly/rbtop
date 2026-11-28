//! Minimal terminal layer: raw mode, double-buffered cell grid with diff
//! flushing (only changed cells are written), and input parsing.
//! Deliberately no TUI framework — every byte written is accounted for.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

pub static RESIZED: AtomicBool = AtomicBool::new(false);
pub static TERMINATE: AtomicBool = AtomicBool::new(false);
pub static CONTINUED: AtomicBool = AtomicBool::new(false);

/// Colour: 0x00RRGGBB, or DEFAULT for the terminal default.
pub type Color = u32;
pub const DEFAULT: Color = 0x0100_0000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

impl Cell {
    pub const BLANK: Cell = Cell { ch: ' ', fg: DEFAULT, bg: DEFAULT, bold: false };
}

pub struct Screen {
    pub w: u16,
    pub h: u16,
    pub cells: Vec<Cell>,
    prev: Vec<Cell>,
    force: bool,
    out: Vec<u8>,
    pub bg: Color,
}

impl Screen {
    pub fn new(w: u16, h: u16) -> Self {
        let n = w as usize * h as usize;
        Screen { w, h, cells: vec![Cell::BLANK; n], prev: vec![Cell::BLANK; n], force: true, out: Vec::with_capacity(1 << 16), bg: DEFAULT }
    }

    pub fn resize(&mut self, w: u16, h: u16) {
        let n = w as usize * h as usize;
        self.w = w;
        self.h = h;
        self.cells = vec![Cell::BLANK; n];
        self.prev = vec![Cell::BLANK; n];
        self.force = true;
    }

    pub fn invalidate(&mut self) {
        self.force = true;
    }

    pub fn clear(&mut self, fg: Color, bg: Color) {
        self.bg = bg;
        let c = Cell { ch: ' ', fg, bg, bold: false };
        for x in self.cells.iter_mut() {
            *x = c;
        }
    }

    #[inline]
    pub fn set(&mut self, x: i32, y: i32, ch: char, fg: Color, bold: bool) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = y as usize * self.w as usize + x as usize;
        let c = &mut self.cells[i];
        c.ch = ch;
        c.fg = fg;
        c.bold = bold;
    }

    #[inline]
    pub fn set_bg(&mut self, x: i32, y: i32, bg: Color) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = y as usize * self.w as usize + x as usize;
        self.cells[i].bg = bg;
    }

    /// Write text, clipped to `max` columns. Returns x after the text.
    pub fn text(&mut self, x: i32, y: i32, s: &str, fg: Color, bold: bool) -> i32 {
        let mut cx = x;
        for ch in s.chars() {
            self.set(cx, y, sanitize(ch), fg, bold);
            cx = cx.saturating_add(1);
        }
        cx
    }

    pub fn text_max(&mut self, x: i32, y: i32, s: &str, max: usize, fg: Color, bold: bool) -> i32 {
        let mut cx = x;
        for ch in s.chars().take(max) {
            self.set(cx, y, sanitize(ch), fg, bold);
            cx = cx.saturating_add(1);
        }
        cx
    }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, bg: Color) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.set(xx, yy, ' ', DEFAULT, false);
                self.set_bg(xx, yy, bg);
            }
        }
    }

    /// Darken everything (used behind menus/dialogs, like bashtop's pause screen).
    pub fn dim(&mut self, to: Color) {
        for c in self.cells.iter_mut() {
            if c.fg != DEFAULT {
                c.fg = darken(c.fg);
            } else {
                c.fg = to;
            }
            if c.bg != DEFAULT {
                c.bg = darken(c.bg);
            }
            c.bold = false;
        }
    }

    /// Emit only the changed cells, with minimal cursor moves and SGR changes.
    pub fn flush(&mut self) {
        let out = &mut self.out;
        out.clear();
        out.extend_from_slice(b"\x1b[?2026h"); // synchronized update (ignored if unsupported)
        if self.force {
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
        }
        let w = self.w as usize;
        let mut cur: Option<(usize, usize)> = None;
        let mut fg: Option<Color> = None;
        let mut bg: Option<Color> = None;
        let mut bold: Option<bool> = None;
        let mut buf = [0u8; 4];
        for (i, c) in self.cells.iter().enumerate() {
            if !self.force && self.prev[i] == *c {
                continue;
            }
            let (x, y) = (i % w, i / w);
            if cur != Some((x, y)) {
                let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
            }
            if bold != Some(c.bold) {
                if c.bold {
                    out.extend_from_slice(b"\x1b[1m");
                } else {
                    out.extend_from_slice(b"\x1b[22m");
                }
                bold = Some(c.bold);
            }
            if fg != Some(c.fg) {
                sgr(out, c.fg, true);
                fg = Some(c.fg);
            }
            if bg != Some(c.bg) {
                sgr(out, c.bg, false);
                bg = Some(c.bg);
            }
            out.extend_from_slice(c.ch.encode_utf8(&mut buf).as_bytes());
            cur = if x + 1 < w { Some((x + 1, y)) } else { None };
        }
        out.extend_from_slice(b"\x1b[?2026l");
        self.prev.copy_from_slice(&self.cells);
        self.force = false;
        write_all(out);
    }
}

fn sgr(out: &mut Vec<u8>, c: Color, fg: bool) {
    if c == DEFAULT {
        out.extend_from_slice(if fg { b"\x1b[39m" } else { b"\x1b[49m" });
    } else {
        let _ = write!(out, "\x1b[{};2;{};{};{}m", if fg { 38 } else { 48 }, (c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
    }
}

#[inline]
fn sanitize(ch: char) -> char {
    let u = ch as u32;
    // C0, DEL and C1 controls (U+009B is a one-byte CSI on some terminals):
    // process names and command lines must never reach the terminal as control codes.
    if u < 0x20 || (0x7f..=0x9f).contains(&u) {
        return ' ';
    }
    // Bidi overrides/isolates could make a process name display as something
    // else ("Trojan Source"); zero-width and combining marks shift the grid.
    if (0x200b..=0x200f).contains(&u) || (0x202a..=0x202e).contains(&u) || (0x2066..=0x2069).contains(&u) || u == 0xfeff || u == 0x061c {
        return '?';
    }
    // Replace double-width CJK/emoji so the grid stays aligned.
    if (0x1100..=0x115f).contains(&u) || (0x2e80..=0xa4cf).contains(&u) || (0xac00..=0xd7a3).contains(&u)
        || (0xf900..=0xfaff).contains(&u) || (0xfe30..=0xfe4f).contains(&u) || (0xff00..=0xff60).contains(&u)
        || (0xffe0..=0xffe6).contains(&u) || (0x1f300..=0x1faff).contains(&u) || (0x20000..=0x3fffd).contains(&u)
    {
        return '?';
    }
    ch
}

pub fn darken(c: Color) -> Color {
    let r = ((c >> 16) & 0xff) * 2 / 5;
    let g = ((c >> 8) & 0xff) * 2 / 5;
    let b = (c & 0xff) * 2 / 5;
    (r << 16) | (g << 8) | b
}

pub fn write_all(b: &[u8]) {
    let mut off = 0;
    while off < b.len() {
        let n = unsafe { libc::write(1, b[off..].as_ptr() as *const libc::c_void, b.len() - off) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        off += n as usize;
    }
}

// ---------------------------------------------------------------- raw mode

static mut ORIG: Option<libc::termios> = None;
static MOUSE: AtomicBool = AtomicBool::new(true);

pub fn size() -> (u16, u16) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }
}

pub fn enter(mouse: bool) {
    MOUSE.store(mouse, Ordering::Relaxed);
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            if (*std::ptr::addr_of!(ORIG)).is_none() {
                ORIG = Some(t);
            }
            t.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
            t.c_iflag &= !(libc::IXON | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::BRKINT);
            t.c_cc[libc::VMIN] = 0;
            t.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(0, libc::TCSAFLUSH, &t);
        }
    }
    let mut s = String::from("\x1b[?1049h\x1b[?25l\x1b[?7l");
    if mouse {
        s.push_str("\x1b[?1000h\x1b[?1006h");
    }
    write_all(s.as_bytes());
}

pub fn leave() {
    let mut s = String::new();
    if MOUSE.load(Ordering::Relaxed) {
        s.push_str("\x1b[?1000l\x1b[?1006l");
    }
    s.push_str("\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l");
    write_all(s.as_bytes());
    unsafe {
        if let Some(t) = *std::ptr::addr_of!(ORIG) {
            libc::tcsetattr(0, libc::TCSAFLUSH, &t);
        }
    }
}

extern "C" fn on_winch(_: libc::c_int) {
    RESIZED.store(true, Ordering::Relaxed);
}
extern "C" fn on_term(_: libc::c_int) {
    TERMINATE.store(true, Ordering::Relaxed);
}
extern "C" fn on_cont(_: libc::c_int) {
    CONTINUED.store(true, Ordering::Relaxed);
    RESIZED.store(true, Ordering::Relaxed);
}

pub fn install_signals() {
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGCONT, on_cont as *const () as libc::sighandler_t);
    }
}

/// Ctrl-Z: restore terminal, stop ourselves, re-enter on SIGCONT.
pub fn suspend(mouse: bool) {
    leave();
    unsafe {
        libc::raise(libc::SIGTSTP);
    }
    enter(mouse);
}

// ---------------------------------------------------------------- input

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    PgUp,
    PgDn,
    Home,
    End,
    Enter,
    Esc,
    Backspace,
    Delete,
    Tab,
    BackTab,
    F(u8),
    CtrlC,
    CtrlZ,
    MouseDown(u16, u16),
    /// Internal: click on a process column header (index into the header list).
    SortCol(u8),
    WheelUp(u16, u16),
    WheelDown(u16, u16),
}

/// Wait up to `ms` for input. Returns true if stdin is readable.
/// select() instead of poll(): macOS poll() does not support tty devices
/// (it returns POLLNVAL), select() works on both Linux and macOS.
pub fn poll_input(ms: i32) -> bool {
    let ms = ms.max(0);
    unsafe {
        let mut set: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut set);
        libc::FD_SET(0, &mut set);
        let mut tv = libc::timeval { tv_sec: (ms / 1000) as libc::time_t, tv_usec: ((ms % 1000) * 1000) as libc::suseconds_t };
        let r = libc::select(1, &mut set, std::ptr::null_mut(), std::ptr::null_mut(), &mut tv);
        r > 0 && libc::FD_ISSET(0, &set)
    }
}

thread_local! {
    /// Bytes of an escape sequence or UTF-8 character that was cut off at the
    /// end of the last read (slow ssh links split "\x1b[A" or mouse reports).
    static PENDING: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// How long to wait for the rest of a cut-off sequence before a lone Esc
/// counts as the Esc key (like vim's ttimeoutlen).
const ESC_TIMEOUT_MS: i32 = 50;
/// An unterminated sequence longer than this is garbage and gets dropped.
const MAX_PENDING: usize = 64;

pub fn read_keys(out: &mut Vec<Key>) {
    PENDING.with(|p| {
        let mut pend = p.borrow_mut();
        let mut buf = [0u8; 1024];
        let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            pend.extend_from_slice(&buf[..n as usize]);
        }
        // wait for the rest of a split sequence: each next byte may take up to
        // ESC_TIMEOUT_MS, the whole sequence at most 4x that (and MAX_PENDING bytes)
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(4 * ESC_TIMEOUT_MS as u64);
        loop {
            let used = parse(&pend, out, false);
            pend.drain(..used);
            if pend.is_empty() {
                return;
            }
            if pend.len() > MAX_PENDING || std::time::Instant::now() >= deadline || !poll_input(ESC_TIMEOUT_MS) {
                break;
            }
            let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            pend.extend_from_slice(&buf[..n as usize]);
        }
        // timed out: whatever is left is final (lone Esc = Esc key)
        parse(&pend, out, true);
        pend.clear();
    })
}

/// Parse input bytes into keys. Returns how many bytes were consumed: with
/// `last == false` an incomplete sequence at the end is left unconsumed so the
/// caller can complete it with the next read; with `last == true` everything
/// is consumed (a lone Esc becomes Key::Esc, a cut-off sequence is dropped).
fn parse(b: &[u8], out: &mut Vec<Key>, last: bool) -> usize {
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            0x1b => {
                if i + 1 >= b.len() {
                    if !last {
                        return i;
                    }
                    out.push(Key::Esc);
                    i += 1;
                    continue;
                }
                let n = b[i + 1];
                if n == b'O' && i + 2 >= b.len() && !last {
                    return i;
                }
                if n == b'O' && i + 2 < b.len() {
                    out.push(match b[i + 2] {
                        b'P' => Key::F(1),
                        b'Q' => Key::F(2),
                        b'R' => Key::F(3),
                        b'S' => Key::F(4),
                        b'A' => Key::Up,
                        b'B' => Key::Down,
                        b'C' => Key::Right,
                        b'D' => Key::Left,
                        b'H' => Key::Home,
                        b'F' => Key::End,
                        _ => Key::Esc,
                    });
                    i += 3;
                } else if n == b'[' {
                    // CSI: params until final byte 0x40..0x7e
                    let mut j = i + 2;
                    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                        j += 1;
                    }
                    if j >= b.len() {
                        if !last {
                            return i;
                        }
                        i = b.len();
                        continue;
                    }
                    let params = &b[i + 2..j];
                    let fin = b[j];
                    i = j + 1;
                    if params.first() == Some(&b'<') {
                        // SGR mouse: <btn;x;y(M|m)
                        let s = std::str::from_utf8(&params[1..]).unwrap_or("");
                        let v: Vec<u32> = s.split(';').filter_map(|p| p.parse().ok()).collect();
                        if v.len() == 3 && fin == b'M' {
                            let (bt, x, y) = (v[0], v[1].saturating_sub(1) as u16, v[2].saturating_sub(1) as u16);
                            match bt & 0b1100_0011 {
                                0 => out.push(Key::MouseDown(x, y)),
                                64 => out.push(Key::WheelUp(x, y)),
                                65 => out.push(Key::WheelDown(x, y)),
                                _ => {}
                            }
                        }
                        continue;
                    }
                    let num: u32 = std::str::from_utf8(params).ok().and_then(|s| s.split(';').next()).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let k = match fin {
                        b'A' => Some(Key::Up),
                        b'B' => Some(Key::Down),
                        b'C' => Some(Key::Right),
                        b'D' => Some(Key::Left),
                        b'H' => Some(Key::Home),
                        b'F' => Some(Key::End),
                        b'Z' => Some(Key::BackTab),
                        b'P' => Some(Key::F(1)),
                        b'Q' => Some(Key::F(2)),
                        b'~' => match num {
                            1 | 7 => Some(Key::Home),
                            4 | 8 => Some(Key::End),
                            3 => Some(Key::Delete),
                            5 => Some(Key::PgUp),
                            6 => Some(Key::PgDn),
                            11 => Some(Key::F(1)),
                            12 => Some(Key::F(2)),
                            13 => Some(Key::F(3)),
                            14 => Some(Key::F(4)),
                            15 => Some(Key::F(5)),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(k) = k {
                        out.push(k);
                    }
                } else {
                    // Alt+key or lone Esc followed by a key
                    out.push(Key::Esc);
                    i += 1;
                }
            }
            b'\r' | b'\n' => {
                out.push(Key::Enter);
                i += 1;
            }
            0x7f | 0x08 => {
                out.push(Key::Backspace);
                i += 1;
            }
            b'\t' => {
                out.push(Key::Tab);
                i += 1;
            }
            0x03 => {
                out.push(Key::CtrlC);
                i += 1;
            }
            0x1a => {
                out.push(Key::CtrlZ);
                i += 1;
            }
            c if c < 0x20 => {
                i += 1;
            }
            _ => {
                let len = if c >= 0xf0 { 4 } else if c >= 0xe0 { 3 } else if c >= 0xc0 { 2 } else { 1 };
                if i + len > b.len() && !last {
                    return i;
                }
                let end = (i + len).min(b.len());
                if let Ok(s) = std::str::from_utf8(&b[i..end]) {
                    if let Some(ch) = s.chars().next() {
                        out.push(Key::Char(ch));
                    }
                }
                i = end;
            }
        }
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_keys() {
        let mut v = Vec::new();
        parse(b"\x1b[A\x1b[6~\x1bOPq\x1b[<0;10;5M\x1b[<64;3;3M\x1b", &mut v, true);
        assert_eq!(v, vec![Key::Up, Key::PgDn, Key::F(1), Key::Char('q'), Key::MouseDown(9, 4), Key::WheelUp(2, 2), Key::Esc]);
    }
    #[test]
    fn sanitize_controls() {
        assert_eq!(sanitize('\u{1b}'), ' ');
        assert_eq!(sanitize('\u{9b}'), ' ');
        assert_eq!(sanitize('\u{85}'), ' ');
        assert_eq!(sanitize('\u{202e}'), '?');
        assert_eq!(sanitize('\u{200b}'), '?');
        assert_eq!(sanitize('a'), 'a');
        assert_eq!(sanitize('ä'), 'ä');
        assert_eq!(sanitize('█'), '█');
    }
    #[test]
    fn split_sequences() {
        // every split point of an arrow key, a mouse click and a 3-byte char
        // must give the same keys as the unsplit input
        let input: &[u8] = "\x1b[A\x1b[<0;10;5M€x\x1bOP".as_bytes();
        let mut want = Vec::new();
        parse(input, &mut want, true);
        assert_eq!(want, vec![Key::Up, Key::MouseDown(9, 4), Key::Char('€'), Key::Char('x'), Key::F(1)]);
        for cut in 0..=input.len() {
            let mut got = Vec::new();
            let mut pend = input[..cut].to_vec();
            let used = parse(&pend, &mut got, false);
            pend.drain(..used);
            pend.extend_from_slice(&input[cut..]);
            parse(&pend, &mut got, true);
            assert_eq!(got, want, "split at {cut}");
        }
    }
}
