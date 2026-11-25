//! Drawing primitives: boxes, braille graphs, meters, number formatting.

use crate::term::{Color, Screen};
use crate::theme::Gradient;
use std::collections::VecDeque;

pub fn draw_box(s: &mut Screen, x: i32, y: i32, w: i32, h: i32, c: Color) {
    if w < 2 || h < 2 {
        return;
    }
    s.set(x, y, '╭', c, false);
    s.set(x + w - 1, y, '╮', c, false);
    s.set(x, y + h - 1, '╰', c, false);
    s.set(x + w - 1, y + h - 1, '╯', c, false);
    for i in x + 1..x + w - 1 {
        s.set(i, y, '─', c, false);
        s.set(i, y + h - 1, '─', c, false);
    }
    for j in y + 1..y + h - 1 {
        s.set(x, j, '│', c, false);
        s.set(x + w - 1, j, '│', c, false);
    }
}

/// `┤text├` segment on a border. Parts are (text, color, bold).
pub fn tag(s: &mut Screen, x: i32, y: i32, line: Color, parts: &[(&str, Color, bool)]) -> i32 {
    s.set(x, y, '┤', line, false);
    let mut cx = x + 1;
    for (t, c, b) in parts {
        cx = s.text(cx, y, t, *c, *b);
    }
    s.set(cx, y, '├', line, false);
    cx + 1
}

pub fn tag_len(parts: &[(&str, Color, bool)]) -> i32 {
    2 + parts.iter().map(|p| p.0.chars().count() as i32).sum::<i32>()
}

const UP_L: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
const UP_R: [u32; 4] = [0x80, 0x20, 0x10, 0x08];
const DN_L: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
const DN_R: [u32; 4] = [0x08, 0x10, 0x20, 0x80];

#[inline]
fn level(v: f32, levels: i32) -> i32 {
    if v <= 0.0 {
        return 0;
    }
    ((v / 100.0 * levels as f32).round() as i32).clamp(1, levels)
}

/// Braille graph. `data` values are 0..=100, newest last. Right-aligned.
pub fn graph<I>(s: &mut Screen, x: i32, y: i32, w: i32, h: i32, data: I, n: usize, grad: &Gradient, invert: bool, hires: bool)
where
    I: DoubleEndedIterator<Item = f32> + ExactSizeIterator,
{
    if w <= 0 || h <= 0 {
        return;
    }
    let per = if hires { 2 } else { 1 };
    let need = (w as usize) * per;
    let skip = n.saturating_sub(need);
    let vals: Vec<f32> = data.skip(skip).collect();
    let pad = need.saturating_sub(vals.len());
    let levels = h * 4;
    for col in 0..w {
        let (l, r) = if hires {
            let i = col as usize * 2;
            let a = if i >= pad { vals[i - pad] } else { -1.0 };
            let b = if i + 1 >= pad { vals[i + 1 - pad] } else { -1.0 };
            (a, b)
        } else {
            let i = col as usize;
            let a = if i >= pad { vals[i - pad] } else { -1.0 };
            (a, a)
        };
        if l < 0.0 && r < 0.0 {
            continue;
        }
        let (ll, lr) = (level(l, levels), level(r, levels));
        for row in 0..h {
            // row 0 is top for normal graphs, bottom for inverted
            let base = (h - 1 - row) * 4;
            let fl = (ll - base).clamp(0, 4) as usize;
            let fr = (lr - base).clamp(0, 4) as usize;
            if fl == 0 && fr == 0 {
                continue;
            }
            let (tl, tr) = if invert { (&DN_L, &DN_R) } else { (&UP_L, &UP_R) };
            let mut bits = 0u32;
            for k in 0..fl {
                bits |= tl[k];
            }
            for k in 0..fr {
                bits |= tr[k];
            }
            let ch = char::from_u32(0x2800 + bits).unwrap_or(' ');
            let pos = ((h - row) * 100 / h).clamp(0, 100) as usize;
            let yy = if invert { y + (h - 1 - row) } else { y + row };
            s.set(x + col, yy, ch, grad[pos], false);
        }
    }
}

