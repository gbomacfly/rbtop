//! Linux system collectors: cpu, sensors, memory/zram, disks (incl. ZFS io), network,
//! battery, gpu. Everything reads /proc and /sys directly with reused buffers;
//! no external commands except an optional single long-running nvidia-smi.

pub use crate::common::{push, read_num, read_str, read_to, shorten_model, HIST};
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufRead;
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ======================================================================= CPU

#[derive(Default)]
struct TempSrc {
    pkg: Option<String>,
    cores: Vec<(usize, String)>, // core_id -> path
    crit: f32,
}

pub struct Cpu {
    pub threads: usize,
    pub model: String,
    prev: Vec<(u64, u64)>,
    pub usage: Vec<f32>,            // [0] = total, [1..] = threads
    pub hist: Vec<VecDeque<f32>>,   // same indexing
    pub freq: Option<f32>,          // GHz
    freq_paths: Vec<String>,
    pub load: [f32; 3],
    pub uptime: f64,
    temps: TempSrc,
    thread_core: Vec<usize>,
    pub temp: Option<f32>,
    pub temp_crit: f32,
    pub core_temps: Vec<Option<f32>>, // per thread
    pub temp_hist: Vec<VecDeque<f32>>, // [0] pkg, [1..] per thread (percent of crit)
    pub jiffies_total: u64,             // sum of all-cpu jiffies delta (for proc %)
    buf: String,
}

