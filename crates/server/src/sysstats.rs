//! The computer and the program for the statistics (admins): the processor
//! and its use, the memory, and the CPU time of the program's threads.

use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// What does not change while the program runs.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub cpu_model: String,
    /// Logical processors the program may use.
    pub cores: usize,
    pub os: String,
}

/// Use over the time since the previous sample; what a system cannot tell
/// stays empty.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// The whole computer: % of all its processors.
    pub cpu: Option<f32>,
    pub ram_total: Option<u64>,
    pub ram_used: Option<u64>,
    /// The program: % of one processor (200 = two busy).
    pub process_cpu: Option<f32>,
    /// Memory of the program in use (resident set / working set).
    pub process_ram: Option<u64>,
    pub thread_count: usize,
    /// Threads by name, % of one processor, the busiest first.
    pub threads: Vec<ThreadUse>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThreadUse {
    pub name: String,
    pub count: usize,
    pub cpu: f32,
}

/// Counters as they are now (totals since boot or since the start).
#[derive(Default)]
struct Counters {
    /// (busy, all) time of every processor together.
    system: Option<(Duration, Duration)>,
    /// (total, available)
    ram: Option<(u64, Option<u64>)>,
    process: Option<Duration>,
    process_ram: Option<u64>,
    /// id -> (name, CPU time)
    threads: HashMap<u64, (String, Duration)>,
}

/// Takes samples; the first has no use figures yet (nothing to compare with).
#[derive(Default)]
pub struct Sampler {
    last: Option<(Instant, Counters)>,
}

impl Sampler {
    pub fn sample(&mut self) -> Sample {
        let now = Instant::now();
        let c = os::counters();
        let mut s = Sample {
            ram_total: c.ram.map(|r| r.0),
            ram_used: c.ram.and_then(|(total, avail)| avail.map(|a| total.saturating_sub(a))),
            process_ram: c.process_ram,
            thread_count: c.threads.len(),
            ..Default::default()
        };
        if let Some((then, prev)) = &self.last {
            let wall = now.duration_since(*then).as_secs_f32().max(0.001);
            if let (Some((b1, a1)), Some((b0, a0))) = (c.system, prev.system) {
                let all = a1.saturating_sub(a0).as_secs_f32();
                if all > 0.0 {
                    s.cpu = Some((b1.saturating_sub(b0).as_secs_f32() / all * 100.0).clamp(0.0, 100.0));
                }
            }
            if let (Some(p1), Some(p0)) = (c.process, prev.process) {
                s.process_cpu = Some(p1.saturating_sub(p0).as_secs_f32() / wall * 100.0);
            }
            let mut by_name: HashMap<String, (usize, Duration)> = HashMap::new();
            for (tid, (name, t1)) in &c.threads {
                // (a thread that began since: all of its time is new)
                let t0 = prev.threads.get(tid).map(|p| p.1).unwrap_or_default();
                let e = by_name.entry(group(name)).or_default();
                e.0 += 1;
                e.1 += t1.saturating_sub(t0);
            }
            let mut threads: Vec<ThreadUse> =
                by_name.into_iter().map(|(name, (count, t))| ThreadUse { name, count, cpu: t.as_secs_f32() / wall * 100.0 }).collect();
            threads.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(a.name.cmp(&b.name)));
            s.threads = threads;
        }
        self.last = Some((now, c));
        s
    }
}

/// Threads of one kind under one name (the workers of the web server are
/// "tokio-runtime-worker", cut to 15 characters by Linux).
fn group(name: &str) -> String {
    if name.starts_with("tokio-runtime") {
        "tokio".into()
    } else if name.is_empty() {
        "?".into()
    } else {
        name.to_string()
    }
}

/// Seconds since the computer started.
pub fn uptime() -> Option<u64> {
    os::uptime()
}

