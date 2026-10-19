//! ZIP 写入器集成测试：用自己的读取器回读，覆盖内存路径、溢出到临时文件的大文件路径、不可压缩改存储。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use piesi_core::archive::{Entry, EntryKind, EntrySink, Method, PasswordNeed};
use piesi_core::create::{create_archive, CreateFormat, CreateOptions};
use piesi_core::progress::OpContext;
use piesi_core::WarningKind;

/// 把条目内容收进内存并算 CRC 的 sink。
#[derive(Default)]
struct MemSink {
    files: Vec<(String, u32, u64)>,
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
    fn symlink(&mut self, _entry: &Entry, _target: &[u8]) -> piesi_core::Result<()> {
        Ok(())
    }
}

/// 临时工作目录。
fn workdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("piesi-zipw-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 65 MB 可压缩文件走溢出路径，1 MB 随机数据走「没变小改存储」，小文本走内存路径；全部回读校验 CRC。
#[test]
fn spill_store_and_memory_paths_roundtrip() {
    let work = workdir("mix");
    let src = work.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    // 65 MB：超过 64 MB 的溢出阈值；内容有规律，deflate 会明显变小
    let chunk: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let mut big = std::io::BufWriter::new(std::fs::File::create(src.join("big.bin")).unwrap());
    for _ in 0..(65 * 1024 * 1024 / 4096) {
        big.write_all(&chunk).unwrap();
    }
    big.flush().unwrap();
    drop(big);
    // 1 MB 伪随机：不可压缩，应改为存储
    let mut x = 0x9e3779b97f4a7c15u64;
    let random: Vec<u8> = (0..1 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 56) as u8
        })
        .collect();
    std::fs::write(src.join("sub/random.bin"), &random).unwrap();
    std::fs::write(src.join("sub/小文本.txt"), "你好 ".repeat(1000)).unwrap();
    std::fs::write(src.join(".DS_Store"), "junk").unwrap();

    let out = work.join("out.zip");
    let opts = CreateOptions { format: CreateFormat::Zip, level: 5, ..Default::default() };
    let report = create_archive(&out, std::slice::from_ref(&src), &opts, &OpContext::silent()).unwrap();
    assert_eq!(report.skipped_junk, 1);
    assert!(report.output_bytes < report.input_bytes / 2, "65 MB 规律数据应明显压缩：{} / {}", report.output_bytes, report.input_bytes);
    // 临时文件必须已清理
    let leftovers: Vec<_> = std::fs::read_dir(&work).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("piesi-zip-")).collect();
    assert!(leftovers.is_empty(), "残留临时文件：{leftovers:?}");

    let mut reader = piesi_core::open(&out).unwrap();
    let entries = reader.entries().to_vec();
    let by_name = |n: &str| entries.iter().find(|e| String::from_utf8_lossy(&e.raw_name).ends_with(n)).cloned().unwrap();
    assert_eq!(by_name("big.bin").method, Method::Deflate);
    assert_eq!(by_name("random.bin").method, Method::Store, "不可压缩数据应改为存储");
    assert!(entries.iter().all(|e| e.name_is_utf8), "所有条目都应带 UTF-8 标志");
    assert!(entries.iter().any(|e| e.kind == EntryKind::Dir));

    let mut sink = MemSink::default();
    let rep = reader.extract(None, &mut sink, &OpContext::silent()).unwrap();
    // 驱动的计数含目录条目（2 个目录 + 3 个文件）；真正落盘的文件数以 sink 为准
    assert_eq!(rep.extracted, 5);
    assert_eq!(sink.files.len(), 3);
    for (name, crc, n) in &sink.files {
        let original = if name.ends_with("big.bin") {
            std::fs::read(src.join("big.bin")).unwrap()
        } else if name.ends_with("random.bin") {
            random.clone()
        } else {
            std::fs::read(src.join("sub/小文本.txt")).unwrap()
        };
        assert_eq!(*n, original.len() as u64, "{name} 大小不符");
        assert_eq!(*crc, crc32fast::hash(&original), "{name} CRC 不符");
    }
    std::fs::remove_dir_all(&work).unwrap();
}

/// xorshift 伪随机字节：不可压缩的数据。
fn pseudo_random(n: usize) -> Vec<u8> {
    let mut x = 0x9e3779b97f4a7c15u64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 56) as u8
        })
        .collect()
}