impl Cpu {
    pub fn new(custom_name: &str) -> Cpu {
        let mut threads = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) }.max(1) as usize;
        // layout testing only: pretend to have more threads
        if let Some(t) = std::env::var("RBTOP_TEST_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
            threads = t.clamp(1, 1024);
        }
        let mut model = String::from("cpu");
        if let Ok(s) = std::fs::read_to_string("/proc/cpuinfo") {
            for l in s.lines() {
                if l.starts_with("model name") || l.starts_with("Model") || l.starts_with("Hardware") {
                    if let Some((_, v)) = l.split_once(':') {
                        model = shorten_model(v.trim());
                        break;
                    }
                }
            }
        }
        if !custom_name.is_empty() {
            model = custom_name.to_string();
        }
        let mut freq_paths = Vec::new();
        for i in 0..threads {
            let p = format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq");
            if std::path::Path::new(&p).exists() {
                freq_paths.push(p);
            }
        }
        let thread_core = (0..threads)
            .map(|i| read_num(&format!("/sys/devices/system/cpu/cpu{i}/topology/core_id")).map(|v| v as usize).unwrap_or(i))
            .collect();
        let mut c = Cpu {
            threads,
            model,
            prev: vec![(0, 0); threads + 1],
            usage: vec![0.0; threads + 1],
            hist: vec![VecDeque::with_capacity(HIST); threads + 1],
            freq: None,
            freq_paths,
            load: [0.0; 3],
            uptime: 0.0,
            temps: TempSrc::default(),
            thread_core,
            temp: None,
            temp_crit: 95.0,
            core_temps: vec![None; threads],
            temp_hist: vec![VecDeque::with_capacity(HIST); threads + 1],
            jiffies_total: 0,
            buf: String::with_capacity(8192),
        };
        c.find_temps();
        c.collect(true);
        c
    }

    fn find_temps(&mut self) {
        let mut ts = TempSrc { crit: 95.0, ..Default::default() };
        let mut fallback: Option<String> = None;
        if let Ok(rd) = std::fs::read_dir("/sys/class/hwmon") {
            let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            dirs.sort();
            for d in dirs {
                let name = read_str(&format!("{}/name", d.display())).unwrap_or_default();
                let mut inputs: Vec<(String, String)> = Vec::new();
                if let Ok(files) = std::fs::read_dir(&d) {
                    for f in files.flatten() {
                        let fname = f.file_name().to_string_lossy().to_string();
                        if fname.starts_with("temp") && fname.ends_with("_input") {
                            let base = fname.trim_end_matches("_input").to_string();
                            let label = read_str(&format!("{}/{}_label", d.display(), base)).unwrap_or_default();
                            inputs.push((label, format!("{}/{}", d.display(), fname)));
                        }
                    }
                }
                inputs.sort();
                match name.as_str() {
                    "coretemp" => {
                        for (label, p) in &inputs {
                            if label.starts_with("Package id") || label.starts_with("Physical id") {
                                if ts.pkg.is_none() {
                                    ts.pkg = Some(p.clone());
                                    if let Some(c) = read_num(&p.replace("_input", "_crit")) {
                                        ts.crit = (c / 1000.0) as f32;
                                    }
                                }
                            } else if let Some(n) = label.strip_prefix("Core ") {
                                if let Ok(id) = n.trim().parse::<usize>() {
                                    ts.cores.push((id, p.clone()));
                                }
                            }
                        }
                    }
                    "k10temp" | "zenpower" => {
                        // Prefer Tdie, then Tctl (issue #48)
                        let pick = inputs.iter().find(|(l, _)| l == "Tdie").or_else(|| inputs.iter().find(|(l, _)| l == "Tctl")).or(inputs.first());
                        if let Some((_, p)) = pick {
                            ts.pkg = Some(p.clone());
                        }
                    }
                    "cpu_thermal" | "soc_thermal" | "cpu-thermal" | "scpi_sensors" => {
                        if ts.pkg.is_none() {
                            ts.pkg = inputs.first().map(|x| x.1.clone());
                        }
                    }
                    "acpitz" => {
                        if fallback.is_none() {
                            fallback = inputs.first().map(|x| x.1.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        if ts.pkg.is_none() {
            if let Ok(rd) = std::fs::read_dir("/sys/class/thermal") {
                for e in rd.flatten() {
                    let p = e.path();
                    if read_str(&format!("{}/type", p.display())).as_deref() == Some("x86_pkg_temp") {
                        ts.pkg = Some(format!("{}/temp", p.display()));
                    }
                }
            }
        }
        if ts.pkg.is_none() {
            ts.pkg = fallback;
        }
        self.temps = ts;
        self.temp_crit = self.temps.crit;
    }

    pub fn collect(&mut self, check_temp: bool) {
        if read_to("/proc/stat", &mut self.buf) {
            let mut idx = 0;
            let mut total_delta = 0;
            for line in self.buf.lines() {
                if !line.starts_with("cpu") {
                    break;
                }
                if idx > self.threads {
                    break;
                }
                let mut it = line.split_ascii_whitespace();
                let name = it.next().unwrap_or("");
                let i = if name == "cpu" { 0 } else { name[3..].parse::<usize>().map(|n| n + 1).unwrap_or(idx) };
                if i > self.threads {
                    continue;
                }
                let v: [u64; 8] = std::array::from_fn(|_| it.next().and_then(|x| x.parse().ok()).unwrap_or(0));
                let total: u64 = v.iter().sum();
                let idle = v[3] + v[4];
                let (pt, pi) = self.prev[i];
                let dt = total.saturating_sub(pt);
                let di = idle.saturating_sub(pi);
                let pct = if dt > 0 && pt > 0 { (100.0 * (dt - di.min(dt)) as f32 / dt as f32).clamp(0.0, 100.0) } else { 0.0 };
                if i == 0 {
                    total_delta = dt;
                }
                self.prev[i] = (total, idle);
                self.usage[i] = pct;
                push(&mut self.hist[i], pct);
                idx += 1;
            }
            self.jiffies_total = total_delta;
        }
        if !self.freq_paths.is_empty() {
            let mut sum = 0.0;
            let mut n = 0.0;
            for p in &self.freq_paths {
                if read_to(p, &mut self.buf) {
                    if let Ok(v) = self.buf.trim().parse::<f64>() {
                        sum += v;
                        n += 1.0;
                    }
                }
            }
            if n > 0.0 {
                self.freq = Some((sum / n / 1_000_000.0) as f32);
            }
        }
        if read_to("/proc/loadavg", &mut self.buf) {
            let mut it = self.buf.split_ascii_whitespace();
            for l in self.load.iter_mut() {
                *l = it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
            }
        }
        if read_to("/proc/uptime", &mut self.buf) {
            self.uptime = self.buf.split_ascii_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
        }
        if check_temp {
            let crit = self.temp_crit.max(1.0);
            if let Some(p) = &self.temps.pkg {
                if read_to(p, &mut self.buf) {
                    self.temp = self.buf.trim().parse::<f32>().ok().map(|v| v / 1000.0);
                }
            }
            if let Some(t) = self.temp {
                push(&mut self.temp_hist[0], (t / crit * 100.0).clamp(0.0, 100.0));
            }
            if !self.temps.cores.is_empty() {
                let mut by_core: Vec<(usize, f32)> = Vec::with_capacity(self.temps.cores.len());
                for (id, p) in &self.temps.cores {
                    if read_to(p, &mut self.buf) {
                        if let Ok(v) = self.buf.trim().parse::<f32>() {
                            by_core.push((*id, v / 1000.0));
                        }
                    }
                }
                for t in 0..self.threads {
                    let cid = self.thread_core[t];
                    let v = by_core.iter().find(|(id, _)| *id == cid).map(|x| x.1).or(self.temp);
                    self.core_temps[t] = v;
                    if let Some(v) = v {
                        push(&mut self.temp_hist[t + 1], (v / crit * 100.0).clamp(0.0, 100.0));
                    }
                }
            }
        }
    }

    pub fn has_core_temps(&self) -> bool {
        !self.temps.cores.is_empty()
    }
}

// ======================================================================= MEM

#[derive(Default)]
pub struct Zram {
    pub name: String,
    pub orig: u64,
    pub compr: u64,
    pub used: u64,
}

pub struct Mem {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub cached: u64,
    pub free: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub swap_free: u64,
    pub hist: [VecDeque<f32>; 6], // used, available, cached, free, swap used, swap free
    pub zram: Vec<Zram>,
    zram_devs: Vec<String>,
    buf: String,
}

impl Mem {
    pub fn new() -> Mem {
        let mut zram_devs = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/sys/block") {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with("zram") {
                    zram_devs.push(n);
                }
            }
        }
        zram_devs.sort();
        let mut m = Mem {
            total: 1,
            used: 0,
            available: 0,
            cached: 0,
            free: 0,
            swap_total: 0,
            swap_used: 0,
            swap_free: 0,
            hist: Default::default(),
            zram: Vec::new(),
            zram_devs,
            buf: String::with_capacity(4096),
        };
        m.collect();
        m
    }

    pub fn collect(&mut self) {
        if read_to("/proc/meminfo", &mut self.buf) {
            let (mut total, mut free, mut avail, mut cached, mut st, mut sf, mut sreclaim) = (0u64, 0u64, None, 0u64, 0u64, 0u64, 0u64);
            for l in self.buf.lines() {
                let mut it = l.split_ascii_whitespace();
                let k = it.next().unwrap_or("");
                let v: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0) * 1024;
                match k {
                    "MemTotal:" => total = v,
                    "MemFree:" => free = v,
                    "MemAvailable:" => avail = Some(v),
                    "Cached:" => cached = v,
                    "SReclaimable:" => sreclaim = v,
                    "SwapTotal:" => st = v,
                    "SwapFree:" => sf = v,
                    _ => {}
                }
            }
            let avail = avail.unwrap_or(free + cached);
            self.total = total.max(1);
            self.free = free;
            self.available = avail;
            self.cached = cached + sreclaim;
            self.used = total.saturating_sub(avail);
            self.swap_total = st;
            self.swap_free = sf;
            self.swap_used = st.saturating_sub(sf);
            let p = |v: u64, t: u64| if t > 0 { (v as f64 * 100.0 / t as f64) as f32 } else { 0.0 };
            let vals = [p(self.used, total), p(avail, total), p(self.cached, total), p(free, total), p(self.swap_used, st), p(sf, st)];
            for (h, v) in self.hist.iter_mut().zip(vals) {
                push(h, v);
            }
        }
        self.zram.clear();
        for d in &self.zram_devs {
            if read_to(&format!("/sys/block/{d}/mm_stat"), &mut self.buf) {
                let v: Vec<u64> = self.buf.split_ascii_whitespace().filter_map(|x| x.parse().ok()).collect();
                let size = read_num(&format!("/sys/block/{d}/disksize")).unwrap_or(0.0) as u64;
                if v.len() >= 3 && size > 0 {
                    self.zram.push(Zram { name: d.clone(), orig: v[0], compr: v[1], used: v[2] });
                }
            }
        }
    }
}

// ===================================================================== DISKS

pub struct Disk {
    pub name: String,
    pub mount: String,
    /// io rates available for this disk
    pub io: bool,
    dev: String,
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub used_pct: f32,
    io_dev: Option<String>,
    zfs_objset: Option<String>,
    prev_rw: Option<(u64, u64)>,
    pub read_bps: f64,
    pub write_bps: f64,
}

pub struct Disks {
    pub list: Vec<Disk>,
    last_scan: Option<Instant>,
    last_io: Instant,
    filter: String,
    buf: String,
}

const FS_OK: &[&str] = &["ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk", "jfs", "reiserfs", "bcachefs", "hfsplus", "apfs"];

fn unescape_mount(s: &str) -> String {
    s.replace("\\040", " ").replace("\\011", "\t").replace("\\134", "\\")
}

impl Disks {
    pub fn new(filter: &str) -> Disks {
        let mut d = Disks { list: Vec::new(), last_scan: None, last_io: Instant::now(), filter: filter.to_string(), buf: String::with_capacity(8192) };
        d.collect();
        d
    }

    pub fn set_filter(&mut self, f: &str) {
        self.filter = f.to_string();
        self.last_scan = None;
    }

    fn passes(&self, mount: &str) -> bool {
        let f = self.filter.trim();
        if f.is_empty() {
            return true;
        }
        let (exclude, list) = match f.strip_prefix("exclude=") {
            Some(r) => (true, r),
            None => (false, f),
        };
        let name = if mount == "/" { "root" } else { mount.rsplit('/').next().unwrap_or(mount) };
        let hit = list.split(|c: char| c == ' ' || c == ',').filter(|s| !s.is_empty()).any(|x| {
            let x = x.trim();
            x == name || x == mount || (x == "root" && mount == "/")
        });
        hit != exclude
    }

    fn scan(&mut self) {
        let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else { return };
        let mut new: Vec<Disk> = Vec::new();
        for l in mounts.lines() {
            let mut it = l.split_ascii_whitespace();
            let (Some(dev), Some(mnt), Some(fs)) = (it.next(), it.next(), it.next()) else { continue };
            if !FS_OK.contains(&fs) {
                continue;
            }
            let mnt = unescape_mount(mnt);
            if mnt.starts_with("/snap") || mnt.starts_with("/var/lib/docker") || mnt.starts_with("/var/lib/containers") || mnt.starts_with("/run") {
                continue;
            }
            if dev.starts_with("/dev/loop") {
                continue;
            }
            if !self.passes(&mnt) {
                continue;
            }
            // Same device mounted twice (bind mounts, OMV's /srv/dev-disk-by-*):
            // keep one entry, under the most readable mountpoint.
            if let Some(d) = new.iter_mut().find(|d| d.dev == dev) {
                if mount_rank(&mnt) < mount_rank(&d.mount) {
                    d.name = disk_name(&mnt);
                    d.mount = mnt;
                }
                continue;
            }
            // keep io state of existing entries (matched by device, not mountpoint)
            if let Some(pos) = self.list.iter().position(|d| d.dev == dev) {
                let mut d = self.list.swap_remove(pos);
                d.name = disk_name(&mnt);
                d.mount = mnt;
                new.push(d);
                continue;
            }
            let (io_dev, zfs_objset) = if fs == "zfs" {
                (None, find_zfs_objset(dev))
            } else {
                let canon = std::fs::canonicalize(dev).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
                (canon.or_else(|| dev.rsplit('/').next().map(String::from)), None)
            };
            let io = io_dev.is_some() || zfs_objset.is_some();
            new.push(Disk { name: disk_name(&mnt), mount: mnt, io, dev: dev.to_string(), total: 0, used: 0, free: 0, used_pct: 0.0, io_dev, zfs_objset, prev_rw: None, read_bps: 0.0, write_bps: 0.0 });
        }
        // root first, rest in mount order
        if let Some(pos) = new.iter().position(|d| d.mount == "/") {
            let r = new.remove(pos);
            new.insert(0, r);
        }
        self.list = new;
        self.last_scan = Some(Instant::now());
    }

    pub fn collect(&mut self) {
        if self.last_scan.map_or(true, |t| t.elapsed().as_secs() >= 10) {
            self.scan();
        }
        for d in self.list.iter_mut() {
            let Ok(c) = std::ffi::CString::new(d.mount.as_str()) else { continue };
            let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0 {
                let fr = s.f_frsize as u64;
                let total = s.f_blocks as u64 * fr;
                let avail = s.f_bavail as u64 * fr;
                let used = (s.f_blocks as u64).saturating_sub(s.f_bfree as u64) * fr;
                d.total = total;
                d.used = used;
                d.free = avail;
                d.used_pct = if used + avail > 0 { used as f32 * 100.0 / (used + avail) as f32 } else { 0.0 };
            }
        }
        // io
        let dt = self.last_io.elapsed().as_secs_f64().max(0.001);
        self.last_io = Instant::now();
        let need_ds = self.list.iter().any(|d| d.io_dev.is_some());
        if need_ds && read_to("/proc/diskstats", &mut self.buf) {
            for l in self.buf.lines() {
                let mut it = l.split_ascii_whitespace();
                let name = it.nth(2).unwrap_or("");
                if let Some(d) = self.list.iter_mut().find(|d| d.io_dev.as_deref() == Some(name)) {
                    let f: Vec<u64> = it.take(8).filter_map(|x| x.parse().ok()).collect();
                    if f.len() >= 7 {
                        let (r, w) = (f[2] * 512, f[6] * 512);
                        if let Some((pr, pw)) = d.prev_rw {
                            d.read_bps = r.saturating_sub(pr) as f64 / dt;
                            d.write_bps = w.saturating_sub(pw) as f64 / dt;
                        }
                        d.prev_rw = Some((r, w));
                    }
                }
            }
        }
        for d in self.list.iter_mut() {
            if let Some(p) = &d.zfs_objset {
                if read_to(p, &mut self.buf) {
                    let (mut r, mut w) = (0, 0);
                    for l in self.buf.lines() {
                        let mut it = l.split_ascii_whitespace();
                        match it.next() {
                            Some("nread") => r = it.nth(1).and_then(|x| x.parse().ok()).unwrap_or(0),
                            Some("nwritten") => w = it.nth(1).and_then(|x| x.parse().ok()).unwrap_or(0),
                            _ => {}
                        }
                    }
                    if let Some((pr, pw)) = d.prev_rw {
                        d.read_bps = (r as u64).saturating_sub(pr) as f64 / dt;
                        d.write_bps = (w as u64).saturating_sub(pw) as f64 / dt;
                    }
                    d.prev_rw = Some((r, w));
                }
            }
        }
    }
}

/// Display name for a mountpoint. OpenMediaVault-style `/srv/dev-disk-by-label-raid`
/// becomes `raid`, `/srv/dev-disk-by-uuid-1a2b3c4d-...` becomes `uuid-1a2b3c4d`.
pub fn disk_name(mnt: &str) -> String {
    if mnt == "/" {
        return "root".into();
    }
    let base = mnt.trim_end_matches('/').rsplit('/').next().unwrap_or(mnt);
    if let Some(r) = base.strip_prefix("dev-disk-by-") {
        if let Some(l) = r.strip_prefix("label-") {
            return l.to_string();
        }
        if let Some(u) = r.strip_prefix("uuid-") {
            return format!("uuid-{}", u.chars().take(8).collect::<String>());
        }
        if let Some(i) = r.strip_prefix("id-") {
            return i.to_string();
        }
        return r.to_string();
    }
    base.to_string()
}

/// Lower is nicer: prefer a plain mountpoint over /srv/dev-disk-by-* and deep paths.
fn mount_rank(mnt: &str) -> usize {
    let by_dev = if mnt.contains("/dev-disk-by-") { 100 } else { 0 };
    by_dev + mnt.matches('/').count()
}

fn find_zfs_objset(dataset: &str) -> Option<String> {
    let pool = dataset.split('/').next()?;
    let dir = format!("/proc/spl/kstat/zfs/{pool}");
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if !n.starts_with("objset-") {
            continue;
        }
        let p = format!("{dir}/{n}");
        if let Ok(s) = std::fs::read_to_string(&p) {
            if s.lines().any(|l| l.starts_with("dataset_name") && l.split_ascii_whitespace().nth(2) == Some(dataset)) {
                return Some(p);
            }
        }
    }
    None
}

