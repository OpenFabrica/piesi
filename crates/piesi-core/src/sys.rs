//! 本机资源查询：物理内存、解码内存上限。编解码器的内存预算都从这里取，别各写各的。

use std::sync::OnceLock;

/// 物理内存字节数；macOS 用 sysctl hw.memsize，拿不到按 8 GB。只查一次，之后用缓存（模糊测试每轮都会调）。
pub fn physical_memory() -> u64 {
    static MEM: OnceLock<u64> = OnceLock::new();
    *MEM.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            if let Ok(out) = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).output() {
                if let Some(v) = String::from_utf8(out.stdout).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
                    return v;
                }
            }
        }
        8 << 30
    })
}

/// 解码单条流允许的内存上限：物理内存的一半，夹在 256 MB 到 2 GB 之间。
/// .xz / .lzma 头里可以声明 4 GB 字典，解码器开场就按它分配；不设限就是几十字节的恶意输入换几 GB 内存（模糊测试抓到过）。
/// 上限取 2 GB 是因为 xz 编码器的字典最大 1.5 GB，再大的声明没有合法来源。
pub fn decode_memlimit() -> u64 {
    (physical_memory() / 2).clamp(256 << 20, 2 << 30)
}
