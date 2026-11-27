//! Minimal hand-written IOKit / CoreFoundation bindings for macOS, plus three
//! readers built on them:
//!
//! - `Smc`: the System Management Controller (temperatures). Same protocol as
//!   smcFanControl / iStats / macmon: IOConnectCallStructMethod on AppleSMC with
//!   an 80-byte key-data struct. Apple Silicon reports cpu temps as `Tp*`/`Te*`
//!   keys (type "flt "), gpu as `Tg*`; Intel uses `TC0P` etc. (type "sp78").
//! - `hid_cpu_temp`: fallback via the (private) IOHIDEventSystem, which older
//!   Apple Silicon models expose as "PMU tdie*" sensors.
//! - registry helpers: block storage statistics (disk io), IOAccelerator
//!   performance statistics (gpu load), AppleSmartBattery (battery).
//!
//! Everything returns Option / empty results on failure; nothing here may panic.

#![allow(non_upper_case_globals, non_camel_case_types, clippy::upper_case_acronyms)]

use crate::common::{decode_smc, fourcc, fourcc_str, thin};
use std::ffi::{c_char, c_void, CStr, CString};

// ------------------------------------------------------------- CoreFoundation

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFArrayRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFIndex = isize;
pub type CFTypeID = usize;
pub type Boolean = u8;

const kCFStringEncodingUTF8: u32 = 0x0800_0100;
const kCFNumberSInt32Type: CFIndex = 3;
const kCFNumberSInt64Type: CFIndex = 4;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    fn CFNumberGetTypeID() -> CFTypeID;
    fn CFStringGetTypeID() -> CFTypeID;
    fn CFDictionaryGetTypeID() -> CFTypeID;
    fn CFBooleanGetTypeID() -> CFTypeID;
    fn CFStringCreateWithCString(alloc: CFAllocatorRef, cstr: *const c_char, encoding: u32) -> CFStringRef;
    fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: CFIndex, encoding: u32) -> Boolean;
    fn CFNumberCreate(alloc: CFAllocatorRef, the_type: CFIndex, value: *const c_void) -> CFNumberRef;
    fn CFNumberGetValue(n: CFNumberRef, the_type: CFIndex, value: *mut c_void) -> Boolean;
    fn CFBooleanGetValue(b: CFTypeRef) -> Boolean;
    fn CFDictionaryCreate(alloc: CFAllocatorRef, keys: *const CFTypeRef, values: *const CFTypeRef, n: CFIndex, kcb: *const u8, vcb: *const u8) -> CFDictionaryRef;
    fn CFDictionaryGetValue(d: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    fn CFArrayGetCount(a: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(a: CFArrayRef, i: CFIndex) -> CFTypeRef;
}

/// Owned CF object, released on drop.
struct Cf(CFTypeRef);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

fn cfstr(s: &str) -> Cf {
    let c = CString::new(s).unwrap_or_default();
    Cf(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) })
}

fn cf_to_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    if unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as CFIndex, kCFStringEncodingUTF8) } == 0 {
        return None;
    }
    Some(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

fn cf_to_i64(v: CFTypeRef) -> Option<i64> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut x: i64 = 0;
    (unsafe { CFNumberGetValue(v, kCFNumberSInt64Type, &mut x as *mut i64 as *mut c_void) } != 0).then_some(x)
}

fn cf_to_bool(v: CFTypeRef) -> Option<bool> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFBooleanGetTypeID() } {
        return None;
    }
    Some(unsafe { CFBooleanGetValue(v) } != 0)
}

fn dict_get(d: CFTypeRef, key: &str) -> CFTypeRef {
    if d.is_null() || unsafe { CFGetTypeID(d) != CFDictionaryGetTypeID() } {
        return std::ptr::null();
    }
    let k = cfstr(key);
    unsafe { CFDictionaryGetValue(d, k.0) }
}

// ---------------------------------------------------------------------- IOKit

