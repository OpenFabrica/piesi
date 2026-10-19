//! 7z 写入器集成测试：多 solid 块并行、加密与文件名加密，用自己的读取器回读；有 7zz 时再交叉校验。

use std::path::{Path, PathBuf};

use piesi_core::archive::{Entry, EntrySink, PasswordNeed};
use piesi_core::create::{create_archive, CreateFormat, CreateOptions};
use piesi_core::progress::OpContext;

/// 收集条目内容 CRC 的 sink。
#[derive(Default)]
struct MemSink {
    files: Vec<(String, u32, u64)>,
    links: Vec<(String, Vec<u8>)>,
}

impl EntrySink for MemSink {
    fn dir(&mut self, _entry: &Entry) -> piesi_core::Result<()> {
        Ok(())
    }
    fn file(&mut self, entry: &Entry, data: &mut dyn std::io::Read) -> piesi_core::Result<u64> {
        let mut hasher = crc32fast::Hasher::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut n = 0u64;
        loop {
            let k = data.read(&mut buf)?;
            if k == 0 {
                break;
            }
            hasher.update(&buf[..k]);
            n += k as u64;
        }
        self.files.push((String::from_utf8_lossy(&entry.raw_name).into_owned(), hasher.finalize(), n));
        Ok(n)
    }
    fn symlink(&mut self, entry: &Entry, target: &[u8]) -> piesi_core::Result<()> {
        self.links.push((String::from_utf8_lossy(&entry.raw_name).into_owned(), target.to_vec()));
        Ok(())
    }
}

/// 生成 40 个 8–40 KB 的伪随机但可压缩文件、一个空文件、一个符号链接、嵌套目录。
fn make_tree(tag: &str) -> (PathBuf, PathBuf, Vec<(String, Vec<u8>)>) {
    let work = std::env::temp_dir().join(format!("piesi-7zw-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let src = work.join("树");
    std::fs::create_dir_all(src.join("a/b")).unwrap();
    let mut files = Vec::new();
    let mut x = 0x1234_5678_9abc_def0u64;
    for i in 0..40 {
        let len = 8 * 1024 + (i * 811) % (32 * 1024);
        let mut data = Vec::with_capacity(len);
        while data.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // 只用 4 个字节值（2 bit 熵），保证明显可压缩
            data.push(b'a' + (x % 4) as u8);
        }
        let rel = if i % 3 == 0 { format!("a/b/文件{i}.txt") } else { format!("a/文件{i}.txt") };
        std::fs::write(src.join(&rel), &data).unwrap();
        files.push((format!("树/{rel}"), data));
    }
    std::fs::write(src.join("空.txt"), b"").unwrap();
    std::os::unix::fs::symlink("a/文件1.txt", src.join("链接")).unwrap();
    (work, src, files)
}

/// 回读并逐文件校验 CRC。
fn verify(out: &Path, password: Option<&str>, files: &[(String, Vec<u8>)]) {
    let mut reader = piesi_core::open(out).unwrap();
    if let Some(pw) = password {
        assert_ne!(reader.needs_password(), PasswordNeed::None);
        reader.set_password(Some(pw)).unwrap();
    }
    let groups: std::collections::HashSet<_> = reader.entries().iter().filter_map(|e| e.solid_group).collect();
    assert!(groups.len() >= 3, "应切成多个 solid 块，实际 {} 个", groups.len());
    let mut sink = MemSink::default();
    reader.extract(None, &mut sink, &OpContext::silent()).unwrap();
    assert_eq!(sink.files.len(), files.len() + 1, "文件数（含空文件）");
    assert_eq!(sink.links.len(), 1);
    assert_eq!(sink.links[0].1, "a/文件1.txt".as_bytes());
    for (name, data) in files {
        let got = sink.files.iter().find(|(n, _, _)| n == name).unwrap_or_else(|| panic!("缺少 {name}"));
        assert_eq!(got.2, data.len() as u64, "{name} 大小");
        assert_eq!(got.1, crc32fast::hash(data), "{name} CRC");
    }
}

#[test]
fn parallel_solid_blocks_roundtrip() {
    let (work, src, files) = make_tree("plain");
    let out = work.join("out.7z");
    let opts = CreateOptions { format: CreateFormat::SevenZ, level: 5, solid_block_bytes: 200 * 1024, threads: 4, ..Default::default() };
    let report = create_archive(&out, std::slice::from_ref(&src), &opts, &OpContext::silent()).unwrap();
    assert!(report.output_bytes < report.input_bytes * 6 / 10, "{} / {}", report.output_bytes, report.input_bytes);
    verify(&out, None, &files);
    if let Ok(status) = std::process::Command::new("/opt/homebrew/bin/7zz").args(["t", "-bso0", "-bsp0"]).arg(&out).status() {
        assert!(status.success(), "7zz 校验失败");
    }
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn encrypted_with_header_roundtrip() {
    let (work, src, files) = make_tree("enc");
    let out = work.join("enc.7z");
    let opts = CreateOptions {
        format: CreateFormat::SevenZ,
        level: 3,
        solid_block_bytes: 200 * 1024,
        threads: 3,
        password: Some("密码 secret".into()),
        encrypt_names: true,
        ..Default::default()
    };
    create_archive(&out, std::slice::from_ref(&src), &opts, &OpContext::silent()).unwrap();
    let reader = piesi_core::open(&out).unwrap();
    assert_eq!(reader.needs_password(), PasswordNeed::ForNames, "文件名应已加密");
    assert!(reader.entries().is_empty());
    verify(&out, Some("密码 secret"), &files);
    if let Ok(status) = std::process::Command::new("/opt/homebrew/bin/7zz").args(["t", "-bso0", "-bsp0", "-p密码 secret"]).arg(&out).status() {
        assert!(status.success(), "7zz 校验加密包失败");
    }
    std::fs::remove_dir_all(&work).unwrap();
}