// ======================================================================= NET

pub struct Net {
    pub devs: Vec<String>,
    pub sel: usize,
    prev: Option<(u64, u64)>,
    offset: (u64, u64),
    last: Instant,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub rx_top: f64,
    pub tx_top: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub hist_rx: VecDeque<f64>,
    pub hist_tx: VecDeque<f64>,
    buf: String,
}

fn default_route_iface() -> Option<String> {
    let s = std::fs::read_to_string("/proc/net/route").ok()?;
    for l in s.lines().skip(1) {
        let f: Vec<&str> = l.split_ascii_whitespace().collect();
        if f.len() > 2 && f[1] == "00000000" {
            return Some(f[0].to_string());
        }
    }
    None
}

impl Net {
    pub fn new(pref: &str) -> Net {
        let mut n = Net {
            devs: Vec::new(),
            sel: 0,
            prev: None,
            offset: (0, 0),
            last: Instant::now(),
            rx_bps: 0.0,
            tx_bps: 0.0,
            rx_top: 0.0,
            tx_top: 0.0,
            rx_total: 0,
            tx_total: 0,
            hist_rx: VecDeque::with_capacity(HIST),
            hist_tx: VecDeque::with_capacity(HIST),
            buf: String::with_capacity(4096),
        };
        n.refresh_devs();
        let want = if pref.is_empty() { default_route_iface() } else { Some(pref.to_string()) };
        if let Some(w) = want {
            if let Some(i) = n.devs.iter().position(|d| *d == w) {
                n.sel = i;
            }
        }
        n.collect();
        n
    }

