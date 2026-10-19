//! 模糊测试抓到的样本回放：fuzz/regressions/ 下每个文件都按 fuzz/fuzz_targets/open_any.rs 的规则跑一遍
//! （首字节选扩展名 → 打开 → 规划 → 校验），只允许返回错误，不允许 panic、卡死、吃光内存。
//! 新样本直接把 fuzz/artifacts/open_any/ 里的文件拷进来并起个说明性的名字。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use piesi_core::facade::Archive;
use piesi_core::plan::ExtractOptions;
use piesi_core::progress::NoProgress;
use piesi_core::{CancelToken, OpContext};

/// 和 open_any.rs 里的表一致。
const EXTS: &[&str] = &["zip", "7z", "rar", "tar", "tar.gz", "tar.xz", "tar.zst", "tar.bz2", "gz", "xz", "zst", "bz2", "tgz", "jar", "zip.001", "7z.001"];

/// 跑一个样本，返回耗时。
fn replay(sample: &[u8], scratch: &Path) -> Duration {
    let started = Instant::now();
    let Some((&selector, body)) = sample.split_first() else { return started.elapsed() };
    let path = scratch.join(format!("sample.{}", EXTS[selector as usize % EXTS.len()]));
    std::fs::write(&path, body).unwrap();
    if let Ok(mut archive) = Archive::open(&path, Some("zh-CN")) {
        let _ = archive.names().len();
        let _ = archive.plan(None, &ExtractOptions::beside_archive(&path));
        let progress = NoProgress;
        let token = CancelToken::new();
        let ctx = OpContext::new(&progress, &token);
        let _ = archive.verify(None, &ctx);
    }
    started.elapsed()
}

#[test]
fn fuzz_regressions_do_not_panic() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions");
    let scratch: PathBuf = std::env::temp_dir().join(format!("piesi-fuzz-regress-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let sample = std::fs::read(&path).unwrap();
        let took = replay(&sample, &scratch);
        // 样本都是几十字节到几十 KB，超过 5 秒说明有死循环或巨量分配
        assert!(took < Duration::from_secs(5), "{} 跑了 {took:?}", path.display());
        count += 1;
    }
    std::fs::remove_dir_all(&scratch).unwrap();
    assert!(count >= 1, "fuzz/regressions 里没有样本");
}
