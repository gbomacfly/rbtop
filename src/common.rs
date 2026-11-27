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
    fn root_user() {
        assert_eq!(lookup_user(0).as_deref(), Some("root"));
    }
}