    fn refresh_devs(&mut self) {
        let cur = self.devs.get(self.sel).cloned();
        self.devs.clear();
        if read_to("/proc/net/dev", &mut self.buf) {
            for l in self.buf.lines().skip(2) {
                if let Some((n, _)) = l.split_once(':') {
                    let n = n.trim();
                    if n != "lo" {
                        self.devs.push(n.to_string());
                    }
                }
            }
        }
        if let Some(c) = cur {
            self.sel = self.devs.iter().position(|d| *d == c).unwrap_or(0);
        }
    }

    pub fn current(&self) -> &str {
        self.devs.get(self.sel).map(|s| s.as_str()).unwrap_or("none")
    }

    pub fn switch(&mut self, dir: i32) {
        self.refresh_devs();
        if self.devs.is_empty() {
            return;
        }
        let n = self.devs.len() as i32;
        self.sel = ((self.sel as i32 + dir).rem_euclid(n)) as usize;
        self.prev = None;
        self.offset = (0, 0);
        self.rx_top = 0.0;
        self.tx_top = 0.0;
        self.hist_rx.clear();
        self.hist_tx.clear();
        self.collect();
    }

    /// Reset download/upload totals (issue #63).
    pub fn reset_totals(&mut self) {
        if let Some((r, t)) = self.prev {
            self.offset = (r, t);
            self.rx_total = 0;
            self.tx_total = 0;
            self.rx_top = 0.0;
            self.tx_top = 0.0;
        }
    }

