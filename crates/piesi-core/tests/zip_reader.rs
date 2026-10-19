//! zip 读取器集成测试：逐个打开夹具、列目录、全部解压到内存 sink 并校验内容与 CRC。
//! 夹具由 `tests/fixtures/zip/gen.sh` 生成，源文件在 `tests/fixtures/zip/src/`。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use piesi_core::archive::{ArchiveReader, Entry, EntryKind, EntrySink, Method, PasswordNeed};
use piesi_core::fmt::zip::ZipReader;
use piesi_core::progress::{CancelToken, OpContext, Progress, ProgressEvent};
use piesi_core::{Error, Result};

/// 夹具里 UT/NTFS 时间戳对应的 Unix 秒：2024-01-02 15:30:00 Asia/Shanghai。
const STAMP_UTC: i64 = 1_704_180_600;
/// 同一时刻按 UTC 解释 DOS 字段得到的值；本地换算结果与它的差在 ±14 小时内。
const STAMP_AS_UTC: i64 = 1_704_209_400;

/// 夹具目录。
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zip")
}

/// 夹具路径。
fn fx(name: &str) -> PathBuf {
    fixtures().join(name)
}

/// 源文件内容（条目名 → 期望内容）。
fn src(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join("src").join(name)).unwrap_or_else(|e| panic!("read src/{name}: {e}"))
}

/// 条目显示名：优先 0x7075 的 UTF-8 名。
fn name_of(e: &Entry) -> String {
    e.unicode_name.clone().unwrap_or_else(|| String::from_utf8_lossy(&e.raw_name).into_owned())
}

/// 内存接收端：收集目录、文件内容、符号链接。
#[derive(Default)]
struct MemSink {
    dirs: Vec<String>,
    files: HashMap<String, Vec<u8>>,
    links: HashMap<String, Vec<u8>>,
}

impl EntrySink for MemSink {
    fn dir(&mut self, e: &Entry) -> Result<()> {
        self.dirs.push(name_of(e));
        Ok(())
    }

    fn file(&mut self, e: &Entry, data: &mut dyn Read) -> Result<u64> {
        let mut v = Vec::new();
        data.read_to_end(&mut v)?;
        let n = v.len() as u64;
        self.files.insert(name_of(e), v);
        Ok(n)
    }

    fn symlink(&mut self, e: &Entry, target: &[u8]) -> Result<()> {
        self.links.insert(name_of(e), target.to_vec());
        Ok(())
    }
}

/// 打开、全部解压，并校验每个文件条目的长度与 CRC。
fn extract_all(path: &Path, password: Option<&str>) -> (ZipReader, MemSink) {
    let mut r = ZipReader::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    if let Some(p) = password {
        r.set_password(Some(p)).unwrap();
    }
    let mut sink = MemSink::default();
    let report = r
        .extract(None, &mut sink, &OpContext::silent())
        .unwrap_or_else(|e| panic!("extract {}: {e}", path.display()));
    assert_eq!(report.extracted as usize, r.entries().len());
    for e in r.entries() {
        if e.kind == EntryKind::File {
            let data = &sink.files[&name_of(e)];
            assert_eq!(data.len() as u64, e.size, "size of {}", name_of(e));
            if let Some(crc) = e.crc32 {
                assert_eq!(crc32fast::hash(data), crc, "crc of {}", name_of(e));
            }
        }
    }
    (r, sink)
}

/// 按名字找条目。
fn entry<'a>(r: &'a ZipReader, name: &str) -> &'a Entry {
    r.entries().iter().find(|e| name_of(e) == name).unwrap_or_else(|| panic!("no entry {name}"))
}

/// 标准四件套内容校验。
fn check_std_files(sink: &MemSink) {
    assert_eq!(sink.files["hello.txt"], src("hello.txt"));
    assert_eq!(sink.files["empty.txt"], src("empty.txt"));
    assert_eq!(sink.files["bin.dat"], src("bin.dat"));
    assert_eq!(sink.files["data/lorem.txt"], src("data/lorem.txt"));
}

