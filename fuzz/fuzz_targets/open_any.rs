#![no_main]
//! 模糊测试：任意字节当压缩包打开 → 列目录 → 规划 → 校验（解到丢弃式 sink，走完解码和 CRC）。
//! 不可信输入只允许返回错误，不允许 panic、死循环、吃光内存。时间和内存上限由 libFuzzer 的 -timeout / -rss_limit_mb 管。

use std::io::Write;

use libfuzzer_sys::fuzz_target;
use piesi_core::facade::Archive;
use piesi_core::plan::ExtractOptions;
use piesi_core::progress::NoProgress;
use piesi_core::{CancelToken, OpContext};

/// 第一个字节选扩展名：tar 系和单文件压缩流没有可靠魔数，靠扩展名兜底才走得到那些驱动。
const EXTS: &[&str] = &["zip", "7z", "rar", "tar", "tar.gz", "tar.xz", "tar.zst", "tar.bz2", "gz", "xz", "zst", "bz2", "tgz", "jar", "zip.001", "7z.001"];

fuzz_target!(|data: &[u8]| {
    let Some((&selector, body)) = data.split_first() else { return };
    let ext = EXTS[selector as usize % EXTS.len()];
    // 驱动按路径打开（分卷要找邻居文件），所以每轮写一个临时文件；文件名带 pid，多 job 并行不打架
    let dir = std::env::temp_dir().join("piesi-fuzz");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.{ext}", std::process::id()));
    std::fs::File::create(&path).and_then(|mut f| f.write_all(body)).expect("写临时文件");

    let Ok(mut archive) = Archive::open(&path, Some("zh-CN")) else { return };
    let _ = archive.names().len();
    let _ = archive.detection();
    let _ = archive.plan(None, &ExtractOptions::beside_archive(&path));
    let progress = NoProgress;
    let token = CancelToken::new();
    let ctx = OpContext::new(&progress, &token);
    let _ = archive.verify(None, &ctx);
});
