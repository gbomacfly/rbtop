//! Process collection from /proc. Hot path reads only /proc/<pid>/stat per
//! process per tick; cmdline, uid and cgroup are read once per new pid (or on exec).

use crate::collect::{push, read_to, HIST};
use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub name: String,
    pub cmd: String,
    pub uid: u32,
    pub state: char,
    pub threads: u32,
    pub rss: u64,
    pub mem_pct: f32,
    pub cpu: f32,
    pub cpu_lazy: f32,
    pub mini: [u8; 8],
    pub nice: i32,
    pub start: u64,
    pub container: Option<String>,
    ticks: u64,
    gen: u32,
}

pub struct Row {
    pub pid: i32,
    pub prefix: String,
}

pub struct Detail {
    pub pid: i32,
    pub hist: VecDeque<f32>,
    pub mem_hist: VecDeque<f32>,
    pub alive: bool,
    pub io_read: Option<u64>,
    pub io_write: Option<u64>,
    pub io_rps: f64,
    pub io_wps: f64,
    io_prev: Option<(u64, u64, Instant)>,
    pub name: String,
}

pub struct Procs {
    pub map: HashMap<i32, Proc>,
    pub rows: Vec<Row>,
    pub detail: Option<Detail>,
    users: HashMap<u32, String>,
    ct_names: HashMap<String, String>,
    gen: u32,
    last: Instant,
    clk_tck: f64,
    page: u64,
    buf: String,
    path: String,
    raw: Vec<u8>,
    proc_fd: libc::c_int,
    pub collect_us: u128,
}

fn load_users() -> HashMap<u32, String> {
    let mut m = HashMap::new();
    if let Ok(s) = std::fs::read_to_string("/etc/passwd") {
        for l in s.lines() {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() > 2 {
                if let Ok(uid) = f[2].parse() {
                    m.entry(uid).or_insert_with(|| f[0].to_string());
                }
            }
        }
    }
    m
}

impl Procs {
    pub fn new() -> Procs {
        Procs {
            map: HashMap::with_capacity(1024),
            rows: Vec::with_capacity(1024),
            detail: None,
            users: load_users(),
            ct_names: HashMap::new(),
            gen: 0,
            last: Instant::now(),
            clk_tck: unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64,
            page: unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64,
            buf: String::with_capacity(1024),
            path: String::with_capacity(64),
            raw: vec![0u8; 2048],
            proc_fd: unsafe { libc::open(c"/proc".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) },
            collect_us: 0,
        }
    }

    pub fn clk_tck(&self) -> f64 {
        self.clk_tck
    }

    pub fn user(&self, uid: u32) -> String {
        self.users.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
    }