pub fn machine() -> Machine {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    Machine { cpu_model: os::cpu_model().unwrap_or_default(), cores, os: os::name() }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod os {
    use super::Counters;
    use std::time::Duration;

    /// Linux counts processor time in ticks of 1/100 s for user space.
    fn ticks(n: u64) -> Duration {
        Duration::from_millis(n * 10)
    }

    fn read(path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    pub fn uptime() -> Option<u64> {
        read("/proc/uptime")?.split_whitespace().next()?.parse::<f64>().ok().map(|s| s as u64)
    }

    pub fn cpu_model() -> Option<String> {
        let info = read("/proc/cpuinfo")?;
        let field = |key: &str| {
            info.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                (k.trim() == key && !v.trim().is_empty()).then(|| v.trim().to_string())
            })
        };
        // x86 names the processor, ARM boards often the board only
        field("model name")
            .or_else(|| field("Model"))
            .or_else(|| read("/sys/firmware/devicetree/base/model").map(|m| m.trim_end_matches('\0').trim().to_string()))
            .or_else(|| field("Hardware"))
    }

    pub fn name() -> String {
        let kernel = read("/proc/sys/kernel/osrelease").map(|v| v.trim().to_string()).unwrap_or_default();
        let distro = read("/etc/os-release")
            .and_then(|r| r.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())));
        let base = if cfg!(target_os = "android") { "Android" } else { "Linux" };
        match distro {
            // (in a container: the system of the image, the kernel of the host)
            Some(d) if crate::update::in_container() => format!("{base} {kernel}, {d} (Docker)"),
            Some(d) => format!("{base} {kernel}, {d}"),
            None => format!("{base} {kernel}"),
        }
    }

    /// utime + stime of a /proc/.../stat line (after the name in brackets).
    fn stat_times(stat: &str) -> Option<(String, Duration)> {
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        let name = stat[open + 1..close].to_string();
        let f: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        // fields 14 and 15 of the line; f[0] is field 3 (the state)
        let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
        Some((name, super::os::ticks(ticks)))
    }

    pub fn counters() -> Counters {
        let mut c = Counters::default();
        if let Some(stat) = read("/proc/stat") {
            if let Some(line) = stat.lines().next().and_then(|l| l.strip_prefix("cpu ")) {
                let v: Vec<u64> = line.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if v.len() >= 5 {
                    // user nice system idle iowait irq softirq steal
                    let all: u64 = v.iter().take(8).sum();
                    let idle = v[3] + v[4];
                    c.system = Some((ticks(all.saturating_sub(idle)), ticks(all)));
                }
            }
        }
        if let Some(mem) = read("/proc/meminfo") {
            let kb = |key: &str| {
                mem.lines().find_map(|l| l.strip_prefix(key).and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok()))
            };
            if let (Some(total), Some(avail)) = (kb("MemTotal:"), kb("MemAvailable:")) {
                c.ram = Some((total * 1024, Some(avail * 1024)));
            }
        }
        c.process = read("/proc/self/stat").and_then(|s| stat_times(&s)).map(|t| t.1);
        c.process_ram = read("/proc/self/status")
            .and_then(|s| {
                s.lines().find_map(|l| l.strip_prefix("VmRSS:").and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok()))
            })
            .map(|kb| kb * 1024);
        if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
            for e in dir.flatten() {
                let Some(tid) = e.file_name().to_str().and_then(|t| t.parse::<u64>().ok()) else { continue };
                if let Some(t) = read(&format!("/proc/self/task/{tid}/stat")).and_then(|s| stat_times(&s)) {
                    c.threads.insert(tid, t);
                }
            }
        }
        c
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn a_stat_line_with_spaces_in_the_name() {
            let line = "1234 (tokio-runtime w) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 3 0 100 0 0";
            let (name, t) = super::stat_times(line).unwrap();
            assert_eq!(name, "tokio-runtime w");
            assert_eq!(t, std::time::Duration::from_secs(3));
        }
    }
}

#[cfg(windows)]
mod os {
    use super::Counters;
    use std::time::Duration;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    impl FileTime {
        fn duration(self) -> Duration {
            // (in 100 ns)
            Duration::from_nanos(((self.high as u64) << 32 | self.low as u64) * 100)
        }
    }

