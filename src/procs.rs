//! Process list. Platform-independent part: cpu % (responsive + lazy), tree,
//! filter and sorting. Reading the raw per-process values is platform code in
//! procs/linux.rs (/proc) and procs/macos.rs (libproc + sysctl).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

use crate::common::{display_clean, lookup_user, push, HIST};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub name: String,
    pub cmd: String,
    /// byte offset in `cmd` where the arguments start (after argv[0])
    pub args_at: usize,
    pub uid: u32,
    pub state: char,
    pub threads: u32,
    pub rss: u64,
    pub mem_pct: f32,
    pub cpu: f32,
    pub cpu_lazy: f32,
    pub mini: [u8; 8],
    pub nice: i32,
    /// start time in clk_tck units since boot
    pub start: u64,
    pub container: Option<String>,
    ticks: u64,
    gen: u32,
}

/// One raw reading of a process, borrowed from the platform sampler's buffers.
pub struct Sample<'a> {
    pub pid: i32,
    pub ppid: i32,
    pub name: &'a str,
    pub state: char,
    /// cpu time (user+system) in clk_tck units
    pub ticks: u64,
    pub nice: i32,
    pub threads: u32,
    pub start: u64,
    pub rss: u64,
    /// set if the platform gets the uid for free with the sample
    pub uid: Option<u32>,
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
    #[allow(dead_code)]
    ct_names: HashMap<String, String>,
    gen: u32,
    last: Instant,
    clk_tck: f64,
    sampler: platform::Sampler,
    pub collect_us: u128,
}

