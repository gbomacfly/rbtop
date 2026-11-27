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

/// Parse a macOS KERN_PROCARGS2 buffer into a command line:
/// [argc: i32][exec path\0][\0 padding][argv0\0 argv1\0 ...][env ...]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_procargs2(data: &[u8]) -> String {
    if data.len() < 4 {
        return String::new();
    }
    let argc = i32::from_ne_bytes([data[0], data[1], data[2], data[3]]).max(0) as usize;
    let mut i = 4;
    while i < data.len() && data[i] != 0 {
        i += 1; // exec path
    }
    while i < data.len() && data[i] == 0 {
        i += 1; // padding
    }
    let mut out = String::new();
    let mut n = 0;
    while n < argc && i < data.len() {
        let s = i;
        while i < data.len() && data[i] != 0 {
            i += 1;
        }
        if n > 0 {
            out.push(' ');
        }
        out.push_str(&String::from_utf8_lossy(&data[s..i]));
        i += 1;
        n += 1;
    }
    out.replace('\n', " ")
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
        assert_eq!(parse_procargs2(&b), "python3 -m http.server");
        let mut b = 1i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/sbin/launchd\0\0/sbin/launchd\0");
        assert_eq!(parse_procargs2(&b), "/sbin/launchd");
        assert_eq!(parse_procargs2(&[1, 0]), "");
        let mut b = 5i32.to_ne_bytes().to_vec(); // truncated buffer must not panic
        b.extend_from_slice(b"/bin/x\0\0a\0b");
        assert_eq!(parse_procargs2(&b), "a b");
    }
    #[test]
    fn root_user() {
        assert_eq!(lookup_user(0).as_deref(), Some("root"));
    }
}