pub type io_object_t = u32;
pub type kern_return_t = i32;
const MAIN_PORT: u32 = 0; // kIOMainPortDefault == MACH_PORT_NULL

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOBSDNameMatching(main_port: u32, options: u32, bsd_name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingService(main_port: u32, matching: CFDictionaryRef) -> io_object_t;
    fn IOServiceGetMatchingServices(main_port: u32, matching: CFDictionaryRef, existing: *mut io_object_t) -> kern_return_t;
    fn IOIteratorNext(iterator: io_object_t) -> io_object_t;
    fn IOObjectRelease(object: io_object_t) -> kern_return_t;
    fn IOObjectRetain(object: io_object_t) -> kern_return_t;
    fn IOObjectConformsTo(object: io_object_t, class_name: *const c_char) -> i32;
    fn IORegistryEntryGetParentEntry(entry: io_object_t, plane: *const c_char, parent: *mut io_object_t) -> kern_return_t;
    fn IORegistryEntryCreateCFProperty(entry: io_object_t, key: CFStringRef, allocator: CFAllocatorRef, options: u32) -> CFTypeRef;
    fn IOServiceOpen(service: io_object_t, owning_task: u32, the_type: u32, connect: *mut io_object_t) -> kern_return_t;
    fn IOServiceClose(connect: io_object_t) -> kern_return_t;
    fn IOConnectCallStructMethod(connection: io_object_t, selector: u32, input: *const c_void, input_cnt: usize, output: *mut c_void, output_cnt: *mut usize) -> kern_return_t;

    // private, but exported and stable since 10.x (used by many monitors)
    fn IOHIDEventSystemClientCreate(allocator: CFAllocatorRef) -> CFTypeRef;
    fn IOHIDEventSystemClientSetMatching(client: CFTypeRef, matching: CFDictionaryRef) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: CFTypeRef) -> CFArrayRef;
    fn IOHIDServiceClientCopyProperty(service: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDServiceClientCopyEvent(service: CFTypeRef, the_type: i64, options: i32, timestamp: i64) -> CFTypeRef;
    fn IOHIDEventGetFloatValue(event: CFTypeRef, field: i32) -> f64;
}

/// Owned IOKit object, released on drop.
pub struct IoObj(pub io_object_t);
impl Drop for IoObj {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                IOObjectRelease(self.0);
            }
        }
    }
}
impl Clone for IoObj {
    fn clone(&self) -> Self {
        if self.0 != 0 {
            unsafe {
                IOObjectRetain(self.0);
            }
        }
        IoObj(self.0)
    }
}

fn first_service(class: &str) -> Option<IoObj> {
    let c = CString::new(class).ok()?;
    // IOServiceGetMatchingService consumes the matching dictionary
    let s = unsafe { IOServiceGetMatchingService(MAIN_PORT, IOServiceMatching(c.as_ptr())) };
    (s != 0).then_some(IoObj(s))
}

fn all_services(class: &str) -> Vec<IoObj> {
    let mut out = Vec::new();
    let Ok(c) = CString::new(class) else { return out };
    let mut it: io_object_t = 0;
    if unsafe { IOServiceGetMatchingServices(MAIN_PORT, IOServiceMatching(c.as_ptr()), &mut it) } != 0 || it == 0 {
        return out;
    }
    let it = IoObj(it);
    loop {
        let s = unsafe { IOIteratorNext(it.0) };
        if s == 0 {
            break;
        }
        out.push(IoObj(s));
    }
    out
}

fn property(entry: io_object_t, key: &str) -> Cf {
    let k = cfstr(key);
    Cf(unsafe { IORegistryEntryCreateCFProperty(entry, k.0, std::ptr::null(), 0) })
}

// -------------------------------------------------------------- disk io stats