/// 7zz 写的各压缩方法：内容、方法号、目录条目、NTFS 时间戳。
#[test]
fn methods_from_7zz() {
    let cases = [
        ("store.zip", Method::Store),
        ("deflate.zip", Method::Deflate),
        ("deflate64.zip", Method::Deflate64),
        ("bzip2.zip", Method::Bzip2),
        ("lzma.zip", Method::Lzma),
        ("xz.zip", Method::Xz),
        ("ppmd.zip", Method::Ppmd),
    ];
    for (file, method) in cases {
        let (r, sink) = extract_all(&fx(file), None);
        check_std_files(&sink);
        let lorem = entry(&r, "data/lorem.txt");
        assert_eq!(lorem.method, method, "{file}");
        assert_eq!(lorem.mtime, Some(STAMP_UTC), "{file} ntfs mtime");
        assert!(lorem.packed_size.unwrap() < lorem.size || method == Method::Store, "{file} should compress");
        let dir = entry(&r, "data/");
        assert_eq!(dir.kind, EntryKind::Dir, "{file}");
        assert!(sink.dirs.contains(&"data/".to_string()), "{file}");
        assert_eq!(entry(&r, "empty.txt").size, 0);
        assert!(!lorem.encrypted);
        assert_eq!(r.needs_password(), PasswordNeed::None);
    }
}

/// 手工拼的 zstd（方法 93）。
#[test]
fn zstd_method() {
    let (r, sink) = extract_all(&fx("zstd.zip"), None);
    assert_eq!(entry(&r, "data/lorem.txt").method, Method::Zstd);
    assert_eq!(sink.files["data/lorem.txt"], src("data/lorem.txt"));
    assert_eq!(sink.files["hello.txt"], src("hello.txt"));
}

/// WinZip AES：无密码、错密码、对密码；AE-2 无 CRC，AE-1 有 CRC。
#[test]
fn aes_entries() {
    for file in ["aes256.zip", "aes128.zip", "aes256_ae1.zip"] {
        let mut r = ZipReader::open(&fx(file)).unwrap();
        assert_eq!(r.needs_password(), PasswordNeed::ForData, "{file}");
        let hello = entry(&r, "hello.txt");
        assert!(hello.encrypted);
        assert!(matches!(hello.method, Method::Store | Method::Deflate), "{file}: {:?}", hello.method);
        let ae1 = file == "aes256_ae1.zip";
        assert_eq!(hello.crc32.is_some(), ae1, "{file} crc presence");

        let mut sink = MemSink::default();
        let e = r.extract(None, &mut sink, &OpContext::silent()).unwrap_err();
        assert!(matches!(e, Error::PasswordRequired), "{file}: {e}");
        r.set_password(Some("WRONG")).unwrap();
        let e = r.extract(None, &mut sink, &OpContext::silent()).unwrap_err();
        assert!(matches!(e, Error::WrongPassword), "{file}: {e}");

        let (_, sink) = extract_all(&fx(file), Some("TEST"));
        assert_eq!(sink.files["hello.txt"], src("hello.txt"));
        if file == "aes128.zip" {
            assert_eq!(sink.files["bin.dat"], src("bin.dat"));
        } else {
            assert_eq!(sink.files["data/lorem.txt"], src("data/lorem.txt"));
            // 空文件也走完整的盐 + 校验值 + 认证码流程
            assert_eq!(sink.files["empty.txt"], b"");
        }
    }
}

/// 篡改 AES 密文一个字节：解压后认证码对不上，必须报损坏。
#[test]
fn aes_tamper_is_corrupt() {
    let mut data = std::fs::read(fx("aes256.zip")).unwrap();
    // 第一个本地头在 0：30 + 名字 + 扩展 后面是 16 字节盐 + 2 字节校验值，再往后就是密文
    let name_len = u16::from_le_bytes([data[26], data[27]]) as usize;
    let extra_len = u16::from_le_bytes([data[28], data[29]]) as usize;
    let ct = 30 + name_len + extra_len + 16 + 2;
    data[ct] ^= 0x55;
    let dir = tempdir("aes-tamper");
    let p = dir.join("t.zip");
    std::fs::write(&p, &data).unwrap();
    let mut r = ZipReader::open(&p).unwrap();
    r.set_password(Some("TEST")).unwrap();
    let mut sink = MemSink::default();
    let e = r.extract(None, &mut sink, &OpContext::silent()).unwrap_err();
    assert!(matches!(e, Error::Corrupt(_)), "{e}");
    std::fs::remove_dir_all(dir).ok();
}