    pub fn collect(&mut self, threads: usize, mem_total: u64, per_core: bool, containers: bool) {
        let t0 = Instant::now();
        let dt = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();
        self.gen = self.gen.wrapping_add(1);
        let gen = self.gen;
        let Ok(rd) = std::fs::read_dir("/proc") else { return };
        let div = if per_core { 1.0 } else { threads.max(1) as f64 };
        for e in rd.flatten() {
            let fname = e.file_name();
            let Some(pid) = fname.to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
            // openat() relative to the /proc dirfd: no full path walk per process
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
            let mut f = stat[r + 1..].split_ascii_whitespace();
            let state = f.next().and_then(|s| s.chars().next()).unwrap_or('?');
            let ppid: i32 = f.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            // fields after ppid: pgrp(0) session tty tpgid flags minflt cminflt majflt cmajflt utime(9) stime(10) cutime cstime prio nice(14) threads(15) itreal start(17) vsize rss(19)
            let mut vals = [0i64; 20];
            for v in vals.iter_mut() {
                *v = f.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            }
            let ticks = (vals[9] + vals[10]) as u64;
            let nice = vals[14] as i32;
            let nthreads = vals[15] as u32;
            let start = vals[17] as u64;
            let rss = vals[19].max(0) as u64 * self.page;

            let entry = self.map.get_mut(&pid);
            match entry {
                Some(p) if p.start == start => {
                    let dticks = ticks.saturating_sub(p.ticks) as f64;
                    let c = ((dticks / (dt * self.clk_tck)) * 100.0 / div) as f32;
                    p.cpu = c.max(0.0);
                    p.cpu_lazy = p.cpu_lazy * 0.7 + p.cpu * 0.3;
                    p.mini.rotate_left(1);
                    p.mini[7] = p.cpu.min(100.0) as u8;
                    p.ticks = ticks;
                    p.state = state;
                    p.ppid = ppid;
                    p.threads = nthreads;
                    p.rss = rss;
                    p.nice = nice;
                    p.mem_pct = (rss as f64 * 100.0 / mem_total as f64) as f32;
                    p.gen = gen;
                    if p.name != name {
                        // exec happened
                        p.name = name.to_string();
                        p.cmd = read_cmdline(pid);
                    }
                }
                _ => {
                    let name = name.to_string();
                    let uid =std::fs::metadata(format!("/proc/{pid}")).map(|m| m.uid()).unwrap_or(0);
                    let container = if containers { self.container_of(pid) } else { None };
                    let p = Proc {
                        pid,
                        ppid,
                        name,
                        cmd: read_cmdline(pid),
                        uid,
                        state,
                        threads: nthreads,
                        rss,
                        mem_pct: (rss as f64 * 100.0 / mem_total as f64) as f32,
                        cpu: 0.0,
                        cpu_lazy: 0.0,
                        mini: [0; 8],
                        nice,
                        start,
                        container,
                        ticks,
                        gen,
                    };
                    self.map.insert(pid, p);
                }
            }
        }
        self.map.retain(|_, p| p.gen == gen);

        // detailed process
        if let Some(d) = self.detail.as_mut() {
            match self.map.get(&d.pid) {
                Some(p) => {
                    d.alive = true;
                    push(&mut d.hist, p.cpu.min(100.0));
                    push(&mut d.mem_hist, p.mem_pct);
                    d.name = p.name.clone();
                    if read_to(&format!("/proc/{}/io", d.pid), &mut self.buf) {
                        let (mut r, mut w) = (None, None);
                        for l in self.buf.lines() {
                            if let Some(v) = l.strip_prefix("read_bytes: ") {
                                r = v.trim().parse().ok();
                            } else if let Some(v) = l.strip_prefix("write_bytes: ") {
                                w = v.trim().parse().ok();
                            }
                        }
                        d.io_read = r;
                        d.io_write = w;
                        if let (Some(r), Some(w)) = (r, w) {
                            if let Some((pr, pw, t)) = d.io_prev {
                                let s = t.elapsed().as_secs_f64().max(0.001);
                                d.io_rps = r.saturating_sub(pr) as f64 / s;
                                d.io_wps = w.saturating_sub(pw) as f64 / s;
                            }
                            d.io_prev = Some((r, w, Instant::now()));
                        }
                    }
                }
                None => d.alive = false,
            }
        }
        self.collect_us = t0.elapsed().as_micros();
    }

    pub fn set_detail(&mut self, pid: Option<i32>) {
        self.detail = pid.map(|pid| {
            let name = self.map.get(&pid).map(|p| p.name.clone()).unwrap_or_default();
            Detail {
                pid,
                hist: VecDeque::with_capacity(HIST),
                mem_hist: VecDeque::with_capacity(HIST),
                alive: true,
                io_read: None,
                io_write: None,
                io_rps: 0.0,
                io_wps: 0.0,
                io_prev: None,
                name,
            }
        });
    }

