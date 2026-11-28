//! Platform-independent helpers shared by the Linux and macOS collectors.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;

pub const HIST: usize = 600;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_to(path: &str, buf: &mut String) -> bool {
    buf.clear();
    match File::open(path) {
        Ok(mut f) => f.read_to_string(buf).is_ok(),
        Err(_) => false,
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_str(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_num(path: &str) -> Option<f64> {
    read_str(path).and_then(|s| s.parse().ok())
}

pub fn push<T>(h: &mut VecDeque<T>, v: T) {
    if h.len() >= HIST {
        h.pop_front();
    }
    h.push_back(v);
}

pub fn shorten_model(m: &str) -> String {
    // Generic virtual CPU models (Proxmox/QEMU default cpu types)
    let lower = m.to_ascii_lowercase();
    if lower.starts_with("qemu virtual cpu") {
        return "QEMU vCPU".into();
    }
    if lower.contains("common kvm processor") || lower.starts_with("kvm virtual") {
        return "KVM vCPU".into();
    }
    let mut s = m.to_string();
    for pat in ["(R)", "(r)", "(TM)", "(tm)", "CPU", "Processor", "Core", "Intel", "AMD", "Technology", "with Radeon Graphics", "Radeon Graphics", "-Core", "Six", "Eight", "Quad", "Twelve", "Sixteen", "Dual"] {
        s = s.replace(pat, " ");
    }
    if let Some(i) = s.find('@') {
        s.truncate(i);
    }
    let words: Vec<&str> = s.split_whitespace().filter(|w| !w.ends_with("-Core")).collect();
    let out = words.join(" ");
    if out.is_empty() { m.trim().to_string() } else { out }
}

/// User name for a uid via getpwuid_r (works for /etc/passwd, LDAP/sssd and macOS Directory Services).
pub fn lookup_user(uid: u32) -> Option<String> {
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = [0 as libc::c_char; 4096];
        let mut res: *mut libc::passwd = std::ptr::null_mut();
        let r = libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut res);
        if r == 0 && !res.is_null() && !pw.pw_name.is_null() {
            return Some(std::ffi::CStr::from_ptr(pw.pw_name).to_string_lossy().into_owned());
        }
    }
    None
}

/// Join argv parts into one command line. Returns the line and the byte offset
/// where the arguments start (after argv[0]), so program paths with spaces
/// ("/Applications/Google Chrome.app/...") are not split in the middle.
pub fn join_argv<'a, I: Iterator<Item = &'a [u8]>>(parts: I) -> (String, usize) {
    let mut out = String::new();
    let mut args_at = 0;
    for (i, p) in parts.enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&String::from_utf8_lossy(p));
        if i == 0 {
            args_at = out.len();
        }
    }
    // newlines would break the row layout; same byte length, so args_at stays valid
    let out = out.replace('\n', " ");
    let args_at = if out.len() > args_at { args_at + 1 } else { out.len() };
    (out, args_at)
}

/// Parse a macOS KERN_PROCARGS2 buffer into (command line, args offset):
/// [argc: i32][exec path\0][\0 padding][argv0\0 argv1\0 ...][env ...]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_procargs2(data: &[u8]) -> (String, usize) {
    if data.len() < 4 {
        return (String::new(), 0);
    }
    let argc = i32::from_ne_bytes([data[0], data[1], data[2], data[3]]).max(0) as usize;
    let mut i = 4;
    while i < data.len() && data[i] != 0 {
        i += 1; // exec path
    }
    while i < data.len() && data[i] == 0 {
        i += 1; // padding
    }
    let mut parts: Vec<&[u8]> = Vec::with_capacity(argc.min(64));
    while parts.len() < argc && i < data.len() {
        let s = i;
        while i < data.len() && data[i] != 0 {
            i += 1;
        }
        parts.push(&data[s..i]);
        i += 1;
    }
    join_argv(parts.into_iter())
}

/// Evenly spaced subset of at most `max` elements (first and last included).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn thin<T: Clone>(v: &[T], max: usize) -> Vec<T> {
    if v.len() <= max || max == 0 {
        return if max == 0 { Vec::new() } else { v.to_vec() };
    }
    if max == 1 {
        return vec![v[0].clone()];
    }
    (0..max).map(|i| v[i * (v.len() - 1) / (max - 1)].clone()).collect()
}

