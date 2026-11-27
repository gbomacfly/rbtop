//! Linux process sampling from /proc. Hot path: one openat()+read() of
//! /proc/<pid>/stat per process per tick, relative to an open /proc dirfd.

use super::Sample;
use crate::common::read_to;
use std::os::unix::fs::MetadataExt;

pub struct Sampler {
    proc_fd: libc::c_int,
    raw: Vec<u8>,
    path: String,
    page: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        Sampler { proc_fd: -1, raw: vec![0u8; 2048], path: String::with_capacity(32), page: unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64 }
    }
}

pub fn clk_tck() -> f64 {
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64
}

impl Sampler {
    pub fn each<F: FnMut(Sample)>(&mut self, mut f: F) {
        if self.proc_fd < 0 {
            self.proc_fd = unsafe { libc::open(c"/proc".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        }
        let Ok(rd) = std::fs::read_dir("/proc") else { return };
        for e in rd.flatten() {
            let fname = e.file_name();
            let Some(pid) = fname.to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
            self.path.clear();
            use std::fmt::Write;
            let _ = write!(self.path, "{pid}/stat\0");
            let n = unsafe {
                let fd = libc::openat(self.proc_fd, self.path.as_ptr() as *const libc::c_char, libc::O_RDONLY | libc::O_CLOEXEC);
                if fd < 0 {
                    continue;
                }
                let n = libc::read(fd, self.raw.as_mut_ptr() as *mut libc::c_void, self.raw.len());
                libc::close(fd);
                n
            };
            if n <= 0 {
                continue;
            }
            let Ok(stat) = std::str::from_utf8(&self.raw[..n as usize]) else { continue };
            let (Some(l), Some(r)) = (stat.find('('), stat.rfind(')')) else { continue };
            let name = &stat[l + 1..r];
            let mut it = stat[r + 1..].split_ascii_whitespace();
            let state = it.next().and_then(|s| s.chars().next()).unwrap_or('?');
            let ppid: i32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            // fields after ppid: pgrp(0) session tty tpgid flags minflt cminflt majflt cmajflt
            // utime(9) stime(10) cutime cstime prio nice(14) threads(15) itreal start(17) vsize rss(19)
            let mut v = [0i64; 20];
            for x in v.iter_mut() {
                *x = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            }
            f(Sample {
                pid,
                ppid,
                name,
                state,
                ticks: (v[9] + v[10]).max(0) as u64,
                nice: v[14] as i32,
                threads: v[15] as u32,
                start: v[17].max(0) as u64,
                rss: v[19].max(0) as u64 * self.page,
                uid: None,
            });
        }
    }
}

pub fn uid_of(pid: i32) -> u32 {
    std::fs::metadata(format!("/proc/{pid}")).map(|m| m.uid()).unwrap_or(0)
}

/// Command line and the offset where the arguments start (after argv[0]).
pub fn cmdline(pid: i32) -> (String, usize) {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(mut b) => {
            while b.last() == Some(&0) {
                b.pop();
            }
            if b.is_empty() {
                return (String::new(), 0);
            }
            crate::common::join_argv(b.split(|&c| c == 0))
        }
        Err(_) => (String::new(), 0),
    }
}

pub fn io_bytes(pid: i32) -> Option<(u64, u64)> {
    let mut buf = String::new();
    if !read_to(&format!("/proc/{pid}/io"), &mut buf) {
        return None;
    }
    let (mut r, mut w) = (None, None);
    for l in buf.lines() {
        if let Some(v) = l.strip_prefix("read_bytes: ") {
            r = v.trim().parse().ok();
        } else if let Some(v) = l.strip_prefix("write_bytes: ") {
            w = v.trim().parse().ok();
        }
    }
    Some((r?, w?))
}

pub fn read_state(pid: i32) -> Option<char> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    s[s.rfind(')')? + 1..].split_ascii_whitespace().next()?.chars().next()
}

// ------------------------------------------------------------ containers (#97)

impl super::Procs {
    pub(super) fn container_of(&mut self, pid: i32) -> Option<String> {
        let mut s = String::new();
        if !read_to(&format!("/proc/{pid}/cgroup"), &mut s) {
            return None;
        }
        match parse_cgroup(&s)? {
            Ct::Docker(id) => Some(self.docker_name(&id)),
            Ct::Podman(id) => Some(format!("podman:{}", &id[..12])),
            Ct::Lxc(id) => Some(self.pve_name("lxc", &id, "hostname:", "CT")),
            Ct::Qemu(id) => Some(self.pve_name("qemu-server", &id, "name:", "VM")),
            Ct::Other(n) => Some(n.to_string()),
        }
    }