/// ZipCrypto：无密码、错密码、对密码；stdin 产物带 data descriptor，校验字节走 DOS 时间。
#[test]
fn zipcrypto_entries() {
    let mut r = ZipReader::open(&fx("zipcrypto.zip")).unwrap();
    assert_eq!(r.needs_password(), PasswordNeed::ForData);
    let mut sink = MemSink::default();
    assert!(matches!(r.extract(None, &mut sink, &OpContext::silent()), Err(Error::PasswordRequired)));
    r.set_password(Some("nope")).unwrap();
    assert!(matches!(r.extract(None, &mut sink, &OpContext::silent()), Err(Error::WrongPassword)));

    let (r, sink) = extract_all(&fx("zipcrypto.zip"), Some("TEST"));
    assert!(entry(&r, "bin.dat").encrypted);
    assert_eq!(sink.files["hello.txt"], src("hello.txt"));
    assert_eq!(sink.files["bin.dat"], src("bin.dat"));
    assert_eq!(sink.files["data/lorem.txt"], src("data/lorem.txt"));

    let (r, sink) = extract_all(&fx("zipcrypto_dd.zip"), Some("TEST"));
    assert_eq!(r.entries().len(), 1);
    assert_eq!(sink.files["-"], src("data/lorem.txt"));
    let mut r = ZipReader::open(&fx("zipcrypto_dd.zip")).unwrap();
    r.set_password(Some("nope")).unwrap();
    assert!(matches!(r.extract(None, &mut MemSink::default(), &OpContext::silent()), Err(Error::WrongPassword)));
}

/// data descriptor（zip 从 stdin 写）：正常打开与扫描重建都能读出同样的内容。
#[test]
fn data_descriptor_and_scan() {
    let (r, sink) = extract_all(&fx("datadesc.zip"), None);
    assert_eq!(r.entries().len(), 1);
    assert_eq!(sink.files["-"], src("data/lorem.txt"));

    let mut s = ZipReader::open_by_scan(&fx("datadesc.zip")).unwrap();
    assert_eq!(s.entries().len(), 1);
    assert_eq!(s.entries()[0].size, src("data/lorem.txt").len() as u64);
    assert_eq!(s.entries()[0].crc32, Some(crc32fast::hash(&src("data/lorem.txt"))));
    let mut sink = MemSink::default();
    s.extract(None, &mut sink, &OpContext::silent()).unwrap();
    assert_eq!(sink.files["-"], src("data/lorem.txt"));
}

/// zip64：python 的本地头 zip64 与手工拼的全量 zip64（目录头与 EOCD 全部饱和）。
#[test]
fn zip64_variants() {
    for file in ["zip64_py.zip", "zip64_full.zip"] {
        let (r, sink) = extract_all(&fx(file), None);
        assert_eq!(r.entries().len(), 2, "{file}");
        assert_eq!(sink.files["hello.txt"], src("hello.txt"), "{file}");
        assert_eq!(sink.files["data/lorem.txt"], src("data/lorem.txt"), "{file}");
        assert_eq!(entry(&r, "data/lorem.txt").size, src("data/lorem.txt").len() as u64);
    }
}

/// 文件名编码：bit 11 UTF-8、GBK + 0x7075、裸 GBK。原始字节必须原样保留。
#[test]
fn name_encodings() {
    let (r, sink) = extract_all(&fx("utf8_names.zip"), None);
    for e in r.entries() {
        assert!(e.name_is_utf8, "{:?}", e.raw_name);
    }
    assert_eq!(entry(&r, "中文/文件名.txt").raw_name, "中文/文件名.txt".as_bytes());
    assert_eq!(sink.files["中文/文件名.txt"], "中文内容\n".as_bytes());
    assert_eq!(sink.files["日本語.txt"], "にほんご\n".as_bytes());

    let (r, sink) = extract_all(&fx("unicode_path.zip"), None);
    let e = &r.entries()[0];
    assert!(!e.name_is_utf8);
    assert_eq!(e.raw_name, b"\xd6\xd0\xce\xc4\xc3\xfb.txt");
    assert_eq!(e.unicode_name.as_deref(), Some("中文名.txt"));
    assert_eq!(sink.files["中文名.txt"], b"unicode path test\n");

    let (r, _) = extract_all(&fx("gbk_names.zip"), None);
    let e = &r.entries()[0];
    assert!(!e.name_is_utf8);
    assert_eq!(e.unicode_name, None);
    assert_eq!(e.raw_name, b"\xc2\xd2\xc2\xeb\xb2\xe2\xca\xd4.txt");
    assert_eq!(e.method, Method::Deflate);
    // 纯 DOS 时间按本地时区换算，不同机器时区不同，只能框一个范围
    let m = e.mtime.expect("dos mtime");
    assert!((m - STAMP_AS_UTC).abs() <= 14 * 3600, "mtime {m}");
}

