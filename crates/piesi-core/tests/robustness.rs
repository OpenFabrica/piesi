//! 健壮性：把所有夹具做截断和随机翻转后喂给引擎，要求只能返回错误，不能 panic、不能死循环。
//! 这是 fuzz 之前的廉价防线；真正的 cargo-fuzz 目标以后再加。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use piesi_core::archive::{Entry, EntrySink};
use piesi_core::progress::OpContext;

/// 丢弃一切的 sink。
#[derive(Default)]
struct NullSink;

impl EntrySink for NullSink {
    fn dir(&mut self, _entry: &Entry) -> piesi_core::Result<()> {
        Ok(())
    }
    fn file(&mut self, _entry: &Entry, data: &mut dyn std::io::Read) -> piesi_core::Result<u64> {
        Ok(std::io::copy(data, &mut std::io::sink())?)
    }
    fn symlink(&mut self, _entry: &Entry, _target: &[u8]) -> piesi_core::Result<()> {
        Ok(())
    }
}

/// 确定性的线性同余伪随机数，测试可复现。
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// 收集所有夹具文件（分卷只取首卷，避免把 .002 当独立包）。
fn fixtures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    for dir in ["tests/fixtures/zip", "tests/fixtures/sevenz", "tests/fixtures/rar", "tests/fixtures/tar", "../../tests/corpus/encoding"] {
        let Ok(rd) = std::fs::read_dir(root.join(dir)) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if !p.is_file() || name.ends_with(".sh") || name.ends_with(".json") || name.ends_with(".md") {
                continue;
            }
            if name.ends_with(".002") || name.ends_with(".003") || name.ends_with(".z01") || name.ends_with(".z02") {
                continue;
            }
            out.push(p);
        }
    }
    out.sort();
    out
}

/// 对一份字节做「打开 + 解压」，只关心不 panic、不超时。
fn exercise(bytes: &[u8], ext: &str, work: &Path, idx: usize) {
    let path = work.join(format!("case-{idx}.{ext}"));
    std::fs::write(&path, bytes).unwrap();
    let started = Instant::now();
    if let Ok(mut reader) = piesi_core::open(&path) {
        let _ = reader.set_password(Some("TEST"));
        let _ = reader.extract(None, &mut NullSink, &OpContext::silent());
    }
    assert!(started.elapsed() < Duration::from_secs(20), "处理畸形输入超过 20 秒：{}", path.display());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn truncated_and_flipped_fixtures_never_panic() {
    let work = std::env::temp_dir().join(format!("piesi-robust-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let files = fixtures();
    assert!(files.len() > 20, "夹具太少：{}", files.len());
    let mut rng = Lcg(0x5eed);
    let mut cases = 0usize;
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let ext = f.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        if bytes.is_empty() {
            continue;
        }
        // 截断：头部一小段、中间、尾部缺几个字节
        for cut in [bytes.len() / 7, bytes.len() / 2, bytes.len().saturating_sub(3)] {
            exercise(&bytes[..cut], &ext, &work, cases);
            cases += 1;
        }
        // 翻转：每份 6 轮，每轮改 1–8 个随机字节
        for _ in 0..6 {
            let mut m = bytes.clone();
            let n = 1 + (rng.next() % 8) as usize;
            for _ in 0..n {
                let i = (rng.next() as usize) % m.len();
                m[i] ^= 1 << (rng.next() % 8);
            }
            exercise(&m, &ext, &work, cases);
            cases += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    eprintln!("健壮性用例：{cases}");
}