pub fn graph_dq(s: &mut Screen, x: i32, y: i32, w: i32, h: i32, d: &VecDeque<f32>, grad: &Gradient, invert: bool, hires: bool) {
    graph(s, x, y, w, h, d.iter().copied(), d.len(), grad, invert, hires);
}

/// Mini graph colored by each value (used for cores / processes).
pub fn mini<I>(s: &mut Screen, x: i32, y: i32, w: i32, data: I, n: usize, grad: &Gradient, hires: bool)
where
    I: DoubleEndedIterator<Item = f32> + ExactSizeIterator,
{
    let per = if hires { 2 } else { 1 };
    let need = (w.max(0) as usize) * per;
    let vals: Vec<f32> = data.skip(n.saturating_sub(need)).collect();
    let pad = need.saturating_sub(vals.len());
    for col in 0..w {
        let get = |i: usize| if i >= pad { vals[i - pad] } else { -1.0 };
        let (l, r) = if hires { (get(col as usize * 2), get(col as usize * 2 + 1)) } else { (get(col as usize), get(col as usize)) };
        if l < 0.0 && r < 0.0 {
            continue;
        }
        let (fl, fr) = (level(l, 4) as usize, level(r, 4) as usize);
        let mut bits = 0;
        for k in 0..fl {
            bits |= UP_L[k];
        }
        for k in 0..fr {
            bits |= UP_R[k];
        }
        let ch = if bits == 0 { ' ' } else { char::from_u32(0x2800 + bits).unwrap_or(' ') };
        let v = l.max(r).clamp(0.0, 100.0) as usize;
        s.set(x + col, y, ch, grad[v], false);
    }
}

pub fn meter(s: &mut Screen, x: i32, y: i32, w: i32, v: f32, grad: &Gradient, empty: Color) {
    let filled = ((v.clamp(0.0, 100.0) / 100.0) * w as f32).round() as i32;
    for i in 0..w {
        if i < filled {
            let pos = ((i + 1) * 100 / w.max(1)).clamp(0, 100) as usize;
            s.set(x + i, y, '■', grad[pos], false);
        } else {
            s.set(x + i, y, '■', empty, false);
        }
    }
}

pub fn fmt_bytes(v: f64) -> String {
    const U: [&str; 7] = ["Byte", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut v = v;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", v as u64, U[0])
    } else if v >= 100.0 {
        format!("{:.0} {}", v, U[i])
    } else if v >= 10.0 {
        format!("{:.1} {}", v, U[i])
    } else {
        format!("{:.2} {}", v, U[i])
    }
}

/// Short form for tight spaces: "1.5G", "512M"
pub fn fmt_short(v: f64) -> String {
    const U: [&str; 7] = ["B", "K", "M", "G", "T", "P", "E"];
    let mut v = v;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 10.0 || i == 0 { format!("{:.0}{}", v, U[i]) } else { format!("{:.1}{}", v, U[i]) }
}

pub fn fmt_bits(bytes_per_s: f64) -> String {
    const U: [&str; 5] = ["bps", "Kbps", "Mbps", "Gbps", "Tbps"];
    let mut v = bytes_per_s * 8.0;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if v >= 100.0 || i == 0 { format!("{:.0} {}", v, U[i]) } else { format!("{:.1} {}", v, U[i]) }
}

pub fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (d, h, m, sec) = (s / 86400, (s / 3600) % 24, (s / 60) % 60, s % 60);
    if d > 0 { format!("{d}d {h:02}:{m:02}") } else { format!("{h:02}:{m:02}:{sec:02}") }
}

pub fn strftime(fmt: &str) -> String {
    if fmt.is_empty() {
        return String::new();
    }
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let Ok(cf) = std::ffi::CString::new(fmt) else { return String::new() };
        let mut buf = [0u8; 128];
        let n = libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), cf.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

pub fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n).collect() }
}

pub fn pad(s: &str, n: usize) -> String {
    let c = s.chars().count();
    if c >= n { trunc(s, n) } else { format!("{s}{}", " ".repeat(n - c)) }
}

pub fn rpad(s: &str, n: usize) -> String {
    let c = s.chars().count();
    if c >= n { trunc(s, n) } else { format!("{}{s}", " ".repeat(n - c)) }
}