    #[repr(C)]
    #[derive(Default)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged_pool: usize,
        quota_paged_pool: usize,
        quota_peak_non_paged_pool: usize,
        quota_non_paged_pool: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ThreadEntry32 {
        size: u32,
        usage: u32,
        thread_id: u32,
        owner_process_id: u32,
        base_priority: i32,
        delta_priority: i32,
        flags: u32,
    }

    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }

    type Handle = isize;
    const TH32CS_SNAPTHREAD: u32 = 0x4;
    const THREAD_QUERY_LIMITED_INFORMATION: u32 = 0x0800;
    const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
    const RRF_RT_REG_SZ: u32 = 0x2;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
        fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
        fn GetCurrentProcess() -> Handle;
        fn GetCurrentProcessId() -> u32;
        fn GetProcessTimes(
            process: Handle,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn K32GetProcessMemoryInfo(process: Handle, counters: *mut ProcessMemoryCounters, cb: u32) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, process: u32) -> Handle;
        fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry32) -> i32;
        fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry32) -> i32;
        fn OpenThread(access: u32, inherit: i32, thread: u32) -> Handle;
        fn GetThreadTimes(thread: Handle, created: *mut FileTime, exited: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime)
            -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetModuleHandleW(name: *const u16) -> Handle;
        fn GetProcAddress(module: Handle, name: *const u8) -> *const core::ffi::c_void;
        fn LocalFree(mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(key: isize, sub: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut u16, size: *mut u32) -> i32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetTickCount64() -> u64;
    }

    pub fn uptime() -> Option<u64> {
        Some(unsafe { GetTickCount64() } / 1000)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(w: &[u16]) -> String {
        String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())])
    }

    pub fn cpu_model() -> Option<String> {
        let mut buf = [0u16; 256];
        let mut size = (buf.len() * 2) as u32;
        let sub = wide("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0");
        let value = wide("ProcessorNameString");
        let r = unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, sub.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr(), &mut size)
        };
        (r == 0).then(|| from_wide(&buf).trim().to_string())
    }

    pub fn name() -> String {
        let mut v =
            OsVersionInfo { size: std::mem::size_of::<OsVersionInfo>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
        if unsafe { RtlGetVersion(&mut v) } == 0 {
            // (Windows 11 still says 10.0: its builds start at 22000)
            let name = if v.major == 10 && v.build >= 22000 { "11".to_string() } else { format!("{}.{}", v.major, v.minor) };
            format!("Windows {name} (build {})", v.build)
        } else {
            "Windows".into()
        }
    }

    type GetThreadDescription = unsafe extern "system" fn(Handle, *mut *mut u16) -> i32;

    /// Thread names (Windows 10 1607 and newer; looked up, so older ones start).
    fn description_fn() -> Option<GetThreadDescription> {
        let module = unsafe { GetModuleHandleW(wide("kernel32.dll").as_ptr()) };
        if module == 0 {
            return None;
        }
        let f = unsafe { GetProcAddress(module, b"GetThreadDescription\0".as_ptr()) };
        (!f.is_null()).then(|| unsafe { std::mem::transmute::<*const core::ffi::c_void, GetThreadDescription>(f) })
    }

    pub fn counters() -> Counters {
        let mut c = Counters::default();
        let (mut idle, mut kernel, mut user) = (FileTime::default(), FileTime::default(), FileTime::default());
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0 {
            // (the kernel time includes the idle time)
            let all = kernel.duration() + user.duration();
            c.system = Some((all.saturating_sub(idle.duration()), all));
        }
        let mut mem = MemoryStatusEx { length: std::mem::size_of::<MemoryStatusEx>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut mem) } != 0 {
            c.ram = Some((mem.total_phys, Some(mem.avail_phys)));
        }
        let process = unsafe { GetCurrentProcess() };
        let (mut created, mut exited) = (FileTime::default(), FileTime::default());
        if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } != 0 {
            c.process = Some(kernel.duration() + user.duration());
        }
        let mut pmc = ProcessMemoryCounters { cb: std::mem::size_of::<ProcessMemoryCounters>() as u32, ..Default::default() };
        if unsafe { K32GetProcessMemoryInfo(process, &mut pmc, pmc.cb) } != 0 {
            c.process_ram = Some(pmc.working_set as u64);
        }
        let describe = description_fn();
        let me = unsafe { GetCurrentProcessId() };
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot != -1 {
            let mut e = ThreadEntry32 { size: std::mem::size_of::<ThreadEntry32>() as u32, ..Default::default() };
            let mut more = unsafe { Thread32First(snapshot, &mut e) } != 0;
            while more {
                if e.owner_process_id == me {
                    let h = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, e.thread_id) };
                    if h != 0 {
                        let (mut cr, mut ex, mut k, mut u) =
                            (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
                        if unsafe { GetThreadTimes(h, &mut cr, &mut ex, &mut k, &mut u) } != 0 {
                            let mut name = String::new();
                            if let Some(f) = describe {
                                let mut p: *mut u16 = std::ptr::null_mut();
                                if unsafe { f(h, &mut p) } >= 0 && !p.is_null() {
                                    let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
                                    name = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
                                    unsafe { LocalFree(p.cast()) };
                                }
                            }
                            c.threads.insert(e.thread_id as u64, (name, k.duration() + u.duration()));
                        }
                        unsafe { CloseHandle(h) };
                    }
                }
                more = unsafe { Thread32Next(snapshot, &mut e) } != 0;
            }
            unsafe { CloseHandle(snapshot) };
        }
        c
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::Counters;
    use std::time::Duration;

    fn sysctl_string(name: &str) -> Option<String> {
        let name = std::ffi::CString::new(name).ok()?;
        let mut size: libc::size_t = 0;
        unsafe {
            if libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 || size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size];
            if libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) != 0 {
                return None;
            }
            Some(String::from_utf8_lossy(&buf[..size]).trim_end_matches('\0').trim().to_string())
        }
    }

    pub fn cpu_model() -> Option<String> {
        sysctl_string("machdep.cpu.brand_string")
    }

    pub fn uptime() -> Option<u64> {
        let name = std::ffi::CString::new("kern.boottime").ok()?;
        let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of::<libc::timeval>();
        if unsafe { libc::sysctlbyname(name.as_ptr(), (&mut tv as *mut libc::timeval).cast(), &mut size, std::ptr::null_mut(), 0) } != 0 {
            return None;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(now.saturating_sub(tv.tv_sec as u64))
    }

    pub fn name() -> String {
        sysctl_string("kern.osproductversion").map(|v| format!("macOS {v}")).unwrap_or_else(|| "macOS".into())
    }

    pub fn counters() -> Counters {
        let mut c = Counters::default();
        let name = std::ffi::CString::new("hw.memsize").unwrap();
        let mut total: u64 = 0;
        let mut size = std::mem::size_of::<u64>();
        if unsafe { libc::sysctlbyname(name.as_ptr(), (&mut total as *mut u64).cast(), &mut size, std::ptr::null_mut(), 0) } == 0 {
            // (the memory in use needs the Mach statistics: the total only)
            c.ram = Some((total, None));
        }
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } == 0 {
            let t = |tv: libc::timeval| Duration::new(tv.tv_sec as u64, (tv.tv_usec as u32) * 1000);
            c.process = Some(t(ru.ru_utime) + t(ru.ru_stime));
        }
        c
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", windows, target_os = "macos")))]
mod os {
    use super::Counters;

    pub fn cpu_model() -> Option<String> {
        None
    }

    pub fn uptime() -> Option<u64> {
        None
    }

    pub fn name() -> String {
        std::env::consts::OS.to_string()
    }

    pub fn counters() -> Counters {
        Counters::default()
    }
}