    pub fn collect(&mut self) {
        let dt = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();
        let Some(dev) = self.devs.get(self.sel).cloned() else {
            self.refresh_devs();
            return;
        };
        if !read_to("/proc/net/dev", &mut self.buf) {
            return;
        }
        let mut found = false;
        for l in self.buf.lines().skip(2) {
            let Some((n, rest)) = l.split_once(':') else { continue };
            if n.trim() != dev {
                continue;
            }
            found = true;
            let f: Vec<u64> = rest.split_ascii_whitespace().filter_map(|x| x.parse().ok()).collect();
            if f.len() < 9 {
                break;
            }
            let (rx, tx) = (f[0], f[8]);
            if let Some((pr, pt)) = self.prev {
                // counters can reset (iface re-created)
                self.rx_bps = rx.saturating_sub(pr) as f64 / dt;
                self.tx_bps = tx.saturating_sub(pt) as f64 / dt;
                if rx < pr || tx < pt {
                    self.offset = (0, 0);
                }
            } else {
                self.rx_bps = 0.0;
                self.tx_bps = 0.0;
            }
            self.prev = Some((rx, tx));
            self.rx_total = rx.saturating_sub(self.offset.0);
            self.tx_total = tx.saturating_sub(self.offset.1);
            self.rx_top = self.rx_top.max(self.rx_bps);
            self.tx_top = self.tx_top.max(self.tx_bps);
            push(&mut self.hist_rx, self.rx_bps);
            push(&mut self.hist_tx, self.tx_bps);
            break;
        }
        if !found {
            self.refresh_devs();
        }
    }
}