// ---- SMC helpers (macOS temperatures); pure functions so they are unit-tested everywhere

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn fourcc(s: &str) -> u32 {
    let b = s.as_bytes();
    let g = |i: usize| *b.get(i).unwrap_or(&b' ') as u32;
    (g(0) << 24) | (g(1) << 16) | (g(2) << 8) | g(3)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn fourcc_str(k: u32) -> String {
    k.to_be_bytes().iter().map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '?' }).collect()
}

/// Decode a SMC value: "flt " little-endian f32 (Apple Silicon), "sp78" signed 8.8
/// fixed point big-endian (Intel), "ui8 "/"ui16" integers.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn decode_smc(data_type: u32, bytes: &[u8]) -> Option<f32> {
    match &data_type.to_be_bytes() {
        b"flt " if bytes.len() >= 4 => Some(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        b"sp78" if bytes.len() >= 2 => Some(i16::from_be_bytes([bytes[0], bytes[1]]) as f32 / 256.0),
        b"ui8 " if !bytes.is_empty() => Some(bytes[0] as f32),
        b"ui16" if bytes.len() >= 2 => Some(u16::from_be_bytes([bytes[0], bytes[1]]) as f32),
        _ => None,
    }
}


// ---- display cleanup for process names / command lines

/// (base, combining mark, composed) for Latin-1 Supplement .. Latin Extended-B,
/// sorted, generated from Python's unicodedata.
const COMPOSE: [(u8, u16, u16); 222] = [
    (0x41, 0x0300, 0x00c0), (0x41, 0x0301, 0x00c1), (0x41, 0x0302, 0x00c2), (0x41, 0x0303, 0x00c3),
    (0x41, 0x0304, 0x0100), (0x41, 0x0306, 0x0102), (0x41, 0x0307, 0x0226), (0x41, 0x0308, 0x00c4),
    (0x41, 0x030a, 0x00c5), (0x41, 0x030c, 0x01cd), (0x41, 0x030f, 0x0200), (0x41, 0x0311, 0x0202),
    (0x41, 0x0328, 0x0104), (0x43, 0x0301, 0x0106), (0x43, 0x0302, 0x0108), (0x43, 0x0307, 0x010a),
    (0x43, 0x030c, 0x010c), (0x43, 0x0327, 0x00c7), (0x44, 0x030c, 0x010e), (0x45, 0x0300, 0x00c8),
    (0x45, 0x0301, 0x00c9), (0x45, 0x0302, 0x00ca), (0x45, 0x0304, 0x0112), (0x45, 0x0306, 0x0114),
    (0x45, 0x0307, 0x0116), (0x45, 0x0308, 0x00cb), (0x45, 0x030c, 0x011a), (0x45, 0x030f, 0x0204),
    (0x45, 0x0311, 0x0206), (0x45, 0x0327, 0x0228), (0x45, 0x0328, 0x0118), (0x47, 0x0301, 0x01f4),
    (0x47, 0x0302, 0x011c), (0x47, 0x0306, 0x011e), (0x47, 0x0307, 0x0120), (0x47, 0x030c, 0x01e6),
    (0x47, 0x0327, 0x0122), (0x48, 0x0302, 0x0124), (0x48, 0x030c, 0x021e), (0x49, 0x0300, 0x00cc),
    (0x49, 0x0301, 0x00cd), (0x49, 0x0302, 0x00ce), (0x49, 0x0303, 0x0128), (0x49, 0x0304, 0x012a),
    (0x49, 0x0306, 0x012c), (0x49, 0x0307, 0x0130), (0x49, 0x0308, 0x00cf), (0x49, 0x030c, 0x01cf),
    (0x49, 0x030f, 0x0208), (0x49, 0x0311, 0x020a), (0x49, 0x0328, 0x012e), (0x4a, 0x0302, 0x0134),
    (0x4b, 0x030c, 0x01e8), (0x4b, 0x0327, 0x0136), (0x4c, 0x0301, 0x0139), (0x4c, 0x030c, 0x013d),
    (0x4c, 0x0327, 0x013b), (0x4e, 0x0300, 0x01f8), (0x4e, 0x0301, 0x0143), (0x4e, 0x0303, 0x00d1),
    (0x4e, 0x030c, 0x0147), (0x4e, 0x0327, 0x0145), (0x4f, 0x0300, 0x00d2), (0x4f, 0x0301, 0x00d3),
    (0x4f, 0x0302, 0x00d4), (0x4f, 0x0303, 0x00d5), (0x4f, 0x0304, 0x014c), (0x4f, 0x0306, 0x014e),
    (0x4f, 0x0307, 0x022e), (0x4f, 0x0308, 0x00d6), (0x4f, 0x030b, 0x0150), (0x4f, 0x030c, 0x01d1),
    (0x4f, 0x030f, 0x020c), (0x4f, 0x0311, 0x020e), (0x4f, 0x031b, 0x01a0), (0x4f, 0x0328, 0x01ea),
    (0x52, 0x0301, 0x0154), (0x52, 0x030c, 0x0158), (0x52, 0x030f, 0x0210), (0x52, 0x0311, 0x0212),
    (0x52, 0x0327, 0x0156), (0x53, 0x0301, 0x015a), (0x53, 0x0302, 0x015c), (0x53, 0x030c, 0x0160),
    (0x53, 0x0326, 0x0218), (0x53, 0x0327, 0x015e), (0x54, 0x030c, 0x0164), (0x54, 0x0326, 0x021a),
    (0x54, 0x0327, 0x0162), (0x55, 0x0300, 0x00d9), (0x55, 0x0301, 0x00da), (0x55, 0x0302, 0x00db),
    (0x55, 0x0303, 0x0168), (0x55, 0x0304, 0x016a), (0x55, 0x0306, 0x016c), (0x55, 0x0308, 0x00dc),
    (0x55, 0x030a, 0x016e), (0x55, 0x030b, 0x0170), (0x55, 0x030c, 0x01d3), (0x55, 0x030f, 0x0214),
    (0x55, 0x0311, 0x0216), (0x55, 0x031b, 0x01af), (0x55, 0x0328, 0x0172), (0x57, 0x0302, 0x0174),
    (0x59, 0x0301, 0x00dd), (0x59, 0x0302, 0x0176), (0x59, 0x0304, 0x0232), (0x59, 0x0308, 0x0178),
    (0x5a, 0x0301, 0x0179), (0x5a, 0x0307, 0x017b), (0x5a, 0x030c, 0x017d), (0x61, 0x0300, 0x00e0),
    (0x61, 0x0301, 0x00e1), (0x61, 0x0302, 0x00e2), (0x61, 0x0303, 0x00e3), (0x61, 0x0304, 0x0101),
    (0x61, 0x0306, 0x0103), (0x61, 0x0307, 0x0227), (0x61, 0x0308, 0x00e4), (0x61, 0x030a, 0x00e5),
    (0x61, 0x030c, 0x01ce), (0x61, 0x030f, 0x0201), (0x61, 0x0311, 0x0203), (0x61, 0x0328, 0x0105),
    (0x63, 0x0301, 0x0107), (0x63, 0x0302, 0x0109), (0x63, 0x0307, 0x010b), (0x63, 0x030c, 0x010d),
    (0x63, 0x0327, 0x00e7), (0x64, 0x030c, 0x010f), (0x65, 0x0300, 0x00e8), (0x65, 0x0301, 0x00e9),
    (0x65, 0x0302, 0x00ea), (0x65, 0x0304, 0x0113), (0x65, 0x0306, 0x0115), (0x65, 0x0307, 0x0117),
    (0x65, 0x0308, 0x00eb), (0x65, 0x030c, 0x011b), (0x65, 0x030f, 0x0205), (0x65, 0x0311, 0x0207),
    (0x65, 0x0327, 0x0229), (0x65, 0x0328, 0x0119), (0x67, 0x0301, 0x01f5), (0x67, 0x0302, 0x011d),
    (0x67, 0x0306, 0x011f), (0x67, 0x0307, 0x0121), (0x67, 0x030c, 0x01e7), (0x67, 0x0327, 0x0123),
    (0x68, 0x0302, 0x0125), (0x68, 0x030c, 0x021f), (0x69, 0x0300, 0x00ec), (0x69, 0x0301, 0x00ed),
    (0x69, 0x0302, 0x00ee), (0x69, 0x0303, 0x0129), (0x69, 0x0304, 0x012b), (0x69, 0x0306, 0x012d),
    (0x69, 0x0308, 0x00ef), (0x69, 0x030c, 0x01d0), (0x69, 0x030f, 0x0209), (0x69, 0x0311, 0x020b),
    (0x69, 0x0328, 0x012f), (0x6a, 0x0302, 0x0135), (0x6a, 0x030c, 0x01f0), (0x6b, 0x030c, 0x01e9),
    (0x6b, 0x0327, 0x0137), (0x6c, 0x0301, 0x013a), (0x6c, 0x030c, 0x013e), (0x6c, 0x0327, 0x013c),
    (0x6e, 0x0300, 0x01f9), (0x6e, 0x0301, 0x0144), (0x6e, 0x0303, 0x00f1), (0x6e, 0x030c, 0x0148),
    (0x6e, 0x0327, 0x0146), (0x6f, 0x0300, 0x00f2), (0x6f, 0x0301, 0x00f3), (0x6f, 0x0302, 0x00f4),
    (0x6f, 0x0303, 0x00f5), (0x6f, 0x0304, 0x014d), (0x6f, 0x0306, 0x014f), (0x6f, 0x0307, 0x022f),
    (0x6f, 0x0308, 0x00f6), (0x6f, 0x030b, 0x0151), (0x6f, 0x030c, 0x01d2), (0x6f, 0x030f, 0x020d),
    (0x6f, 0x0311, 0x020f), (0x6f, 0x031b, 0x01a1), (0x6f, 0x0328, 0x01eb), (0x72, 0x0301, 0x0155),
    (0x72, 0x030c, 0x0159), (0x72, 0x030f, 0x0211), (0x72, 0x0311, 0x0213), (0x72, 0x0327, 0x0157),
    (0x73, 0x0301, 0x015b), (0x73, 0x0302, 0x015d), (0x73, 0x030c, 0x0161), (0x73, 0x0326, 0x0219),
    (0x73, 0x0327, 0x015f), (0x74, 0x030c, 0x0165), (0x74, 0x0326, 0x021b), (0x74, 0x0327, 0x0163),
    (0x75, 0x0300, 0x00f9), (0x75, 0x0301, 0x00fa), (0x75, 0x0302, 0x00fb), (0x75, 0x0303, 0x0169),
    (0x75, 0x0304, 0x016b), (0x75, 0x0306, 0x016d), (0x75, 0x0308, 0x00fc), (0x75, 0x030a, 0x016f),
    (0x75, 0x030b, 0x0171), (0x75, 0x030c, 0x01d4), (0x75, 0x030f, 0x0215), (0x75, 0x0311, 0x0217),
    (0x75, 0x031b, 0x01b0), (0x75, 0x0328, 0x0173), (0x77, 0x0302, 0x0175), (0x79, 0x0301, 0x00fd),
    (0x79, 0x0302, 0x0177), (0x79, 0x0304, 0x0233), (0x79, 0x0308, 0x00ff), (0x7a, 0x0301, 0x017a),
    (0x7a, 0x0307, 0x017c), (0x7a, 0x030c, 0x017e),
];