/// 目录条目、Info-ZIP UT 时间戳、Unix 权限位与符号链接。
#[test]
fn dirs_symlink_and_unix_mode() {
    let (r, sink) = extract_all(&fx("dirs.zip"), None);
    let d = entry(&r, "data/");
    assert_eq!(d.kind, EntryKind::Dir);
    assert_eq!(d.unix_mode.map(|m| m & 0xF000), Some(0o040000));
    assert_eq!(entry(&r, "hello.txt").mtime, Some(STAMP_UTC));
    assert_eq!(entry(&r, "hello.txt").unix_mode.map(|m| m & 0o777), Some(0o644));
    assert_eq!(sink.files["data/lorem.txt"], src("data/lorem.txt"));
    assert_eq!(sink.dirs, vec!["data/".to_string()]);

    let (r, sink) = extract_all(&fx("symlink.zip"), None);
    let link = entry(&r, "link");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.unix_mode.map(|m| m & 0xF000), Some(0o120000));
    assert_eq!(sink.links["link"], b"hello.txt");
    assert_eq!(sink.files["hello.txt"], src("hello.txt"));
}

/// 分卷：7-Zip 字节分卷 `.zip.001` 与 PKWARE 多盘 `.z01`（条目数据跨盘）。
#[test]
fn volumes() {
    let (r, sink) = extract_all(&fx("vol.zip.001"), None);
    assert_eq!(r.entries().len(), 3);
    assert_eq!(sink.files["bin.dat"], src("bin.dat"));
    assert_eq!(sink.files["data/lorem.txt"], src("data/lorem.txt"));
    assert_eq!(sink.files["hello.txt"], src("hello.txt"));

    let (r, sink) = extract_all(&fx("pkware_split.zip"), None);
    assert_eq!(r.entries().len(), 3);
    assert_eq!(sink.files["bin.dat"], src("bin.dat"));
    assert_eq!(sink.files["data/lorem.txt"], src("data/lorem.txt"));
    assert_eq!(sink.files["hello.txt"], src("hello.txt"));
}

/// 本机 Info-ZIP `zip -s` 真实分卷（最小 64 KB，不入库），工具不存在就跳过。
#[test]
fn real_infozip_split_if_available() {
    let zip = Path::new("/usr/bin/zip");
    if !zip.exists() {
        eprintln!("skip: /usr/bin/zip not found");
        return;
    }
    let dir = tempdir("infozip-split");
    // xorshift 伪随机：数据不可压缩，150 KB 才能真的切成多盘（zip -s 最小 64 KB）
    let mut x = 0x2545_F491u32;
    let big: Vec<u8> = (0..150_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect();
    std::fs::write(dir.join("big.dat"), &big).unwrap();
    std::fs::write(dir.join("small.txt"), b"small file\n").unwrap();
    let st = std::process::Command::new(zip)
        .current_dir(&dir)
        .args(["-q", "-s", "64k", "split.zip", "big.dat", "small.txt"])
        .status()
        .unwrap();
    assert!(st.success());
    assert!(dir.join("split.z01").exists());
    let (r, sink) = extract_all(&dir.join("split.zip"), None);
    assert_eq!(r.entries().len(), 2);
    assert_eq!(sink.files["big.dat"], big);
    assert_eq!(sink.files["small.txt"], b"small file\n");
    std::fs::remove_dir_all(dir).ok();
}

/// 空包与包注释。
#[test]
fn empty_and_comment() {
    let r = ZipReader::open(&fx("empty.zip")).unwrap();
    assert!(r.entries().is_empty());
    assert_eq!(r.comment(), None);
    let mut r = ZipReader::open(&fx("comment.zip")).unwrap();
    assert_eq!(r.comment(), Some(&b"Piesi test comment"[..]));
    let mut sink = MemSink::default();
    r.extract(None, &mut sink, &OpContext::silent()).unwrap();
    assert_eq!(sink.files["hello.txt"], src("hello.txt"));
}

/// 截断包：正常打开报损坏，扫描重建能救回数据完整的条目。
#[test]
fn truncated_open_fails_scan_recovers() {
    assert!(matches!(ZipReader::open(&fx("truncated.zip")), Err(Error::Corrupt(_))));
    let mut s = ZipReader::open_by_scan(&fx("truncated.zip")).unwrap();
    let names: Vec<String> = s.entries().iter().map(name_of).collect();
    for n in ["hello.txt", "empty.txt", "bin.dat", "data/", "data/lorem.txt"] {
        assert!(names.contains(&n.to_string()), "missing {n} in {names:?}");
    }
    assert_eq!(entry(&s, "data/").kind, EntryKind::Dir);
    let mut sink = MemSink::default();
    s.extract(None, &mut sink, &OpContext::silent()).unwrap();
    check_std_files(&sink);

    assert!(matches!(ZipReader::open(&fx("truncated_data.zip")), Err(Error::Corrupt(_))));
    let mut s = ZipReader::open_by_scan(&fx("truncated_data.zip")).unwrap();
    assert_eq!(s.entries().len(), 4, "最后一个条目数据不完整，应被丢弃");
    let mut sink = MemSink::default();
    s.extract(None, &mut sink, &OpContext::silent()).unwrap();
    for (name, data) in &sink.files {
        assert_eq!(*data, src(name), "{name}");
    }
}

/// 前缀垃圾：偏移整体修正后内容正确。
#[test]
fn prefix_junk_offsets_are_fixed() {
    let (r, sink) = extract_all(&fx("prefix.zip"), None);
    assert_eq!(r.entries().len(), 5);
    check_std_files(&sink);
}

/// 进度计数器：记录回调次数与最后一次快照；可选在首次回调时取消。
struct Counter {
    calls: AtomicU64,
    last: Mutex<ProgressEvent>,
    cancel_on_first: Option<CancelToken>,
}

impl Progress for Counter {
    fn report(&self, event: &ProgressEvent) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        *self.last.lock().unwrap() = event.clone();
        if let Some(t) = &self.cancel_on_first {
            t.cancel();
        }
    }
}