// =================================================================== BATTERY

pub struct Battery {
    path: Option<String>,
    pub percent: Option<u8>,
    pub status: String,
    last: Option<Instant>,
}

impl Battery {
    pub fn new() -> Battery {
        let mut path = None;
        if let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") {
            let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            v.sort();
            for p in v {
                let ty = read_str(&format!("{}/type", p.display())).unwrap_or_default();
                let scope = read_str(&format!("{}/scope", p.display())).unwrap_or_default();
                if ty == "Battery" && scope != "Device" && std::path::Path::new(&format!("{}/capacity", p.display())).exists() {
                    path = Some(p.display().to_string());
                    break;
                }
            }
        }
        Battery { path, percent: None, status: String::new(), last: None }
    }

    pub fn present(&self) -> bool {
        self.path.is_some()
    }

    pub fn collect(&mut self) {
        let Some(p) = &self.path else { return };
        if self.last.map_or(false, |t| t.elapsed().as_secs() < 5) {
            return;
        }
        self.last = Some(Instant::now());
        self.percent = read_num(&format!("{p}/capacity")).map(|v| v.clamp(0.0, 100.0) as u8);
        self.status = read_str(&format!("{p}/status")).unwrap_or_default();
    }
}

// ======================================================================= GPU

#[derive(Clone, Default)]
pub struct GpuStat {
    pub name: String,
    pub util: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub temp: Option<f32>,
}