/// The IOBlockStorageDriver behind a BSD device ("disk3s1s1" -> physical disk driver).
/// APFS volumes are walked up through their container to the physical store.
pub fn block_driver_for(bsd: &str) -> Option<IoObj> {
    let c = CString::new(bsd).ok()?;
    let media = unsafe { IOServiceGetMatchingService(MAIN_PORT, IOBSDNameMatching(MAIN_PORT, 0, c.as_ptr())) };
    if media == 0 {
        return None;
    }
    let mut cur = IoObj(media);
    let plane = c"IOService";
    for _ in 0..32 {
        if unsafe { IOObjectConformsTo(cur.0, c"IOBlockStorageDriver".as_ptr()) } != 0 {
            return Some(cur);
        }
        let mut parent: io_object_t = 0;
        if unsafe { IORegistryEntryGetParentEntry(cur.0, plane.as_ptr(), &mut parent) } != 0 || parent == 0 {
            return None;
        }
        cur = IoObj(parent);
    }
    None
}

/// Total bytes (read, written) since boot from the driver's "Statistics".
pub fn block_stats(driver: &IoObj) -> Option<(u64, u64)> {
    let stats = property(driver.0, "Statistics");
    let r = cf_to_i64(dict_get(stats.0, "Bytes (Read)"))?;
    let w = cf_to_i64(dict_get(stats.0, "Bytes (Write)"))?;
    Some((r.max(0) as u64, w.max(0) as u64))
}

// ------------------------------------------------------------------- gpu load

pub struct GpuReading {
    pub util: f32,
    pub mem_used: u64,
}

/// First IOAccelerator's "PerformanceStatistics" (Apple Silicon and AMD Macs).
pub fn gpu_reading(accel: &IoObj) -> Option<GpuReading> {
    let ps = property(accel.0, "PerformanceStatistics");
    if ps.0.is_null() {
        return None;
    }
    let util = cf_to_i64(dict_get(ps.0, "Device Utilization %")).or_else(|| cf_to_i64(dict_get(ps.0, "GPU Activity(%)")))?;
    let mem_used = cf_to_i64(dict_get(ps.0, "In use system memory")).or_else(|| cf_to_i64(dict_get(ps.0, "vramUsedBytes"))).unwrap_or(0);
    Some(GpuReading { util: util.clamp(0, 100) as f32, mem_used: mem_used.max(0) as u64 })
}

pub fn gpu_accelerators() -> Vec<IoObj> {
    all_services("IOAccelerator")
}

pub fn gpu_name(accel: &IoObj) -> String {
    let m = property(accel.0, "model");
    cf_to_string(m.0).map(|s| s.replace("Apple ", "")).unwrap_or_else(|| "GPU".into())
}

// -------------------------------------------------------------------- battery

pub struct BatteryReading {
    pub percent: u8,
    pub status: &'static str,
}

/// The battery, if one is actually installed. Desktop Macs (Mac mini, Studio)
/// also publish an AppleSmartBattery service, but with BatteryInstalled = No.
pub fn battery_service() -> Option<IoObj> {
    let b = first_service("AppleSmartBattery")?;
    let installed = cf_to_bool(property(b.0, "BatteryInstalled").0)
        .unwrap_or_else(|| cf_to_i64(property(b.0, "DesignCapacity").0).unwrap_or(0) > 0);
    installed.then_some(b)
}

pub fn battery_reading(b: &IoObj) -> Option<BatteryReading> {
    let cur = cf_to_i64(property(b.0, "CurrentCapacity").0)?;
    let max = cf_to_i64(property(b.0, "MaxCapacity").0).filter(|&m| m > 0).unwrap_or(100);
    let charging = cf_to_bool(property(b.0, "IsCharging").0).unwrap_or(false);
    let full = cf_to_bool(property(b.0, "FullyCharged").0).unwrap_or(false);
    let external = cf_to_bool(property(b.0, "ExternalConnected").0).unwrap_or(false);
    let pct = (cur * 100 / max).clamp(0, 100) as u8;
    let status = if full { "Full" } else if charging { "Charging" } else if !external { "Discharging" } else { "Not charging" };
    Some(BatteryReading { percent: pct, status })
}