/// read_entry、进度上报与取消。
#[test]
fn read_entry_progress_and_cancel() {
    let mut r = ZipReader::open(&fx("deflate.zip")).unwrap();
    let id = entry(&r, "data/lorem.txt").id;
    let mut out = Vec::new();
    r.read_entry(id, &mut out, &OpContext::silent()).unwrap();
    assert_eq!(out, src("data/lorem.txt"));
    assert!(matches!(r.read_entry(999, &mut out, &OpContext::silent()), Err(Error::NoSuchEntry(999))));
    assert!(matches!(r.extract(Some(&[42]), &mut MemSink::default(), &OpContext::silent()), Err(Error::NoSuchEntry(42))));

    let counter = Counter { calls: AtomicU64::new(0), last: Mutex::new(ProgressEvent::default()), cancel_on_first: None };
    let token = CancelToken::new();
    let ctx = OpContext::new(&counter, &token);
    let mut sink = MemSink::default();
    r.extract(None, &mut sink, &ctx).unwrap();
    // 进度回调按时间/字节节流，不保证每条目一次；至少一次且最终事件计数完整即可
    assert!(counter.calls.load(Ordering::Relaxed) >= 1);
    let last = counter.last.lock().unwrap().clone();
    let total: u64 = r.entries().iter().map(|e| e.size).sum();
    assert_eq!(last.done_bytes, total);
    assert_eq!(last.total_bytes, Some(total));
    assert_eq!(last.done_entries, r.entries().len() as u64);

    let token = CancelToken::new();
    let counter = Counter { calls: AtomicU64::new(0), last: Mutex::new(ProgressEvent::default()), cancel_on_first: Some(token.clone()) };
    let ctx = OpContext::new(&counter, &token);
    let e = r.extract(None, &mut MemSink::default(), &ctx).unwrap_err();
    assert!(matches!(e, Error::Cancelled), "{e}");

    // 只解压选中的条目，顺序按给定 id
    let ids = [entry(&r, "hello.txt").id, entry(&r, "bin.dat").id];
    let mut sink = MemSink::default();
    let report = r.extract(Some(&ids), &mut sink, &OpContext::silent()).unwrap();
    assert_eq!(report.extracted, 2);
    assert_eq!(sink.files.len(), 2);
    assert_eq!(sink.files["bin.dat"], src("bin.dat"));
}

