//! macOS system collectors: cpu (Mach host_processor_info), memory
//! (host_statistics64 + vm.swapusage), disks (getmntinfo), network
//! (sysctl NET_RT_IFLIST2, 64-bit counters). Same public API as the Linux
//! collectors in collect.rs, so the UI code is shared.
//!
//! Not yet on macOS: temperatures, disk io rates, battery, gpu.

#![allow(deprecated)] // libc marks the mach_* functions deprecated in favour of the mach2 crate

pub use crate::common::{push, shorten_model, HIST};
use std::collections::VecDeque;
use std::ffi::CStr;
use std::mem::{size_of, zeroed};
use std::time::Instant;

fn sysctl_string(name: &CStr) -> Option<String> {
    unsafe {
        let mut len: libc::size_t = 0;
        if libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(len.min(buf.len()));
        Some(String::from_utf8_lossy(&buf[..end]).trim().to_string())
    }
}

fn sysctl_u64(name: &CStr) -> Option<u64> {
    unsafe {
        let mut v: u64 = 0;
        let mut len = size_of::<u64>();
        if libc::sysctlbyname(name.as_ptr(), &mut v as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        // some keys are 32 bit
        if len == 4 {
            v &= 0xffff_ffff;
        }
        Some(v)
    }
}

/// kern.boottime in ms since the epoch.
fn boot_time_ms() -> u64 {
    unsafe {
        let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
        let mut tv: libc::timeval = zeroed();
        let mut len = size_of::<libc::timeval>();
        if libc::sysctl(mib.as_mut_ptr(), 2, &mut tv as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) == 0 {
            return tv.tv_sec as u64 * 1000 + tv.tv_usec as u64 / 1000;
        }
    }
    0
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ======================================================================= CPU

pub struct Cpu {
    pub threads: usize,
    pub model: String,
    prev: Vec<(u64, u64)>,
    pub usage: Vec<f32>,
    pub hist: Vec<VecDeque<f32>>,
    pub freq: Option<f32>,
    pub load: [f32; 3],
    pub uptime: f64,
    pub temp: Option<f32>,
    pub temp_crit: f32,
    pub core_temps: Vec<Option<f32>>,
    boot_ms: u64,
}

impl Cpu {
    pub fn new(custom_name: &str) -> Cpu {
        let mut threads = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }.max(1) as usize;
        if let Some(t) = std::env::var("RBTOP_TEST_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
            threads = t.clamp(1, 1024);
        }
        let mut model = sysctl_string(c"machdep.cpu.brand_string").map(|m| shorten_model(&m)).unwrap_or_else(|| "cpu".into());
        if !custom_name.is_empty() {
            model = custom_name.to_string();
        }
        // Intel Macs only; Apple Silicon has no frequency sysctl.
        let freq = sysctl_u64(c"hw.cpufrequency").filter(|&f| f > 0).map(|f| (f as f64 / 1e9) as f32);
        let mut c = Cpu {
            threads,
            model,
            prev: vec![(0, 0); threads + 1],
            usage: vec![0.0; threads + 1],
            hist: vec![VecDeque::with_capacity(HIST); threads + 1],
            freq,
            load: [0.0; 3],
            uptime: 0.0,
            temp: None,
            temp_crit: 100.0,
            core_temps: vec![None; threads],
            boot_ms: boot_time_ms(),
        };
        c.collect(true);
        c
    }

    pub fn has_core_temps(&self) -> bool {
        false
    }

    pub fn collect(&mut self, _check_temp: bool) {
        unsafe {
            let mut count: libc::natural_t = 0;
            let mut info: libc::processor_info_array_t = std::ptr::null_mut();
            let mut info_cnt: libc::mach_msg_type_number_t = 0;
            let kr = libc::host_processor_info(libc::mach_host_self(), libc::PROCESSOR_CPU_LOAD_INFO, &mut count, &mut info, &mut info_cnt);
            if kr == libc::KERN_SUCCESS && !info.is_null() {
                let loads = std::slice::from_raw_parts(info as *const libc::processor_cpu_load_info, count as usize);
                let (mut tt, mut ti) = (0u64, 0u64);
                for (i, l) in loads.iter().enumerate() {
                    let t = &l.cpu_ticks;
                    let user = t[libc::CPU_STATE_USER as usize] as u64;
                    let sys = t[libc::CPU_STATE_SYSTEM as usize] as u64;
                    let idle = t[libc::CPU_STATE_IDLE as usize] as u64;
                    let nice = t[libc::CPU_STATE_NICE as usize] as u64;
                    let total = user + sys + idle + nice;
                    tt += total;
                    ti += idle;
                    if i < self.threads {
                        self.update(i + 1, total, idle);
                    }
                }
                self.update(0, tt, ti);
                libc::vm_deallocate(libc::mach_task_self(), info as libc::vm_address_t, info_cnt as libc::vm_size_t * size_of::<libc::integer_t>() as libc::vm_size_t);
            }
            let mut la = [0f64; 3];
            if libc::getloadavg(la.as_mut_ptr(), 3) == 3 {
                for (d, s) in self.load.iter_mut().zip(la) {
                    *d = s as f32;
                }
            }
        }
        self.uptime = now_ms().saturating_sub(self.boot_ms) as f64 / 1000.0;
    }

    fn update(&mut self, i: usize, total: u64, idle: u64) {
        let (pt, pi) = self.prev[i];
        let dt = total.wrapping_sub(pt);
        let di = idle.wrapping_sub(pi);
        let pct = if pt > 0 && dt > 0 && dt < u64::MAX / 2 { (100.0 * (dt - di.min(dt)) as f32 / dt as f32).clamp(0.0, 100.0) } else { 0.0 };
        self.prev[i] = (total, idle);
        self.usage[i] = pct;
        push(&mut self.hist[i], pct);
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
    pub hist: [VecDeque<f32>; 6],
    /// always empty on macOS (memory compression is shown as part of "used")
    pub zram: Vec<Zram>,
    page: u64,
}

impl Mem {
    pub fn new() -> Mem {
        let mut m = Mem {
            total: sysctl_u64(c"hw.memsize").unwrap_or(1).max(1),
            used: 0,
            available: 0,
            cached: 0,
            free: 0,
            swap_total: 0,
            swap_used: 0,
            swap_free: 0,
            hist: Default::default(),
            zram: Vec::new(),
            page: unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64,
        };
        m.collect();
        m
    }

    pub fn collect(&mut self) {
        unsafe {
            let mut vm: libc::vm_statistics64 = zeroed();
            let mut cnt = libc::HOST_VM_INFO64_COUNT;
            let kr = libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, &mut vm as *mut _ as libc::host_info64_t, &mut cnt);
            if kr == libc::KERN_SUCCESS {
                let p = self.page;
                // copy out of the packed struct first
                let (free, spec, wired, internal, purgeable, external, compressed) = (
                    vm.free_count as u64,
                    vm.speculative_count as u64,
                    vm.wire_count as u64,
                    vm.internal_page_count as u64,
                    vm.purgeable_count as u64,
                    vm.external_page_count as u64,
                    vm.compressor_page_count as u64,
                );
                // Same definitions as Activity Monitor:
                // used = app memory (internal - purgeable) + wired + compressed
                let used = (internal.saturating_sub(purgeable) + wired + compressed) * p;
                self.used = used.min(self.total);
                self.cached = (external + purgeable) * p;
                self.free = free.saturating_sub(spec) * p;
                self.available = self.total.saturating_sub(self.used);
            }
            let mut sw: libc::xsw_usage = zeroed();
            let mut len = size_of::<libc::xsw_usage>();
            let mut mib = [libc::CTL_VM, libc::VM_SWAPUSAGE];
            if libc::sysctl(mib.as_mut_ptr(), 2, &mut sw as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) == 0 {
                self.swap_total = sw.xsu_total;
                self.swap_used = sw.xsu_used;
                self.swap_free = sw.xsu_avail;
            }
        }
        let t = self.total;
        let pc = |v: u64, t: u64| if t > 0 { (v as f64 * 100.0 / t as f64) as f32 } else { 0.0 };
        let st = self.swap_total;
        let vals = [pc(self.used, t), pc(self.available, t), pc(self.cached, t), pc(self.free, t), pc(self.swap_used, st), pc(self.swap_free, st)];
        for (h, v) in self.hist.iter_mut().zip(vals) {
            push(h, v);
        }
    }
}

// ===================================================================== DISKS

pub struct Disk {
    pub name: String,
    pub mount: String,
    /// io rates available (not yet on macOS)
    pub io: bool,
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub used_pct: f32,
    pub read_bps: f64,
    pub write_bps: f64,
}

pub struct Disks {
    pub list: Vec<Disk>,
    filter: String,
}

const FS_OK: &[&str] = &["apfs", "hfs", "exfat", "msdos", "ntfs", "ufs", "zfs"];

/// "/" -> root, "/System/Volumes/Data" -> data, "/Volumes/Backup" -> Backup
pub fn disk_name(mnt: &str) -> String {
    match mnt {
        "/" => "root".into(),
        "/System/Volumes/Data" => "data".into(),
        _ => mnt.trim_end_matches('/').rsplit('/').next().unwrap_or(mnt).to_string(),
    }
}

/// Which mounts to show: user-visible volumes, plus the Data volume that
/// holds everything the user writes (it is flagged "don't browse").
pub fn wanted_mount(mnt: &str, fstype: &str, dontbrowse: bool) -> bool {
    if !FS_OK.contains(&fstype) {
        return false;
    }
    if mnt == "/" || mnt == "/System/Volumes/Data" {
        return true;
    }
    if mnt.starts_with("/System/Volumes/") || mnt.starts_with("/private/") || mnt.contains("com.apple.") || mnt.starts_with("/Library/Developer/CoreSimulator") {
        return false;
    }
    !dontbrowse
}

impl Disks {
    pub fn new(filter: &str) -> Disks {
        let mut d = Disks { list: Vec::new(), filter: filter.to_string() };
        d.collect();
        d
    }

    pub fn set_filter(&mut self, f: &str) {
        self.filter = f.to_string();
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
        let name = disk_name(mount);
        let hit = list.split([' ', ',']).filter(|s| !s.is_empty()).any(|x| x == name || x == mount);
        hit != exclude
    }

    pub fn collect(&mut self) {
        // getmntinfo(MNT_NOWAIT) is cheap, so mounts are re-read every tick
        let mut new: Vec<Disk> = Vec::new();
        unsafe {
            let mut mnts: *mut libc::statfs = std::ptr::null_mut();
            // MNT_NOWAIT: cached values, never blocks on a hung network mount
            let n = libc::getmntinfo(&mut mnts, libc::MNT_NOWAIT);
            if n <= 0 || mnts.is_null() {
                return;
            }
            for s in std::slice::from_raw_parts(mnts, n as usize) {
                let mnt = CStr::from_ptr(s.f_mntonname.as_ptr()).to_string_lossy().into_owned();
                let fstype = CStr::from_ptr(s.f_fstypename.as_ptr()).to_string_lossy().into_owned();
                let dontbrowse = (s.f_flags & libc::MNT_DONTBROWSE as u32) != 0;
                if !wanted_mount(&mnt, &fstype, dontbrowse) || !self.passes(&mnt) {
                    continue;
                }
                let bs = s.f_bsize as u64;
                let total = s.f_blocks * bs;
                let avail = s.f_bavail * bs;
                let used = s.f_blocks.saturating_sub(s.f_bfree) * bs;
                let used_pct = if used + avail > 0 { used as f32 * 100.0 / (used + avail) as f32 } else { 0.0 };
                new.push(Disk { name: disk_name(&mnt), mount: mnt, io: false, total, used, free: avail, used_pct, read_bps: 0.0, write_bps: 0.0 });
            }
        }
        if let Some(pos) = new.iter().position(|d| d.mount == "/") {
            let r = new.remove(pos);
            new.insert(0, r);
        }
        self.list = new;
    }
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
    buf: Vec<u8>,
}

fn skip_iface(n: &str) -> bool {
    ["lo", "gif", "stf", "anpi", "ap", "awdl", "llw", "XHC", "bridge"].iter().any(|p| n.starts_with(p))
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
            buf: Vec::new(),
        };
        let stats = n.read_all();
        n.devs = stats.iter().map(|s| s.0.clone()).collect();
        // preferred device, else the one with the most traffic (no /proc/net/route on macOS)
        n.sel = if !pref.is_empty() {
            n.devs.iter().position(|d| d == pref).unwrap_or(0)
        } else {
            stats.iter().enumerate().max_by_key(|(_, s)| s.1 + s.2).map(|(i, _)| i).unwrap_or(0)
        };
        n.collect();
        n
    }

    /// (name, rx bytes, tx bytes) for every interface, via NET_RT_IFLIST2 (64-bit counters).
    fn read_all(&mut self) -> Vec<(String, u64, u64)> {
        let mut out = Vec::new();
        unsafe {
            let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
            let mut len: libc::size_t = 0;
            if libc::sysctl(mib.as_mut_ptr(), 6, std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 || len == 0 {
                return out;
            }
            self.buf.resize(len + 1024, 0);
            let mut len = self.buf.len();
            if libc::sysctl(mib.as_mut_ptr(), 6, self.buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) != 0 {
                return out;
            }
            let mut off = 0usize;
            while off + 4 <= len {
                let p = self.buf.as_ptr().add(off);
                let msglen = u16::from_ne_bytes([*p, *p.add(1)]) as usize;
                if msglen == 0 {
                    break;
                }
                let mtype = *p.add(3) as libc::c_int;
                if mtype == libc::RTM_IFINFO2 && off + size_of::<libc::if_msghdr2>() <= len {
                    let m: libc::if_msghdr2 = std::ptr::read_unaligned(p as *const libc::if_msghdr2);
                    let idx = m.ifm_index as libc::c_uint;
                    let data = m.ifm_data;
                    let (rx, tx) = (data.ifi_ibytes, data.ifi_obytes);
                    let mut name = [0 as libc::c_char; 64];
                    if !libc::if_indextoname(idx, name.as_mut_ptr()).is_null() {
                        let n = CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned();
                        if !skip_iface(&n) {
                            out.push((n, rx, tx));
                        }
                    }
                }
                off += msglen;
            }
        }
        out
    }

    pub fn current(&self) -> &str {
        self.devs.get(self.sel).map(|s| s.as_str()).unwrap_or("none")
    }

    pub fn switch(&mut self, dir: i32) {
        let cur = self.devs.get(self.sel).cloned();
        self.devs = self.read_all().into_iter().map(|s| s.0).collect();
        if self.devs.is_empty() {
            return;
        }
        let base = cur.and_then(|c| self.devs.iter().position(|d| *d == c)).unwrap_or(0) as i32;
        self.sel = (base + dir).rem_euclid(self.devs.len() as i32) as usize;
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
        let Some(dev) = self.devs.get(self.sel).cloned() else { return };
        let all = self.read_all();
        let Some(&(_, rx, tx)) = all.iter().find(|s| s.0 == dev) else { return };
        if let Some((pr, pt)) = self.prev {
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
    }
}

// ============================================================ BATTERY / GPU
// Step 2 of the macOS port (IOKit). Present as stubs so the UI code is shared.

pub struct Battery {
    pub percent: Option<u8>,
    pub status: String,
}

impl Battery {
    pub fn new() -> Battery {
        Battery { percent: None, status: String::new() }
    }
    pub fn present(&self) -> bool {
        false
    }
    pub fn collect(&mut self) {}
}

#[derive(Clone, Default)]
#[allow(dead_code)]
pub struct GpuStat {
    pub name: String,
    pub util: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub temp: Option<f32>,
}

#[allow(dead_code)]
pub struct Gpus {
    pub stats: Vec<GpuStat>,
    pub hist: Vec<VecDeque<f32>>,
}

impl Gpus {
    pub fn new(_enabled: bool, _update_ms: u64) -> Gpus {
        Gpus { stats: Vec::new(), hist: Vec::new() }
    }
    pub fn collect(&mut self) {}
    pub fn shutdown(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mounts() {
        assert!(wanted_mount("/", "apfs", false));
        assert!(wanted_mount("/System/Volumes/Data", "apfs", true));
        assert!(!wanted_mount("/System/Volumes/Preboot", "apfs", true));
        assert!(!wanted_mount("/System/Volumes/VM", "apfs", true));
        assert!(wanted_mount("/Volumes/Backup", "apfs", false));
        assert!(!wanted_mount("/Volumes/com.apple.TimeMachine.localsnapshots", "apfs", false));
        assert!(!wanted_mount("/dev", "devfs", false));
        assert_eq!(disk_name("/System/Volumes/Data"), "data");
        assert_eq!(disk_name("/Volumes/Backup"), "Backup");
    }
}