impl Procs {
    pub fn new() -> Procs {
        Procs {
            map: HashMap::with_capacity(1024),
            rows: Vec::with_capacity(1024),
            detail: None,
            users: HashMap::new(),
            ct_names: HashMap::new(),
            gen: 0,
            last: Instant::now(),
            clk_tck: platform::clk_tck(),
            sampler: platform::Sampler::default(),
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
        let div = if per_core { 1.0 } else { threads.max(1) as f64 };
        let mem_total = mem_total.max(1);
        // The sampler is moved out so its buffers can be borrowed by the samples
        // while the process map is updated.
        let mut sampler = std::mem::take(&mut self.sampler);
        sampler.each(|s| self.upsert(&s, dt, div, mem_total, containers, gen));
        self.sampler = sampler;
        self.map.retain(|_, p| p.gen == gen);

        // detailed process
        if let Some(d) = self.detail.as_mut() {
            match self.map.get(&d.pid) {
                Some(p) => {
                    d.alive = true;
                    push(&mut d.hist, p.cpu.min(100.0));
                    push(&mut d.mem_hist, p.mem_pct);
                    d.name = p.name.clone();
                    match platform::io_bytes(d.pid) {
                        Some((r, w)) => {
                            d.io_read = Some(r);
                            d.io_write = Some(w);
                            if let Some((pr, pw, t)) = d.io_prev {
                                let s = t.elapsed().as_secs_f64().max(0.001);
                                d.io_rps = r.saturating_sub(pr) as f64 / s;
                                d.io_wps = w.saturating_sub(pw) as f64 / s;
                            }
                            d.io_prev = Some((r, w, Instant::now()));
                        }
                        None => {
                            d.io_read = None;
                            d.io_write = None;
                        }
                    }
                }
                None => d.alive = false,
            }
        }
        self.collect_us = t0.elapsed().as_micros();
    }

    fn upsert(&mut self, s: &Sample, dt: f64, div: f64, mem_total: u64, containers: bool, gen: u32) {
        let mem_pct = (s.rss as f64 * 100.0 / mem_total as f64) as f32;
        match self.map.get_mut(&s.pid) {
            Some(p) if p.start == s.start => {
                let dticks = s.ticks.saturating_sub(p.ticks) as f64;
                let c = ((dticks / (dt * self.clk_tck)) * 100.0 / div) as f32;
                p.cpu = c.max(0.0);
                p.cpu_lazy = p.cpu_lazy * 0.7 + p.cpu * 0.3;
                p.mini.rotate_left(1);
                p.mini[7] = p.cpu.min(100.0) as u8;
                p.ticks = s.ticks;
                p.state = s.state;
                p.ppid = s.ppid;
                p.threads = s.threads;
                p.rss = s.rss;
                p.nice = s.nice;
                p.mem_pct = mem_pct;
                p.gen = gen;
                let name = display_clean(s.name);
                if p.name != *name {
                    // exec happened
                    p.name = name.into_owned();
                    (p.cmd, p.args_at) = cmdline(s.pid);
                }
            }
            _ => {
                let uid = s.uid.unwrap_or_else(|| platform::uid_of(s.pid));
                self.users.entry(uid).or_insert_with(|| lookup_user(uid).unwrap_or_else(|| uid.to_string()));
                let container = if containers { self.container_of(s.pid) } else { None };
                let (cmd, args_at) = cmdline(s.pid);
                let p = Proc {
                    pid: s.pid,
                    ppid: s.ppid,
                    name: display_clean(s.name).into_owned(),
                    cmd,
                    args_at,
                    uid,
                    state: s.state,
                    threads: s.threads,
                    rss: s.rss,
                    mem_pct,
                    cpu: 0.0,
                    cpu_lazy: 0.0,
                    mini: [0; 8],
                    nice: s.nice,
                    start: s.start,
                    container,
                    ticks: s.ticks,
                    gen,
                };
                self.map.insert(s.pid, p);
            }
        }
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
                || users.get(&p.uid).is_some_and(|u| u.to_lowercase().contains(&f))
                || p.container.as_ref().is_some_and(|c| c.to_lowercase().contains(&f))
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
    /// One pass to index children, then a walk with a visited set: ppid
    /// values are read at different times, so a racy cycle is possible.
    pub fn descendants(&self, pid: i32) -> Vec<i32> {
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        for p in self.map.values() {
            if p.pid != p.ppid {
                children.entry(p.ppid).or_default().push(p.pid);
            }
        }
        let mut seen = HashSet::from([pid]);
        let mut out = Vec::new();
        let mut todo = vec![pid];
        while let Some(p) = todo.pop() {
            for &c in children.get(&p).map(Vec::as_slice).unwrap_or(&[]) {
                if seen.insert(c) {
                    out.push(c);
                    todo.push(c);
                }
            }
        }
        out
    }
}

/// Command line cleaned for display; program and arguments are cleaned
/// separately so the args offset stays a valid byte index.
fn cmdline(pid: i32) -> (String, usize) {
    let (cmd, at) = platform::cmdline(pid);
    if cmd.is_ascii() {
        return (cmd, at);
    }
    let at = if cmd.is_char_boundary(at) { at } else { 0 };
    let (prog, args) = cmd.split_at(at);
    let mut out = display_clean(prog).into_owned();
    let at = out.len();
    out.push_str(&display_clean(args));
    (out, at)
}

/// Fresh start time of a process (same unit as `Proc::start`). Compared
/// right before a signal is sent, so a pid that was reused in the meantime
/// by another process is not hit.
pub fn start_time(pid: i32) -> Option<u64> {
    platform::start_time(pid)
}

/// Fresh process state (for the pause/resume toggle).
pub fn read_state(pid: i32) -> Option<char> {
    platform::read_state(pid)
}

/// PID 0 (macOS kernel_task; kill(0, ..) would hit rbtop's own process group)
/// and PID 1 (init/systemd/launchd, killing it takes the system down) are
/// never signalled, and neither are negative pids (process groups).
pub fn signal_allowed(pid: i32) -> bool {
    pid > 1
}

pub fn send_signal(pid: i32, sig: i32) -> Result<(), String> {
    if !signal_allowed(pid) {
        return Err(format!("refused for pid {pid}"));
    }
    let r = unsafe { libc::kill(pid, sig) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(test)]
mod signal_tests {
    use super::*;
    #[test]
    fn kernel_and_init_are_protected() {
        // signal 0 only checks permissions, so this is harmless even if the guard failed
        for pid in [-1, 0, 1] {
            assert!(!signal_allowed(pid));
            assert!(send_signal(pid, 0).is_err());
        }
        assert!(send_signal(std::process::id() as i32, 0).is_ok());
    }
    #[test]
    fn own_start_time_is_stable() {
        let me = std::process::id() as i32;
        let a = start_time(me).expect("own start time");
        assert_eq!(start_time(me), Some(a));
        assert_eq!(start_time(i32::MAX), None);
        // same unit as the sampler, so the kill dialog comparison holds
        let mut ps = Procs::new();
        ps.collect(1, 1 << 30, false, false);
        assert_eq!(ps.map.get(&me).map(|p| p.start), Some(a));
    }
    #[test]
    fn descendants_survive_cycles() {
        let mut ps = Procs::new();
        ps.map.clear();
        let mk = |pid, ppid| Proc { pid, ppid, name: String::new(), cmd: String::new(), args_at: 0, uid: 0, state: 'S', threads: 1, rss: 0, mem_pct: 0.0, cpu: 0.0, cpu_lazy: 0.0, mini: [0; 8], nice: 0, start: 0, container: None, ticks: 0, gen: 0 };
        // 10 -> 11 -> 12 -> 10 (racy cycle), 11 -> 13
        for (pid, ppid) in [(10, 12), (11, 10), (12, 11), (13, 11), (20, 1)] {
            ps.map.insert(pid, mk(pid, ppid));
        }
        let mut d = ps.descendants(10);
        d.sort();
        assert_eq!(d, vec![11, 12, 13]);
        assert!(ps.descendants(13).is_empty());
    }
}