/// 篡改压缩数据：解压失败或 CRC 不符都要报损坏，不能静默给出错误内容。
#[test]
fn corrupted_data_is_reported() {
    let mut data = std::fs::read(fx("store.zip")).unwrap();
    // 第一个条目是 Store，翻转数据区第一个字节 → CRC 不符
    let name_len = u16::from_le_bytes([data[26], data[27]]) as usize;
    let extra_len = u16::from_le_bytes([data[28], data[29]]) as usize;
    let first = 30 + name_len + extra_len;
    data[first] ^= 0xFF;
    let dir = tempdir("corrupt");
    let p = dir.join("c.zip");
    std::fs::write(&p, &data).unwrap();
    let mut r = ZipReader::open(&p).unwrap();
    let e = r.extract(None, &mut MemSink::default(), &OpContext::silent()).unwrap_err();
    assert!(matches!(e, Error::Corrupt(_)), "{e}");
    std::fs::remove_dir_all(dir).ok();
}

/// 畸形输入只许返回错误，不许 panic：随机垃圾、逐段截断、随机翻转字节。
#[test]
fn malformed_inputs_never_panic() {
    let dir = tempdir("malformed");
    let p = dir.join("m.zip");
    let garbage: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
    std::fs::write(&p, &garbage).unwrap();
    assert!(ZipReader::open(&p).is_err());
    assert!(ZipReader::open_by_scan(&p).is_err());
    std::fs::write(&p, b"").unwrap();
    assert!(ZipReader::open(&p).is_err());

    let original = std::fs::read(fx("deflate.zip")).unwrap();
    let mut cut = 0;
    while cut < original.len() {
        std::fs::write(&p, &original[..cut]).unwrap();
        try_everything(&p);
        cut += 61;
    }
    let mut seed = 12345u32;
    for _ in 0..200 {
        let mut d = original.clone();
        for _ in 0..3 {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            let pos = (seed as usize >> 4) % d.len();
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            d[pos] ^= (seed >> 16) as u8 | 1;
        }
        std::fs::write(&p, &d).unwrap();
        try_everything(&p);
    }
    std::fs::remove_dir_all(dir).ok();
}

/// 打开、列目录、解压、扫描都跑一遍，结果不管对错都不能 panic。
fn try_everything(p: &Path) {
    if let Ok(mut r) = ZipReader::open(p) {
        let _ = r.entries().len();
        let _ = r.extract(None, &mut MemSink::default(), &OpContext::silent());
        let _ = r.read_entry(0, &mut std::io::sink(), &OpContext::silent());
    }
    if let Ok(mut r) = ZipReader::open_by_scan(p) {
        let _ = r.extract(None, &mut MemSink::default(), &OpContext::silent());
    }
}

/// 接收端中途不读完：驱动不报错，AES 认证照验。
#[test]
fn sink_may_stop_early() {
    struct Lazy;
    impl EntrySink for Lazy {
        fn dir(&mut self, _: &Entry) -> Result<()> {
            Ok(())
        }
        fn file(&mut self, _: &Entry, data: &mut dyn Read) -> Result<u64> {
            let mut one = [0u8; 1];
            let n = data.read(&mut one)?;
            Ok(n as u64)
        }
        fn symlink(&mut self, _: &Entry, _: &[u8]) -> Result<()> {
            Ok(())
        }
    }
    let mut r = ZipReader::open(&fx("aes256.zip")).unwrap();
    r.set_password(Some("TEST")).unwrap();
    r.extract(None, &mut Lazy, &OpContext::silent()).unwrap();
    let mut r = ZipReader::open(&fx("deflate.zip")).unwrap();
    r.extract(None, &mut Lazy, &OpContext::silent()).unwrap();
}

/// 通过统一入口 `piesi_core::open` 也能走到 zip 驱动（含靠扩展名兜底的分卷末盘）。
#[test]
fn open_via_detect() {
    let r = piesi_core::open(&fx("deflate.zip")).unwrap();
    assert_eq!(r.format(), piesi_core::Format::Zip);
    assert_eq!(r.entries().len(), 5);
    let r = piesi_core::open(&fx("pkware_split.zip")).unwrap();
    assert_eq!(r.entries().len(), 3);
    let r = piesi_core::open(&fx("vol.zip.001")).unwrap();
    assert_eq!(r.entries().len(), 3);
}

