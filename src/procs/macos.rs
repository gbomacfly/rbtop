//! macOS process sampling via libproc and sysctl.
//!
//! proc_pidinfo(PROC_PIDTASKALLINFO) gives everything rbtop needs in one call:
//! ppid, uid, status, name, nice, start time, cpu time, threads, rss.
//! For processes of other users it fails without root; then the BSD info
//! (ppid, uid, status, name, start, nice) is used and cpu/memory stay 0. Run with sudo to see all.
//!
//! Cpu times from proc_taskinfo are in Mach absolute time units (ns on Intel,
//! 125/3 ns ticks on Apple Silicon), converted with mach_timebase_info.

#![allow(deprecated)] // libc marks the mach_* functions deprecated in favour of the mach2 crate

use super::Sample;
use std::cell::RefCell;
use std::mem::{size_of, zeroed};

/// rbtop's tick unit on macOS: milliseconds.
pub fn clk_tck() -> f64 {
    1000.0
}

pub struct Sampler {
    pids: Vec<libc::c_int>,
    numer: u64,
    denom: u64,
    boot_ms: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        let (numer, denom) = unsafe {
            let mut tb: libc::mach_timebase_info = zeroed();
            if libc::mach_timebase_info(&mut tb) == 0 && tb.denom != 0 {
                (tb.numer as u64, tb.denom as u64)
            } else {
                (1, 1)
            }
        };
        Sampler { pids: Vec::new(), numer, denom, boot_ms: boot_time_ms() }
    }
}

/// kern.boottime in ms since the epoch.
pub fn boot_time_ms() -> u64 {
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

/// Fixed-size C char array -> &str up to the first NUL.
fn cstr_arr(a: &[libc::c_char]) -> &str {
    let b = unsafe { std::slice::from_raw_parts(a.as_ptr() as *const u8, a.len()) };
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    std::str::from_utf8(&b[..end]).unwrap_or("?")
}

fn state_char(status: u32, running: i32) -> char {
    match status {
        libc::SRUN => {
            if running > 0 {
                'R'
            } else {
                'S'
            }
        }
        libc::SSLEEP => 'S',
        libc::SSTOP => 'T',
        libc::SZOMB => 'Z',
        libc::SIDL => 'I',
        _ => '?',
    }
}

/// BSD info is available for every process without root (unlike task info).
fn bsd_info(pid: libc::c_int) -> Option<libc::proc_bsdinfo> {
    unsafe {
        let mut bi: libc::proc_bsdinfo = zeroed();
        let sz = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let r = libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut bi as *mut _ as *mut libc::c_void, sz);
        (r == sz).then_some(bi)
    }
}

fn bsd_name(b: &libc::proc_bsdinfo) -> &str {
    if b.pbi_name[0] != 0 {
        cstr_arr(&b.pbi_name)
    } else {
        cstr_arr(&b.pbi_comm)
    }
}

impl Sampler {
    pub fn each<F: FnMut(Sample)>(&mut self, mut f: F) {
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return;
        }
        self.pids.resize(n as usize + 64, 0);
        let bytes = (self.pids.len() * size_of::<libc::c_int>()) as libc::c_int;
        let n = unsafe { libc::proc_listallpids(self.pids.as_mut_ptr() as *mut libc::c_void, bytes) };
        if n <= 0 {
            return;
        }
        let n = (n as usize).min(self.pids.len());
        let full_sz = size_of::<libc::proc_taskallinfo>() as libc::c_int;
        for &pid in &self.pids[..n] {
            if pid < 0 {
                continue;
            }
            let mut ti: libc::proc_taskallinfo = unsafe { zeroed() };
            let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKALLINFO, 0, &mut ti as *mut _ as *mut libc::c_void, full_sz) };
            if r == full_sz {
                let b = &ti.pbsd;
                let t = &ti.ptinfo;
                let name = bsd_name(b);
                let mach = t.pti_total_user as u128 + t.pti_total_system as u128;
                let ms = (mach * self.numer as u128 / self.denom as u128 / 1_000_000) as u64;
                let start_ms = b.pbi_start_tvsec * 1000 + b.pbi_start_tvusec / 1000;
                f(Sample {
                    pid,
                    ppid: b.pbi_ppid as i32,
                    name,
                    state: state_char(b.pbi_status, t.pti_numrunning),
                    ticks: ms,
                    nice: b.pbi_nice,
                    threads: t.pti_threadnum.max(0) as u32,
                    start: start_ms.saturating_sub(self.boot_ms),
                    rss: t.pti_resident_size,
                    uid: Some(b.pbi_uid),
                });
            } else if let Some(b) = bsd_info(pid) {
                // other user's process without root: no cpu/memory figures
                let start_ms = b.pbi_start_tvsec * 1000 + b.pbi_start_tvusec / 1000;
                f(Sample {
                    pid,
                    ppid: b.pbi_ppid as i32,
                    name: bsd_name(&b),
                    state: state_char(b.pbi_status, 0),
                    ticks: 0,
                    nice: b.pbi_nice,
                    threads: 0,
                    start: start_ms.saturating_sub(self.boot_ms),
                    rss: 0,
                    uid: Some(b.pbi_uid),
                });
            }
        }
    }
}

pub fn uid_of(pid: i32) -> u32 {
    bsd_info(pid).map(|b| b.pbi_uid).unwrap_or(0)
}

thread_local! {
    static ARGBUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Command line via sysctl KERN_PROCARGS2:
/// [argc: i32][exec path\0][\0 padding][argv0\0 argv1\0 ...][env ...]
pub fn cmdline(pid: i32) -> String {
    ARGBUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        if buf.is_empty() {
            let mut argmax: libc::c_int = 0;
            let mut len = size_of::<libc::c_int>();
            let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
            let ok = unsafe { libc::sysctl(mib.as_mut_ptr(), 2, &mut argmax as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) } == 0;
            buf.resize(if ok && argmax > 0 { argmax as usize } else { 256 * 1024 }, 0);
        }
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut len = buf.len();
        let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
        if r != 0 || len < 4 {
            return String::new();
        }
        crate::common::parse_procargs2(&buf[..len])
    })
}

/// Disk bytes read/written (rusage v2).
pub fn io_bytes(pid: i32) -> Option<(u64, u64)> {
    unsafe {
        let mut ri: libc::rusage_info_v2 = zeroed();
        let r = libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, &mut ri as *mut _ as *mut libc::rusage_info_t);
        (r == 0).then_some((ri.ri_diskio_bytesread, ri.ri_diskio_byteswritten))
    }
}

pub fn read_state(pid: i32) -> Option<char> {
    bsd_info(pid).map(|b| state_char(b.pbi_status, 0))
}

impl super::Procs {
    /// No containers on macOS.
    pub(super) fn container_of(&mut self, _pid: i32) -> Option<String> {
        None
    }
}