struct AmdGpu {
    busy: String,
    vram_used: Option<String>,
    vram_total: Option<String>,
    temp: Option<String>,
}

pub struct Gpus {
    amd: Vec<AmdGpu>,
    nvidia: Option<Arc<Mutex<Vec<GpuStat>>>>,
    child: Option<libc::pid_t>,
    pub stats: Vec<GpuStat>,
    pub hist: Vec<VecDeque<f32>>,
    buf: String,
}

extern "C" {
    static environ: *const *const libc::c_char;
}

/// posix_spawnp with stdout piped back (std::process would pull in glibc 2.39
/// pidfd symbols and break the binary on Debian 12 / Proxmox VE 8).
fn spawn_reader(args: &[&str]) -> Option<(libc::pid_t, File)> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    let cargs: Vec<CString> = args.iter().map(|a| CString::new(*a).ok()).collect::<Option<_>>()?;
    let mut argv: Vec<*mut libc::c_char> = cargs.iter().map(|c| c.as_ptr() as *mut libc::c_char).collect();
    argv.push(std::ptr::null_mut());
    unsafe {
        let mut fds = [0 as libc::c_int; 2];
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return None;
        }
        let mut fa: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut fa);
        libc::posix_spawn_file_actions_adddup2(&mut fa, fds[1], 1);
        libc::posix_spawn_file_actions_addopen(&mut fa, 0, c"/dev/null".as_ptr(), libc::O_RDONLY, 0);
        libc::posix_spawn_file_actions_addopen(&mut fa, 2, c"/dev/null".as_ptr(), libc::O_WRONLY, 0);
        let mut pid: libc::pid_t = 0;
        let r = libc::posix_spawnp(&mut pid, cargs[0].as_ptr(), &fa, std::ptr::null(), argv.as_ptr(), environ as *const *mut libc::c_char);
        libc::posix_spawn_file_actions_destroy(&mut fa);
        libc::close(fds[1]);
        if r != 0 {
            libc::close(fds[0]);
            return None;
        }
        Some((pid, File::from_raw_fd(fds[0])))
    }
}

fn in_path(bin: &str) -> bool {
    std::env::var("PATH").unwrap_or_default().split(':').any(|d| std::path::Path::new(d).join(bin).is_file())
}