/// 带密码的 zip：文件、链接全部 AES-256，目录条目不加密；短于 20 字节走 AE-2（无 CRC），其余 AE-1；
/// 错密码拒绝，正确密码回读 CRC 一致；本机有 7zz 时让它校验并解压比对。
#[test]
fn aes_encrypted_roundtrip() {
    let work = workdir("aes");
    let src = work.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("tiny.txt"), "hi").unwrap();
    std::fs::write(src.join("empty.txt"), "").unwrap();
    std::fs::write(src.join("sub/text.txt"), "lorem ipsum ".repeat(5000)).unwrap();
    let random = pseudo_random(1 << 16);
    std::fs::write(src.join("sub/random.bin"), &random).unwrap();
    std::os::unix::fs::symlink("text.txt", src.join("sub/link")).unwrap();

    let out = work.join("enc.zip");
    let password = "s3cret-Pass";
    let opts = CreateOptions { format: CreateFormat::Zip, level: 5, password: Some(password.into()), encrypt_names: true, ..Default::default() };
    let report = create_archive(&out, std::slice::from_ref(&src), &opts, &OpContext::silent()).unwrap();
    assert_eq!(report.warnings.len(), 1, "encrypt_names 对 zip 无效应有提示：{:?}", report.warnings);
    assert_eq!(report.warnings[0].kind, WarningKind::ZipCannotEncryptNames);

    let mut reader = piesi_core::open(&out).unwrap();
    assert_ne!(reader.needs_password(), PasswordNeed::None);
    let entries = reader.entries().to_vec();
    let name_of = |e: &Entry| String::from_utf8_lossy(&e.raw_name).into_owned();
    let by_name = |n: &str| entries.iter().find(|e| name_of(e).ends_with(n)).cloned().unwrap_or_else(|| panic!("缺少 {n}"));
    for e in &entries {
        assert_eq!(e.encrypted, e.kind != EntryKind::Dir, "{}：只有目录条目不加密", name_of(e));
    }
    assert_eq!(by_name("tiny.txt").crc32, None, "短文件应用 AE-2，不写 CRC");
    assert_eq!(by_name("empty.txt").crc32, None);
    assert!(by_name("text.txt").crc32.is_some(), "长文件应用 AE-1，保留 CRC");
    assert_eq!(by_name("text.txt").method, Method::Deflate);
    assert_eq!(by_name("random.bin").method, Method::Store, "不可压缩数据应改为存储");

    // 错密码：设置时或解压时必须报 WrongPassword
    let wrong = reader.set_password(Some("wrong")).err().or_else(|| reader.extract(None, &mut MemSink::default(), &OpContext::silent()).err());
    assert!(matches!(wrong, Some(piesi_core::Error::WrongPassword)), "{wrong:?}");

    reader.set_password(Some(password)).unwrap();
    let mut sink = MemSink::default();
    reader.extract(None, &mut sink, &OpContext::silent()).unwrap();
    assert_eq!(sink.files.len(), 4);
    for (name, crc, n) in &sink.files {
        let rel = name.split_once('/').map(|(_, r)| r).unwrap_or(name);
        let original = std::fs::read(src.join(rel)).unwrap();
        assert_eq!(*n, original.len() as u64, "{name} 大小不符");
        assert_eq!(*crc, crc32fast::hash(&original), "{name} CRC 不符");
    }

    // 7zz 交叉验证：能校验、错密码拒绝、解出的内容一致
    if let Some(sevenz) = ["/opt/homebrew/bin/7zz", "/usr/local/bin/7zz"].into_iter().find(|p| Path::new(p).exists()) {
        let run = |args: &[&str]| Command::new(sevenz).args(args).output().unwrap();
        let t = run(&["t", &format!("-p{password}"), "-y", out.to_str().unwrap()]);
        assert!(t.status.success(), "7zz t 失败：{}", String::from_utf8_lossy(&t.stdout));
        assert!(!run(&["t", "-pwrong", "-y", out.to_str().unwrap()]).status.success(), "7zz 用错密码不该通过");
        let dest = work.join("7zz-out");
        let x = run(&["x", &format!("-p{password}"), "-y", &format!("-o{}", dest.display()), out.to_str().unwrap()]);
        assert!(x.status.success(), "7zz x 失败：{}", String::from_utf8_lossy(&x.stdout));
        assert_eq!(std::fs::read(dest.join("src/tiny.txt")).unwrap(), b"hi");
        assert_eq!(std::fs::read(dest.join("src/empty.txt")).unwrap(), b"");
        assert_eq!(std::fs::read(dest.join("src/sub/random.bin")).unwrap(), random);
        assert_eq!(std::fs::read(dest.join("src/sub/text.txt")).unwrap(), std::fs::read(src.join("sub/text.txt")).unwrap());
    }
    std::fs::remove_dir_all(&work).unwrap();
}

/// 非 ASCII 密码按 UTF-8 字节派生密钥，自己能回读。
#[test]
fn aes_non_ascii_password() {
    let work = workdir("aes-utf8");
    let src = work.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.txt"), "内容 content\n".repeat(100)).unwrap();
    let out = work.join("enc.zip");
    let opts = CreateOptions { format: CreateFormat::Zip, level: 1, password: Some("密码 pässwörd".into()), ..Default::default() };
    create_archive(&out, std::slice::from_ref(&src), &opts, &OpContext::silent()).unwrap();
    let mut reader = piesi_core::open(&out).unwrap();
    reader.set_password(Some("密码 pässwörd")).unwrap();
    let mut sink = MemSink::default();
    reader.extract(None, &mut sink, &OpContext::silent()).unwrap();
    assert_eq!(sink.files.len(), 1);
    assert_eq!(sink.files[0].1, crc32fast::hash(&std::fs::read(src.join("a.txt")).unwrap()));
    std::fs::remove_dir_all(&work).unwrap();
}
