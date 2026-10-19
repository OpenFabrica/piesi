//! tar 系与单文件压缩流驱动测试：夹具由 fixtures/tar/gen.sh 生成。

#[path = "drivers_common.rs"]
mod common;

use common::*;
use piesi_core::archive::{EntryKind, Format, IndexState, Method};
use piesi_core::fmt::tar::TarReader;
use piesi_core::progress::OpContext;
use piesi_core::{ArchiveReader, Error, WarningKind};

/// 打开夹具。
fn open(name: &str, format: Format) -> TarReader {
    TarReader::open(&fixture("tar", name), format).unwrap_or_else(|e| panic!("打开 {name} 失败：{e}"))
}

/// 裸 tar：目录区、类型、权限、原始名字（目录带尾部 '/'），全部解压正确。
#[test]
fn plain_tar_list_and_extract() {
    let mut r = open("plain.tar", Format::Tar);
    assert_eq!(r.format(), Format::Tar);
    assert_eq!(r.index_state(), IndexState::Complete);
    let hello = find(&r, "hello.txt");
    assert_eq!(hello.kind, EntryKind::File);
    assert_eq!(hello.size, 12);
    assert_eq!(hello.packed_size, Some(12));
    assert_eq!(hello.method, Method::Store);
    assert!(!hello.name_is_utf8);
    assert!(hello.mtime.is_some());
    assert_eq!(hello.unix_mode, Some(0o100644));
    let dir = find(&r, "dir");
    assert_eq!(dir.kind, EntryKind::Dir);
    assert!(dir.raw_ends_with_slash());
    assert_eq!(dir.unix_mode.map(|m| m & 0o170000), Some(0o040000));
    assert_eq!(find(&r, "中文名.txt").raw_name, "中文名.txt".as_bytes());

    let (sink, report) = extract_all(&mut r);
    check_common_tree(&sink);
    assert_eq!(report.extracted, r.entries().len() as u64);
    assert!(report.warnings.is_empty());
}

/// 四种压缩外壳解出同样的树。
#[test]
fn compressed_tar_variants() {
    for (name, format) in [
        ("plain.tar.gz", Format::TarGz),
        ("plain.tar.bz2", Format::TarBz2),
        ("plain.tar.xz", Format::TarXz),
        ("plain.tar.zst", Format::TarZst),
    ] {
        let mut r = open(name, format);
        assert_eq!(r.format(), format);
        assert_eq!(find(&r, "hello.txt").packed_size, None, "{name}");
        let (sink, _) = extract_all(&mut r);
        check_common_tree(&sink);
    }
}

/// 多成员 gzip（拼接的 .gz）要连续解到底，后半段的 random.bin 才能完整。
#[test]
fn multistream_gzip_tar() {
    let mut r = open("multistream.tar.gz", Format::TarGz);
    let (sink, _) = extract_all(&mut r);
    check_common_tree(&sink);
    assert_eq!(sink.files.get("random.bin"), Some(&random_bin()));
}

/// 长文件名：pax 扩展头与 GNU 'L' 条目都要还原成完整路径。
#[test]
fn long_names_pax_and_gnu() {
    let expected = format!("long/{}.txt", "a".repeat(120));
    for name in ["longname-pax.tar", "longname-gnu.tar"] {
        let mut r = open(name, Format::Tar);
        let e = find(&r, &expected);
        assert_eq!(e.kind, EntryKind::File, "{name}");
        let (sink, _) = extract_all(&mut r);
        assert_eq!(sink.files.get(&expected).map(Vec::as_slice), Some(&b"long name\n"[..]), "{name}");
    }
}

/// 符号链接：类型、目标字节，解压交给 sink.symlink。
#[test]
fn symlink_entry() {
    let mut r = open("symlink.tar", Format::Tar);
    let link = find(&r, "link.txt");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.link_target.as_deref(), Some(&b"hello.txt"[..]));
    assert_eq!(link.unix_mode.map(|m| m & 0o170000), Some(0o120000));
    let (sink, report) = extract_all(&mut r);
    assert_eq!(sink.links, vec![("link.txt".to_owned(), b"hello.txt".to_vec())]);
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
    let names: Vec<_> = r.entries().iter().map(key_of).collect();
    assert_eq!(report.extracted, 2, "entries={names:?} report={report:?}");
}

/// 硬链接：列出来但解压时跳过并告警。
#[test]
fn hardlink_is_skipped_with_warning() {
    let mut r = open("hardlink.tar", Format::Tar);
    let hard = find(&r, "hard.txt");
    assert_eq!(hard.kind, EntryKind::File);
    assert_eq!(hard.size, 0);
    assert_eq!(hard.link_target.as_deref(), Some(&b"hello.txt"[..]));
    let (sink, report) = extract_all(&mut r);
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
    assert!(!sink.files.contains_key("hard.txt"));
    assert_eq!(report.skipped, 1);
    assert_eq!(report.extracted, 1);
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].kind, WarningKind::HardLinkSkipped, "{:?}", report.warnings);
}

/// GBK 编码的文件名原样透传：raw_name 就是 GBK 字节，不标 UTF-8。
#[test]
fn gbk_name_bytes_pass_through() {
    let mut r = open("gbk-names.tar", Format::Tar);
    assert_eq!(r.entries().len(), 1);
    let e = &r.entries()[0];
    assert_eq!(e.raw_name, b"\xd6\xd0\xce\xc4\xc3\xfb.txt");
    assert!(!e.name_is_utf8);
    assert_eq!(e.mtime, Some(1_700_000_000));
    let (sink, _) = extract_all(&mut r);
    assert_eq!(sink.files.values().next().map(Vec::as_slice), Some("GBK 文件名\n".as_bytes()));
}