impl Gpus {
    pub fn new(enabled: bool, update_ms: u64) -> Gpus {
        let mut g = Gpus { amd: Vec::new(), nvidia: None, child: None, stats: Vec::new(), hist: Vec::new(), buf: String::new() };
        if !enabled {
            return g;
        }
        if let Ok(rd) = std::fs::read_dir("/sys/class/drm") {
            let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            v.sort();
            for p in v {
                let n = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                if !n.starts_with("card") || n.contains('-') {
                    continue;
                }
                let dev = format!("{}/device", p.display());
                let busy = format!("{dev}/gpu_busy_percent");
                if !std::path::Path::new(&busy).exists() {
                    continue;
                }
                let ex = |f: &str| {
                    let s = format!("{dev}/{f}");
                    std::path::Path::new(&s).exists().then_some(s)
                };
                let mut temp = None;
                if let Ok(h) = std::fs::read_dir(format!("{dev}/hwmon")) {
                    for e in h.flatten() {
                        let t = format!("{}/temp1_input", e.path().display());
                        if std::path::Path::new(&t).exists() {
                            temp = Some(t);
                        }
                    }
                }
                g.amd.push(AmdGpu { busy, vram_used: ex("mem_info_vram_used"), vram_total: ex("mem_info_vram_total"), temp });
            }
        }
        if (std::path::Path::new("/proc/driver/nvidia").exists() || std::path::Path::new("/dev/nvidia0").exists()) && in_path("nvidia-smi") {
            // One long-running process streaming csv, instead of forking every tick.
            let ms = update_ms.clamp(200, 10_000).to_string();
            if let Some((pid, out)) = spawn_reader(&["nvidia-smi", "--query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu", "--format=csv,noheader,nounits", "-lms", &ms]) {
                let shared = Arc::new(Mutex::new(Vec::<GpuStat>::new()));
                let s2 = shared.clone();
                {
                    std::thread::spawn(move || {
                        let r = std::io::BufReader::new(out);
                        for line in r.lines().map_while(Result::ok) {
                            let f: Vec<&str> = line.split(',').map(|x| x.trim()).collect();
                            if f.len() < 6 {
                                continue;
                            }
                            let idx: usize = f[0].parse().unwrap_or(0);
                            let st = GpuStat {
                                name: f[1].replace("NVIDIA ", "").replace("GeForce ", ""),
                                util: f[2].parse().unwrap_or(0.0),
                                mem_used: f[3].parse::<u64>().unwrap_or(0) << 20,
                                mem_total: f[4].parse::<u64>().unwrap_or(0) << 20,
                                temp: f[5].parse().ok(),
                            };
                            if let Ok(mut v) = s2.lock() {
                                if v.len() <= idx {
                                    v.resize(idx + 1, GpuStat::default());
                                }
                                v[idx] = st;
                            }
                        }
                    });
                }
                g.nvidia = Some(shared);
                g.child = Some(pid);
            }
        }
        g
    }

    pub fn collect(&mut self) {
        self.stats.clear();
        for a in &self.amd {
            let util = if read_to(&a.busy, &mut self.buf) { self.buf.trim().parse().unwrap_or(0.0) } else { 0.0 };
            let rd = |p: &Option<String>| p.as_ref().and_then(|p| read_num(p));
            self.stats.push(GpuStat {
                name: "AMD".into(),
                util,
                mem_used: rd(&a.vram_used).unwrap_or(0.0) as u64,
                mem_total: rd(&a.vram_total).unwrap_or(0.0) as u64,
                temp: rd(&a.temp).map(|t| (t / 1000.0) as f32),
            });
        }
        if let Some(n) = &self.nvidia {
            if let Ok(v) = n.lock() {
                self.stats.extend(v.iter().filter(|s| !s.name.is_empty()).cloned());
            }
        }
        self.hist.resize_with(self.stats.len(), || VecDeque::with_capacity(HIST));
        for (h, s) in self.hist.iter_mut().zip(&self.stats) {
            push(h, s.util);
        }
    }

    pub fn shutdown(&mut self) {
        if let Some(pid) = self.child.take() {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disk_names() {
        assert_eq!(disk_name("/"), "root");
        assert_eq!(disk_name("/mnt/NVME2TB"), "NVME2TB");
        assert_eq!(disk_name("/srv/dev-disk-by-label-raid"), "raid");
        assert_eq!(disk_name("/srv/dev-disk-by-uuid-1a2b3c4d-5e6f-7a8b-9c0d-112233445566"), "uuid-1a2b3c4d");
        assert_eq!(disk_name("/srv/dev-disk-by-id-ata-WDC_WD120"), "ata-WDC_WD120");
        assert!(mount_rank("/mnt/raid") < mount_rank("/srv/dev-disk-by-label-raid"));
        assert!(mount_rank("/data") < mount_rank("/mnt/data/sub"));
    }
}