/// Characters that take no cell of their own: combining marks, variation
/// selectors, zero-width spaces/joiners, word joiner and friends.
fn zero_width(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036f | 0x0483..=0x0489 | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x200b..=0x200f
        | 0x2060..=0x2064 | 0x20d0..=0x20ff | 0xfe00..=0xfe0f | 0xfe20..=0xfe2f | 0xfeff | 0xe0100..=0xe01ef)
}

/// One terminal cell per char: compose decomposed Latin letters (macOS keeps
/// names in NFD, "U" + U+0308 becomes "Ü") and drop the remaining zero-width
/// characters, so width counting by chars() stays right. ASCII is returned as is.
pub fn display_clean(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_ascii() {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !zero_width(c) {
            out.push(c);
            continue;
        }
        let composed = out.chars().last().filter(char::is_ascii).and_then(|b| {
            let key = (b as u8, c as u32 as u16);
            COMPOSE.binary_search_by(|&(tb, tm, _)| (tb, tm).cmp(&key)).ok().and_then(|i| char::from_u32(COMPOSE[i].2 as u32))
        });
        if let Some(ch) = composed {
            out.pop();
            out.push(ch);
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_models() {
        assert_eq!(shorten_model("QEMU Virtual CPU version 2.5+"), "QEMU vCPU");
        assert_eq!(shorten_model("Common KVM processor"), "KVM vCPU");
        assert_eq!(shorten_model("AMD Ryzen 7 5700G with Radeon Graphics"), "Ryzen 7 5700G");
        assert_eq!(shorten_model("Intel(R) Core(TM) i5-2400 CPU @ 3.10GHz"), "i5-2400");
        assert_eq!(shorten_model("Apple M2 Pro"), "Apple M2 Pro");
    }
    #[test]
    fn procargs2() {
        let mut b = 3i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/usr/bin/python3\0\0\0\0python3\0-m\0http.server\0PATH=/usr/bin\0HOME=/x\0");
        let (cmd, at) = parse_procargs2(&b);
        assert_eq!(cmd, "python3 -m http.server");
        assert_eq!(&cmd[at..], "-m http.server");
        let mut b = 1i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/sbin/launchd\0\0/sbin/launchd\0");
        let (cmd, at) = parse_procargs2(&b);
        assert_eq!(cmd, "/sbin/launchd");
        assert_eq!(&cmd[at..], "");
        assert_eq!(parse_procargs2(&[1, 0]), (String::new(), 0));
        let mut b = 5i32.to_ne_bytes().to_vec(); // truncated buffer must not panic
        b.extend_from_slice(b"/bin/x\0\0a\0b");
        assert_eq!(parse_procargs2(&b).0, "a b");
    }
    #[test]
    fn argv_with_spaces() {
        let chrome: [&[u8]; 3] = [b"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", b"--type=renderer", b"--lang=de"];
        let (cmd, at) = join_argv(chrome.into_iter());
        assert_eq!(&cmd[at..], "--type=renderer --lang=de");
        assert_eq!(&cmd[..at - 1], "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        let one: [&[u8]; 1] = [b"/usr/sbin/sshd"];
        let (cmd, at) = join_argv(one.into_iter());
        assert_eq!(&cmd[at..], "");
        let (cmd, at) = join_argv(std::iter::empty());
        assert_eq!((cmd.as_str(), at), ("", 0));
    }
    #[test]
    fn thin_subset() {
        let v: Vec<u32> = (0..30).collect();
        assert_eq!(thin(&v, 6), vec![0, 5, 11, 17, 23, 29]);
        assert_eq!(thin(&v[..4], 6), vec![0, 1, 2, 3]);
        assert_eq!(thin(&v, 1), vec![0]);
        assert!(thin(&v, 0).is_empty());
    }
    #[test]
    fn smc_helpers() {
        assert_eq!(fourcc("#KEY"), 0x234b_4559);
        assert_eq!(fourcc_str(fourcc("Tp01")), "Tp01");
        assert_eq!(fourcc_str(fourcc("ui8")), "ui8 ");
        // Apple Silicon: little-endian f32
        assert_eq!(decode_smc(fourcc("flt "), &45.5f32.to_le_bytes()), Some(45.5));
        // Intel: sp78 signed 8.8 fixed point, big endian (0x2D80 = 45.5)
        assert_eq!(decode_smc(fourcc("sp78"), &[0x2d, 0x80]), Some(45.5));
        assert_eq!(decode_smc(fourcc("ui16"), &[0x01, 0x00]), Some(256.0));
        assert_eq!(decode_smc(fourcc("flt "), &[1, 2]), None);
        assert_eq!(decode_smc(fourcc("ch8*"), &[1, 2, 3, 4]), None);
    }
    #[test]
    fn clean_for_display() {
        assert_eq!(display_clean("U\u{308}bersicht"), "Übersicht");
        assert_eq!(display_clean("cafe\u{301} n\u{303}"), "café ñ");
        assert_eq!(display_clean("a\u{200b}b\u{fe0f}c\u{20dd}"), "abc");
        assert_eq!(display_clean("ä\u{308}"), "ä"); // no base to compose with: mark dropped
        assert_eq!(display_clean("plain"), "plain");
        assert!(COMPOSE.windows(2).all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1)));
    }
    #[test]
    fn root_user() {
        assert_eq!(lookup_user(0).as_deref(), Some("root"));
    }
}