/// 裸 tar 的选择性解压（走 seek 跳过）与 read_entry。
#[test]
fn selective_extract_and_read_entry() {
    let mut r = open("plain.tar", Format::Tar);
    let id = find(&r, "bin/tiny.exe").id;
    let (sink, report) = extract_ids(&mut r, &[id]);
    assert_eq!(sink.files.len(), 1);
    assert_eq!(sink.files.get("bin/tiny.exe"), Some(&tiny_exe()));
    assert_eq!(report.extracted, 1);

    let mut out = Vec::new();
    r.read_entry(find(&r, "dir/nested.txt").id, &mut out, &OpContext::silent()).unwrap();
    assert_eq!(out, nested_txt());
    assert!(matches!(r.extract(Some(&[42]), &mut MemSink::default(), &OpContext::silent()), Err(Error::NoSuchEntry(42))));
}

/// 压缩 tar 的选择性解压（只能顺序读过去）。
#[test]
fn selective_extract_compressed() {
    let mut r = open("plain.tar.xz", Format::TarXz);
    let id = find(&r, "中文名.txt").id;
    let (sink, _) = extract_ids(&mut r, &[id]);
    assert_eq!(sink.files.len(), 1);
    assert_eq!(sink.files.get("中文名.txt").map(Vec::as_slice), Some(CHINESE));
}

/// gzip 单文件：名字取 FNAME，大小取 ISIZE，mtime 取头里的时间。
#[test]
fn single_gz_with_fname() {
    let mut r = open("hello.txt.gz", Format::Gz);
    assert_eq!(r.entries().len(), 1);
    let e = &r.entries()[0];
    assert_eq!(e.raw_name, b"hello.txt");
    assert!(!e.name_is_utf8);
    assert_eq!(e.size, 12);
    assert_eq!(e.method, Method::Deflate);
    assert!(e.mtime.is_some());
    assert!(e.packed_size.is_some());
    let (sink, report) = extract_all(&mut r);
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
    assert_eq!(report.extracted, 1);
}

/// gzip 没有 FNAME 时去掉扩展名；bz2/xz 大小未知填 0；zst 从帧头拿大小。
#[test]
fn single_stream_names_and_sizes() {
    let r = open("noname.gz", Format::Gz);
    assert_eq!(r.entries()[0].raw_name, b"noname");
    assert!(r.entries()[0].name_is_utf8);

    let mut r = open("hello.txt.bz2", Format::Bz2);
    assert_eq!(r.entries()[0].raw_name, b"hello.txt");
    assert_eq!(r.entries()[0].size, 0, "bz2 没有便宜的大小信息，UI 显示未知");
    assert_eq!(r.entries()[0].method, Method::Bzip2);
    assert_eq!(extract_all(&mut r).0.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));

    let mut r = open("hello.txt.xz", Format::Xz);
    assert_eq!(r.entries()[0].size, 0);
    assert_eq!(r.entries()[0].method, Method::Xz);
    assert_eq!(extract_all(&mut r).0.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));

    let mut r = open("hello.txt.zst", Format::Zst);
    assert_eq!(r.entries()[0].size, 12);
    assert_eq!(r.entries()[0].method, Method::Zstd);
    assert_eq!(extract_all(&mut r).0.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
}

/// 两个 gzip 成员拼成的 .gz 解出来是两段内容相连。
#[test]
fn multi_member_gz() {
    let mut r = open("multi-member.gz", Format::Gz);
    let mut out = Vec::new();
    r.read_entry(0, &mut out, &OpContext::silent()).unwrap();
    let mut expected = HELLO.to_vec();
    expected.extend_from_slice(&nested_txt());
    assert_eq!(out, expected);
}

/// 取消与进度。
#[test]
fn cancel_and_progress() {
    let mut r = open("plain.tar.gz", Format::TarGz);
    assert_cancelled(&mut r);
    let (sink, events) = extract_with_progress(&mut r);
    check_common_tree(&sink);
    let total: u64 = r.entries().iter().filter(|e| e.kind == EntryKind::File).map(|e| e.size).sum();
    let last = events.last().unwrap();
    assert_eq!(last.total_bytes, Some(total));
    assert_eq!(last.done_bytes, total);
}

/// sink 失败时错误原样返回。
#[test]
fn sink_error_propagates() {
    let mut r = open("plain.tar", Format::Tar);
    let err = r.extract(None, &mut FailingSink, &OpContext::silent()).unwrap_err();
    assert!(matches!(err, Error::Other(ref m) if m == "sink failed"), "{err}");
}

/// 损坏的压缩流报 Corrupt。
#[test]
fn truncated_stream_is_corrupt() {
    let dir = std::env::temp_dir().join(format!("piesi-tar-trunc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let data = std::fs::read(fixture("tar", "plain.tar.gz")).unwrap();
    let path = dir.join("cut.tar.gz");
    std::fs::write(&path, &data[..data.len() / 2]).unwrap();
    let result = TarReader::open(&path, Format::TarGz);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(matches!(result, Err(Error::Corrupt(_))), "{:?}", result.err());
}