/// 编码语料：`tests/corpus/raw_names.json` 记录了每个包的原始名字字节与标志，
/// 逐个用读取器打开，原始字节、bit 11、0x7075 名、目录判定、made by 先验都要对得上。
#[test]
fn corpus_raw_names_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let json = std::fs::read_to_string(root.join("raw_names.json")).unwrap();
    let all: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut checked = 0;
    for (file, spec) in all.as_object().unwrap() {
        let path = root.join(file);
        let mut r = ZipReader::open(&path).unwrap_or_else(|e| panic!("open {file}: {e}"));
        let made_by = spec["made_by"].as_u64().unwrap();
        assert_eq!(r.hints().made_by_dos, made_by == 0, "{file} hints");
        let expected = spec["entries"].as_array().unwrap();
        assert_eq!(r.entries().len(), expected.len(), "{file} entry count");
        for (e, want) in r.entries().iter().zip(expected) {
            let raw: Vec<u8> = want["raw_name"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
            assert_eq!(e.raw_name, raw, "{file} raw name");
            let flags = want["flags"].as_u64().unwrap();
            assert_eq!(e.name_is_utf8, flags & 0x800 != 0, "{file} bit 11");
            assert_eq!(e.unicode_name.as_deref(), want["unicode_name"].as_str(), "{file} 0x7075");
            assert_eq!(e.kind == EntryKind::Dir, want["is_dir"].as_bool().unwrap(), "{file} is_dir of {:?}", e.raw_name);
        }
        let mut sink = MemSink::default();
        r.extract(None, &mut sink, &OpContext::silent()).unwrap_or_else(|e| panic!("extract {file}: {e}"));
        checked += 1;
    }
    assert!(checked >= 40, "only {checked} corpus archives");
}

/// 10 万条目的 zip64 包（条目数超过 65535，EOCD 计数饱和，必须走 zip64 记录）：
/// 索引要完整，并且打开耗时要在预算内（设计目标 release 下 < 200 ms，这里 debug 构建放宽）。
#[test]
fn large_index_zip64_count() {
    const N: u32 = 100_000;
    let dir = tempdir("large-index");
    let p = dir.join("big.zip");
    let mut out: Vec<u8> = Vec::with_capacity(12 << 20);
    let mut cd: Vec<u8> = Vec::with_capacity(8 << 20);
    for i in 0..N {
        let name = format!("dir{:02}/file{:06}.txt", i % 100, i);
        let off = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0x00, 0x7c, 0x22, 0x58]);
        out.extend_from_slice(&[0u8; 12]);
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0x00, 0x7c, 0x22, 0x58]);
        cd.extend_from_slice(&[0u8; 12]);
        cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&[0u8; 12]);
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u64;
    out.extend_from_slice(&cd);
    let z64 = out.len() as u64;
    out.extend_from_slice(b"PK\x06\x06");
    out.extend_from_slice(&44u64.to_le_bytes());
    out.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&(N as u64).to_le_bytes());
    out.extend_from_slice(&(N as u64).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u64).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(b"PK\x06\x07");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&z64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&(cd_off as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    std::fs::write(&p, &out).unwrap();

    let t = std::time::Instant::now();
    let r = ZipReader::open(&p).unwrap();
    let elapsed = t.elapsed();
    eprintln!("open 100k entries: {elapsed:?}");
    assert_eq!(r.entries().len(), N as usize);
    assert_eq!(name_of(&r.entries()[99_999]), "dir99/file099999.txt");
    assert!(r.entries()[0].mtime.is_some());
    assert!(elapsed.as_millis() < 2000, "打开 10 万条目耗时 {elapsed:?}");
    std::fs::remove_dir_all(dir).ok();
}

/// 临时目录（测试名 + 进程号，避免并行测试互踩）。
fn tempdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("piesi-zip-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 让 `Write` 导入有用武之地：read_entry 写进 Vec 的路径已经覆盖，这里顺带确认 sink 能写文件。
#[test]
fn read_entry_to_file() {
    let dir = tempdir("read-entry");
    let p = dir.join("out.bin");
    let mut r = ZipReader::open(&fx("bzip2.zip")).unwrap();
    let id = entry(&r, "data/lorem.txt").id;
    {
        let mut f = std::fs::File::create(&p).unwrap();
        r.read_entry(id, &mut f, &OpContext::silent()).unwrap();
        f.flush().unwrap();
    }
    assert_eq!(std::fs::read(&p).unwrap(), src("data/lorem.txt"));
    std::fs::remove_dir_all(dir).ok();
}