    fn container_of(&mut self, pid: i32) -> Option<String> {
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

    /// Build display rows (filter, sort, optional tree).
    pub fn build_rows(&mut self, sorting: &str, reverse: bool, tree: bool, filter: &str) {
        let f = filter.to_lowercase();
        let matches = |p: &Proc, users: &HashMap<u32, String>| -> bool {
            if f.is_empty() {
                return true;
            }
            p.pid.to_string().contains(&f)
                || p.name.to_lowercase().contains(&f)
                || p.cmd.to_lowercase().contains(&f)
                || users.get(&p.uid).map_or(false, |u| u.to_lowercase().contains(&f))
                || p.container.as_ref().map_or(false, |c| c.to_lowercase().contains(&f))
        };
        let users = &self.users;
        let map = &self.map;
        let cmp = |a: &i32, b: &i32| -> std::cmp::Ordering {
            let (pa, pb) = (&map[a], &map[b]);
            use std::cmp::Ordering::*;
            let o = match sorting {
                "pid" => pa.pid.cmp(&pb.pid),
                "program" => pa.name.to_lowercase().cmp(&pb.name.to_lowercase()),
                "arguments" => pa.cmd.cmp(&pb.cmd),
                "threads" => pb.threads.cmp(&pa.threads),
                "user" => users.get(&pa.uid).cmp(&users.get(&pb.uid)),
                "memory" => pb.rss.cmp(&pa.rss),
                "cpu responsive" => pb.cpu.partial_cmp(&pa.cpu).unwrap_or(Equal),
                _ => pb.cpu_lazy.partial_cmp(&pa.cpu_lazy).unwrap_or(Equal),
            };
            let o = if o == Equal { pa.pid.cmp(&pb.pid) } else { o };
            if reverse { o.reverse() } else { o }
        };
        self.rows.clear();
        if !tree {
            let mut pids: Vec<i32> = map.values().filter(|p| matches(p, users)).map(|p| p.pid).collect();
            pids.sort_unstable_by(cmp);
            self.rows.extend(pids.into_iter().map(|pid| Row { pid, prefix: String::new() }));
            return;
        }
        // tree view
        let visible: Option<HashSet<i32>> = if f.is_empty() {
            None
        } else {
            let mut set = HashSet::new();
            for p in map.values().filter(|p| matches(p, users)) {
                let mut cur = p.pid;
                while set.insert(cur) {
                    match map.get(&cur) {
                        Some(pp) if map.contains_key(&pp.ppid) => cur = pp.ppid,
                        _ => break,
                    }
                }
            }
            Some(set)
        };
        let mut children: HashMap<i32, Vec<i32>> = HashMap::with_capacity(map.len());
        let mut roots = Vec::new();
        for p in map.values() {
            if let Some(v) = &visible {
                if !v.contains(&p.pid) {
                    continue;
                }
            }
            if p.ppid != p.pid && map.contains_key(&p.ppid) {
                children.entry(p.ppid).or_default().push(p.pid);
            } else {
                roots.push(p.pid);
            }
        }
        roots.sort_unstable_by(cmp);
        for v in children.values_mut() {
            v.sort_unstable_by(cmp);
        }
        let mut stack: Vec<(i32, String, bool, bool)> = roots.iter().rev().enumerate().map(|(i, &p)| (p, String::new(), i == 0, true)).collect();
        while let Some((pid, indent, last, root)) = stack.pop() {
            let prefix = if root { String::new() } else { format!("{indent}{}", if last { "└─" } else { "├─" }) };
            let child_indent = if root { String::new() } else { format!("{indent}{}", if last { "  " } else { "│ " }) };
            if let Some(ch) = children.get(&pid) {
                for (i, c) in ch.iter().enumerate().rev() {
                    stack.push((*c, child_indent.clone(), i == ch.len() - 1, false));
                }
            }
            self.rows.push(Row { pid, prefix });
        }
    }

    /// All descendants of pid (for signalling a whole tree, issue #185).
    pub fn descendants(&self, pid: i32) -> Vec<i32> {
        let mut out = Vec::new();
        let mut todo = vec![pid];
        while let Some(p) = todo.pop() {
            for c in self.map.values().filter(|x| x.ppid == p && x.pid != p) {
                out.push(c.pid);
                todo.push(c.pid);
            }
        }
        out
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

fn read_cmdline(pid: i32) -> String {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(mut b) => {
            while b.last() == Some(&0) {
                b.pop();
            }
            for c in b.iter_mut() {
                if *c == 0 || *c == b'\n' {
                    *c = b' ';
                }
            }
            String::from_utf8_lossy(&b).into_owned()
        }
        Err(_) => String::new(),
    }
}

/// Fresh process state straight from /proc (for the pause/resume toggle).
pub fn read_state(pid: i32) -> Option<char> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    s[s.rfind(')')? + 1..].split_ascii_whitespace().next()?.chars().next()
}

pub fn send_signal(pid: i32, sig: i32) -> Result<(), String> {
    let r = unsafe { libc::kill(pid, sig) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}
