//! 本机资源查询：物理内存、解码内存上限。编解码器的内存预算都从这里取，别各写各的。

use std::sync::OnceLock;

/// 物理内存字节数；使用各平台原生查询，拿不到按 8 GB。进程内缓存物理容量。
pub fn physical_memory() -> u64 {
    static MEM: OnceLock<u64> = OnceLock::new();
    *MEM.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            let mut bytes = 0u64;
            let mut len = std::mem::size_of::<u64>();
            // SAFETY: 输出缓冲长度明确，名字是 NUL 结尾的系统只读键，不提供写入缓冲。
            let result = unsafe { libc::sysctlbyname(c"hw.memsize".as_ptr(), (&mut bytes as *mut u64).cast(), &mut len, std::ptr::null_mut(), 0) };
            if result == 0 && len == std::mem::size_of::<u64>() && bytes > 0 {
                return bytes;
            }
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(bytes) = linux_memory("MemTotal:") {
                return bytes;
            }
        }
        #[cfg(target_os = "windows")]
        {
            if let Some((total, _)) = windows_memory() {
                return total;
            }
        }
        8 << 30
    })
}

/// 当前可回收内存；每次压缩重新查询，避免按启动时的空闲值长期分配。
pub fn available_memory() -> u64 {
    #[cfg(target_os = "macos")]
    {
        // libc 将这一个绑定迁到 mach2，但系统 ABI 仍由 mach/mach_init.h 提供；直接声明避免为一次无参查询新增依赖。
        extern "C" {
            fn mach_host_self() -> libc::mach_port_t;
        }
        static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
        // SAFETY: mach_host_self 只取得当前主机的 send right；进程内缓存一次，不在每次查询累积引用。
        let host = *HOST.get_or_init(|| unsafe { mach_host_self() });
        let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: 缓冲按系统结构体分配并清零，count 以 integer_t 为单位，函数同步写入。
        let result = unsafe { libc::host_statistics64(host, libc::HOST_VM_INFO64, stats.as_mut_ptr().cast(), &mut count) };
        if result == libc::KERN_SUCCESS {
            // SAFETY: 成功后基本 VM 字段已初始化，较新系统增加的尾字段不参与计算。
            let stats = unsafe { stats.assume_init() };
            // SDK 的 vm_statistics.h 明确 free_count 已包含 speculative_count，不能重复相加。
            let pages = u64::from(stats.free_count) + u64::from(stats.inactive_count);
            // SAFETY: sysconf 是无指针的系统查询。
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            if page_size > 0 && pages > 0 {
                return pages.saturating_mul(page_size as u64);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(bytes) = linux_memory("MemAvailable:") {
            return bytes;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some((_, available)) = windows_memory() {
            return available;
        }
    }
    physical_memory() / 4
}

/// 编码预算取物理内存一半与当前可用内存四分之三中的较小值，给系统及其他应用留余量。
pub fn compression_budget() -> u64 {
    (physical_memory() / 2).min(available_memory().saturating_mul(3) / 4).max(64 << 20)
}

/// 按 CPU 与每线程最坏内存估计决定并行度；显式线程数同样受内存预算约束。
pub fn compression_threads(requested: usize, bytes_per_worker: u64) -> usize {
    let cpu = if requested == 0 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) } else { requested };
    (compression_budget() / bytes_per_worker.max(1)).clamp(1, cpu.max(1) as u64) as usize
}

/// Linux 内核提供物理内存与当前可用内存，单位为 KiB。
#[cfg(target_os = "linux")]
fn linux_memory(key: &str) -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo").ok()?.lines().find(|line| line.starts_with(key))?.split_whitespace().nth(1)?.parse::<u64>().ok()?.checked_mul(1024)
}

/// Windows 原生内存快照；布局对应 MEMORYSTATUSEX。
#[cfg(target_os = "windows")]
fn windows_memory() -> Option<(u64, u64)> {
    #[repr(C)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        values: [u64; 7],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
    }
    let mut status = MemoryStatus { length: std::mem::size_of::<MemoryStatus>() as u32, load: 0, values: [0; 7] };
    // SAFETY: 结构体长度、对齐和字段布局与系统 ABI 一致，指针在调用期间有效。
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some((status.values[0], status.values[1]))
}

/// 解码单条流允许的内存上限：物理内存的一半，夹在 256 MB 到 2 GB 之间。
/// .xz / .lzma 头里可以声明 4 GB 字典，解码器开场就按它分配；不设限就是几十字节的恶意输入换几 GB 内存（模糊测试抓到过）。
/// 上限取 2 GB 是因为 xz 编码器的字典最大 1.5 GB，再大的声明没有合法来源。
pub fn decode_memlimit() -> u64 {
    (physical_memory() / 2).clamp(256 << 20, 2 << 30)
}