// ------------------------------------------------------------------------ SMC

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KeyDataVer {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PLimitData {
    version: u16,
    length: u16,
    cpu_plimit: u32,
    gpu_plimit: u32,
    mem_plimit: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KeyInfo {
    data_size: u32,
    data_type: u32,
    data_attributes: u8,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KeyData {
    key: u32,
    vers: KeyDataVer,
    p_limit_data: PLimitData,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

// The kernel side expects exactly this layout.
const _: () = assert!(std::mem::size_of::<KeyData>() == 80);

const SMC_HANDLE_YPC_EVENT: u32 = 2;
const SMC_MAX_CPU_KEYS: usize = 6;
const SMC_MAX_GPU_KEYS: usize = 3;
const SMC_READ_KEY: u8 = 5;
const SMC_GET_KEY_FROM_INDEX: u8 = 8;
const SMC_GET_KEY_INFO: u8 = 9;

pub struct Smc {
    conn: io_object_t,
    /// selected cpu / gpu temperature keys with their type and size
    cpu: Vec<(u32, u32, u32)>,
    gpu: Vec<(u32, u32, u32)>,
}

impl Drop for Smc {
    fn drop(&mut self) {
        if self.conn != 0 {
            unsafe {
                IOServiceClose(self.conn);
            }
        }
    }
}

fn plausible(t: f32) -> bool {
    t.is_finite() && t > 5.0 && t < 130.0
}

impl Smc {
    pub fn open() -> Option<Smc> {
        let svc = first_service("AppleSMC")?;
        let mut conn: io_object_t = 0;
        #[allow(deprecated)]
        let task = unsafe { libc::mach_task_self() };
        if unsafe { IOServiceOpen(svc.0, task, 0, &mut conn) } != 0 || conn == 0 {
            return None;
        }
        let mut smc = Smc { conn, cpu: Vec::new(), gpu: Vec::new() };
        smc.discover();
        Some(smc)
    }

    fn call(&self, input: &KeyData) -> Option<KeyData> {
        let mut out = KeyData::default();
        let mut out_size = std::mem::size_of::<KeyData>();
        let r = unsafe {
            IOConnectCallStructMethod(self.conn, SMC_HANDLE_YPC_EVENT, input as *const KeyData as *const c_void, std::mem::size_of::<KeyData>(), &mut out as *mut KeyData as *mut c_void, &mut out_size)
        };
        (r == 0 && out.result == 0).then_some(out)
    }

    fn key_info(&self, key: u32) -> Option<KeyInfo> {
        let input = KeyData { key, data8: SMC_GET_KEY_INFO, ..Default::default() };
        self.call(&input).map(|o| o.key_info)
    }

    fn read_raw(&self, key: u32, size: u32) -> Option<[u8; 32]> {
        let mut input = KeyData { key, data8: SMC_READ_KEY, ..Default::default() };
        input.key_info.data_size = size;
        self.call(&input).map(|o| o.bytes)
    }

    fn read(&self, key: u32, ty: u32, size: u32) -> Option<f32> {
        let b = self.read_raw(key, size)?;
        decode_smc(ty, &b[..(size as usize).min(32)])
    }

    /// Enumerate all keys once and keep the temperature sensors rbtop needs.
    fn discover(&mut self) {
        let cnt_key = fourcc("#KEY");
        let count = self.key_info(cnt_key).and_then(|ki| self.read_raw(cnt_key, ki.data_size)).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
        let mut intel = Vec::new();
        for i in 0..count.min(4096) {
            let input = KeyData { data8: SMC_GET_KEY_FROM_INDEX, data32: i, ..Default::default() };
            let Some(o) = self.call(&input) else { continue };
            let name = fourcc_str(o.key);
            let is_cpu = name.starts_with("Tp") || name.starts_with("Te");
            let is_gpu = name.starts_with("Tg");
            let is_intel_cpu = matches!(name.as_str(), "TC0P" | "TC0D" | "TC0E" | "TC0F");
            if !(is_cpu || is_gpu || is_intel_cpu) {
                continue;
            }
            let Some(ki) = self.key_info(o.key) else { continue };
            let Some(v) = self.read(o.key, ki.data_type, ki.data_size) else { continue };
            if !plausible(v) {
                continue;
            }
            let e = (o.key, ki.data_type, ki.data_size);
            if is_intel_cpu {
                intel.push(e);
            } else if is_cpu {
                self.cpu.push(e);
            } else {
                self.gpu.push(e);
            }
        }
        if self.cpu.is_empty() {
            self.cpu = intel;
        }
        // Every key costs an IOConnect round trip per tick. Apple Silicon has
        // dozens of cpu sensors; an evenly spread subset gives the same average.
        self.cpu = thin(&self.cpu, SMC_MAX_CPU_KEYS);
        self.gpu = thin(&self.gpu, SMC_MAX_GPU_KEYS);
    }

    fn avg(&self, keys: &[(u32, u32, u32)]) -> Option<f32> {
        let v: Vec<f32> = keys.iter().filter_map(|&(k, t, s)| self.read(k, t, s)).filter(|t| plausible(*t)).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32)
    }

    pub fn cpu_temp(&self) -> Option<f32> {
        self.avg(&self.cpu)
    }

    pub fn gpu_temp(&self) -> Option<f32> {
        self.avg(&self.gpu)
    }

    pub fn has_cpu(&self) -> bool {
        !self.cpu.is_empty()
    }
}

// ------------------------------------------------------------ HID fallback

const kIOHIDEventTypeTemperature: i64 = 15;

/// Temperature sensors via IOHIDEventSystem (Apple Silicon "PMU tdie*" sensors).
pub struct HidTemps {
    client: Cf,
    services: Cf,
    idx: Vec<CFIndex>,
}

impl HidTemps {
    pub fn open() -> Option<HidTemps> {
        unsafe {
            let client = Cf(IOHIDEventSystemClientCreate(std::ptr::null()));
            if client.0.is_null() {
                return None;
            }
            // match PrimaryUsagePage 0xff00 / PrimaryUsage 5 = temperature sensors
            let (page, usage): (i32, i32) = (0xff00, 5);
            let kp = cfstr("PrimaryUsagePage");
            let ku = cfstr("PrimaryUsage");
            let vp = Cf(CFNumberCreate(std::ptr::null(), kCFNumberSInt32Type, &page as *const i32 as *const c_void));
            let vu = Cf(CFNumberCreate(std::ptr::null(), kCFNumberSInt32Type, &usage as *const i32 as *const c_void));
            let keys = [kp.0, ku.0];
            let vals = [vp.0, vu.0];
            let m = Cf(CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), vals.as_ptr(), 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks));
            IOHIDEventSystemClientSetMatching(client.0, m.0);
            let services = Cf(IOHIDEventSystemClientCopyServices(client.0));
            if services.0.is_null() {
                return None;
            }
            let n = CFArrayGetCount(services.0);
            let product = cfstr("Product");
            let mut idx = Vec::new();
            for i in 0..n {
                let s = CFArrayGetValueAtIndex(services.0, i);
                let p = Cf(IOHIDServiceClientCopyProperty(s, product.0));
                if let Some(name) = cf_to_string(p.0) {
                    if name.contains("tdie") || name.contains("CPU") {
                        idx.push(i);
                    }
                }
            }
            (!idx.is_empty()).then_some(HidTemps { client, services, idx })
        }
    }

    pub fn cpu_temp(&self) -> Option<f32> {
        let _ = &self.client; // keep the client alive while services are used
        let mut v = Vec::new();
        for &i in &self.idx {
            unsafe {
                let s = CFArrayGetValueAtIndex(self.services.0, i);
                let ev = Cf(IOHIDServiceClientCopyEvent(s, kIOHIDEventTypeTemperature, 0, 0));
                if ev.0.is_null() {
                    continue;
                }
                let t = IOHIDEventGetFloatValue(ev.0, (kIOHIDEventTypeTemperature << 16) as i32) as f32;
                if plausible(t) {
                    v.push(t);
                }
            }
        }
        (!v.is_empty()).then(|| v.iter().copied().fold(f32::MIN, f32::max))
    }
}