    fn docker_name(&mut self, id: &str) -> String {
        if let Some(n) = self.ct_names.get(id) {
            return n.clone();
        }
        let mut name = format!("docker:{}", &id[..12]);
        if let Ok(js) = std::fs::read_to_string(format!("/var/lib/docker/containers/{id}/config.v2.json")) {
            if let Some(i) = js.find("\"Name\":\"/") {
                let rest = &js[i + 9..];
                if let Some(e) = rest.find('"') {
                    name = format!("docker:{}", &rest[..e]);
                }
            }
        }
        self.ct_names.insert(id.to_string(), name.clone());
        name
    }

    fn pve_name(&mut self, kind: &str, id: &str, key: &str, label: &str) -> String {
        let k = format!("{kind}/{id}");
        if let Some(n) = self.ct_names.get(&k) {
            return n.clone();
        }
        let mut name = format!("{label} {id}");
        if let Ok(conf) = std::fs::read_to_string(format!("/etc/pve/{kind}/{id}.conf")) {
            if let Some(l) = conf.lines().find(|l| l.starts_with(key)) {
                name = format!("{label} {id} ({})", l[key.len()..].trim());
            }
        }
        self.ct_names.insert(k, name.clone());
        name
    }

}

#[derive(Debug, PartialEq)]
pub enum Ct {
    Docker(String),
    Podman(String),
    Lxc(String),
    Qemu(String),
    Other(&'static str),
}

/// Map a /proc/<pid>/cgroup file to a container (issue #97).
pub fn parse_cgroup(s: &str) -> Option<Ct> {
    let hexid = |t: &str| -> Option<String> {
        let t: String = t.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        (t.len() >= 12).then_some(t)
    };
    for line in s.lines() {
        let path = line.splitn(3, ':').nth(2).unwrap_or("");
        for pre in ["docker-", "/docker/"] {
            if let Some(i) = path.find(pre) {
                if let Some(id) = hexid(&path[i + pre.len()..]) {
                    return Some(Ct::Docker(id));
                }
            }
        }
        if let Some(i) = path.find("libpod-") {
            if let Some(id) = hexid(&path[i + 7..]) {
                return Some(Ct::Podman(id));
            }
        }
        // Proxmox / LXC: /lxc/101/... or /lxc.payload.101/...
        for pre in ["/lxc.payload.", "/lxc/"] {
            if let Some(i) = path.find(pre) {
                let id: String = path[i + pre.len()..].chars().take_while(|c| *c != '/').collect();
                if !id.is_empty() {
                    return Some(Ct::Lxc(id));
                }
            }
        }
        if let Some(i) = path.find("qemu.slice/") {
            let id: String = path[i + 11..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if !id.is_empty() {
                return Some(Ct::Qemu(id));
            }
        }
        if path.contains("machine-qemu") {
            return Some(Ct::Other("VM (libvirt)"));
        }
        if path.contains("/kubepods") {
            return Some(Ct::Other("k8s"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "4f2a9c1d8e7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3b2a1f";
    #[test]
    fn cgroups() {
        assert_eq!(parse_cgroup(&format!("0::/system.slice/docker-{ID}.scope\n")), Some(Ct::Docker(ID.into())));
        assert_eq!(parse_cgroup(&format!("12:pids:/docker/{ID}\n")), Some(Ct::Docker(ID.into())));
        assert_eq!(parse_cgroup(&format!("0::/machine.slice/libpod-{ID}.scope/container\n")), Some(Ct::Podman(ID.into())));
        assert_eq!(parse_cgroup("0::/lxc/101/ns/system.slice/nginx.service\n"), Some(Ct::Lxc("101".into())));
        assert_eq!(parse_cgroup("0::/lxc.payload.204/init.scope\n"), Some(Ct::Lxc("204".into())));
        assert_eq!(parse_cgroup("0::/qemu.slice/105.scope\n"), Some(Ct::Qemu("105".into())));
        assert_eq!(parse_cgroup("0::/user.slice/user-1000.slice/session-3.scope\n"), None);
        assert_eq!(parse_cgroup("0::/init.scope\n"), None);
    }
}

