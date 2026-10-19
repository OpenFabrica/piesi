//! 7z 驱动测试：夹具由 fixtures/sevenz/gen.sh 生成。

#[path = "drivers_common.rs"]
mod common;

use common::*;
use piesi_core::archive::{EntryKind, Method, PasswordNeed};
use piesi_core::fmt::sevenz::SevenZReader;
use piesi_core::progress::OpContext;
use piesi_core::{ArchiveReader, Error};

/// 打开夹具。
fn open(name: &str) -> SevenZReader {
    SevenZReader::open(&fixture("sevenz", name)).unwrap_or_else(|e| panic!("打开 {name} 失败：{e}"))
}

/// solid 包：目录区正确，所有带数据的文件同属一个 solid 组，全部解压内容正确。
#[test]
fn solid_list_and_extract() {
    let mut r = open("solid.7z");
    assert_eq!(r.needs_password(), PasswordNeed::None);
    let hello = find(&r, "hello.txt");
    assert!(hello.name_is_utf8);
    assert_eq!(hello.kind, EntryKind::File);
    assert_eq!(hello.size, 12);
    assert_eq!(hello.method, Method::Lzma2);
    assert!(hello.crc32.is_some());
    assert!(hello.mtime.is_some());
    assert_eq!(hello.unix_mode.map(|m| m & 0o170000), Some(0o100000));
    let group = hello.solid_group.expect("solid group");
    assert_eq!(find(&r, "bin/tiny.exe").solid_group, Some(group));
    assert_eq!(find(&r, "dir/nested.txt").solid_group, Some(group));
    assert_eq!(find(&r, "dir").kind, EntryKind::Dir);
    assert_eq!(find(&r, "dir").solid_group, None);
    assert_eq!(find(&r, "empty.txt").solid_group, None);
    assert_eq!(find(&r, "中文名.txt").raw_name, "中文名.txt".as_bytes());

    let (sink, report) = extract_all(&mut r);
    check_common_tree(&sink);
    assert_eq!(report.skipped, 0);
    assert!(report.warnings.is_empty());
    assert_eq!(report.extracted, r.entries().len() as u64);
}

/// 非 solid 包：每个数据文件独立成块；只解一个文件时其他文件不出现。
#[test]
fn nonsolid_selective_extract() {
    let mut r = open("nonsolid.7z");
    let groups: Vec<_> = r.entries().iter().filter(|e| e.kind == EntryKind::File && e.size > 0).map(|e| e.solid_group).collect();
    assert!(groups.iter().all(|g| g.is_some()));
    let mut uniq = groups.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), groups.len(), "非 solid 包每个文件应各占一个块");
    assert!(find(&r, "bin/tiny.exe").packed_size.is_some_and(|p| p > 0));

    let id = find(&r, "dir/nested.txt").id;
    let (sink, report) = extract_ids(&mut r, &[id]);
    assert_eq!(sink.files.len(), 1);
    assert_eq!(sink.files.get("dir/nested.txt"), Some(&nested_txt()));
    assert_eq!(report.extracted, 1);
}

/// solid 包里只选后面的条目：前面的被解码后丢弃，结果只含选中的。
#[test]
fn solid_selective_extract_skips_unselected() {
    let mut r = open("solid.7z");
    let id = find(&r, "bin/tiny.exe").id;
    let (sink, _) = extract_ids(&mut r, &[id]);
    assert_eq!(sink.files.len(), 1);
    assert_eq!(sink.files.get("bin/tiny.exe"), Some(&tiny_exe()));
}

/// read_entry 走同一套逻辑，拿到单个文件内容。
#[test]
fn read_entry_single_file() {
    let mut r = open("solid.7z");
    let id = find(&r, "中文名.txt").id;
    let mut out = Vec::new();
    r.read_entry(id, &mut out, &OpContext::silent()).unwrap();
    assert_eq!(out, CHINESE);
    assert!(matches!(r.read_entry(999, &mut out, &OpContext::silent()), Err(Error::NoSuchEntry(999))));
}

/// 数据加密：文件名可见，解压前必须给密码；错密码报 WrongPassword，对的能解。
#[test]
fn aes_data_password_flow() {
    let mut r = open("aes-data.7z");
    assert_eq!(r.needs_password(), PasswordNeed::ForData);
    assert!(find(&r, "hello.txt").encrypted);
    assert!(!r.entries().is_empty());
    let mut sink = MemSink::default();
    assert!(matches!(r.extract(None, &mut sink, &OpContext::silent()), Err(Error::PasswordRequired)));

    r.set_password(Some("nope")).unwrap();
    assert_eq!(r.needs_password(), PasswordNeed::None);
    let mut sink = MemSink::default();
    assert!(matches!(r.extract(None, &mut sink, &OpContext::silent()), Err(Error::WrongPassword)));

    r.set_password(Some("pass123")).unwrap();
    let (sink, _) = extract_all(&mut r);
    check_common_tree(&sink);
}

/// 头加密：没密码时条目为空且要密码；错密码重读头失败；对了才有条目。
#[test]
fn aes_header_password_flow() {
    let mut r = open("aes-header.7z");
    assert_eq!(r.needs_password(), PasswordNeed::ForNames);
    assert!(r.entries().is_empty());
    let mut sink = MemSink::default();
    assert!(matches!(r.extract(None, &mut sink, &OpContext::silent()), Err(Error::PasswordRequired)));

    assert!(matches!(r.set_password(Some("nope")), Err(Error::WrongPassword)));
    assert_eq!(r.needs_password(), PasswordNeed::ForNames);
    assert!(r.entries().is_empty());

    r.set_password(Some("pass123")).unwrap();
    assert_eq!(r.needs_password(), PasswordNeed::None);
    assert!(find(&r, "hello.txt").encrypted);
    let (sink, _) = extract_all(&mut r);
    check_common_tree(&sink);

    // 再清掉密码：回到要密码的状态
    r.set_password(None).unwrap();
    assert_eq!(r.needs_password(), PasswordNeed::ForNames);
}

/// BCJ2（四路输入的过滤器，7-Zip 对 exe 的默认高压缩配置）能正确还原。
#[test]
fn bcj2_filter() {
    let mut r = open("bcj2.7z");
    assert_eq!(find(&r, "bin/tiny.exe").method, Method::Lzma2);
    let (sink, _) = extract_all(&mut r);
    assert_eq!(sink.files.get("bin/tiny.exe"), Some(&tiny_exe()));
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
}

/// x86 BCJ、ARM64、Delta 过滤器。
#[test]
fn bcj_arm64_delta_filters() {
    for name in ["bcj.7z", "arm64.7z", "delta.7z"] {
        let mut r = open(name);
        let (sink, _) = extract_all(&mut r);
        assert_eq!(sink.files.get("bin/tiny.exe"), Some(&tiny_exe()), "{name}");
    }
}

/// LZMA1 与 Copy 方法。
#[test]
fn lzma1_and_copy_methods() {
    let mut r = open("lzma1.7z");
    assert_eq!(find(&r, "hello.txt").method, Method::Lzma);
    let (sink, _) = extract_all(&mut r);
    check_common_tree(&sink);

    let mut r = open("copy.7z");
    assert_eq!(find(&r, "hello.txt").method, Method::Store);
    assert_eq!(find(&r, "hello.txt").packed_size, Some(12));
    let (sink, _) = extract_all(&mut r);
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
    assert_eq!(sink.files.get("dir/nested.txt"), Some(&nested_txt()));
}

/// PPMd 与 BZip2：目录区能列出方法；解码取决于 sevenz-rust2 是否开了 ppmd/bzip2 feature。
/// 当前 Cargo.toml 只开了 aes256/compress/util，所以这两个包解压应报 Unsupported；
/// 一旦加上 feature，这个测试会改为校验内容。
#[test]
fn ppmd_and_bzip2_methods() {
    for (name, method) in [("ppmd.7z", Method::Ppmd), ("bzip2.7z", Method::Bzip2)] {
        let mut r = open(name);
        assert_eq!(find(&r, "hello.txt").method, method, "{name}");
        let mut sink = MemSink::default();
        match r.extract(None, &mut sink, &OpContext::silent()) {
            Ok(_) => {
                assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO), "{name}");
                assert_eq!(sink.files.get("dir/nested.txt"), Some(&nested_txt()), "{name}");
            }
            Err(Error::Unsupported(msg)) => eprintln!("{name}: 当前 feature 不支持，跳过内容校验：{msg}"),
            Err(e) => panic!("{name}: 意外错误 {e}"),
        }
    }
}

/// 符号链接：类型、mode 带 S_IFLNK，解压时内容作为目标交给 sink.symlink。
#[test]
fn symlink_entry() {
    let mut r = open("symlink.7z");
    let link = find(&r, "link.txt");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.unix_mode.map(|m| m & 0o170000), Some(0o120000));
    let (sink, report) = extract_all(&mut r);
    assert_eq!(sink.links, vec![("link.txt".to_owned(), b"hello.txt".to_vec())]);
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
    assert_eq!(report.extracted, 2);
}

/// `.7z.001` 分卷：自动串联同目录分卷。
#[test]
fn multi_volume() {
    let mut r = open("vol.7z.001");
    assert_eq!(r.entries().len(), 2);
    let (sink, _) = extract_all(&mut r);
    assert_eq!(sink.files.get("random.bin"), Some(&random_bin()));
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
}

/// 只有空文件和空目录的包：没有数据块也要正确交付。
#[test]
fn empty_only() {
    let mut r = open("empty-only.7z");
    assert_eq!(find(&r, "empty.txt").kind, EntryKind::File);
    assert_eq!(find(&r, "empty.txt").size, 0);
    assert_eq!(find(&r, "emptydir").kind, EntryKind::Dir);
    let (sink, report) = extract_all(&mut r);
    assert_eq!(sink.files.get("empty.txt").map(Vec::len), Some(0));
    assert_eq!(sink.dirs, vec!["emptydir".to_owned()]);
    assert_eq!(report.extracted, 2);
}

/// 取消令牌已取消时立刻返回 Cancelled。
#[test]
fn cancel_before_start() {
    let mut r = open("solid.7z");
    assert_cancelled(&mut r);
}

/// 进度：总字节数等于 solid 块里要解码的全部数据，最后一次事件 done == total。
#[test]
fn progress_reports_totals() {
    let mut r = open("solid.7z");
    let (sink, events) = extract_with_progress(&mut r);
    check_common_tree(&sink);
    let total: u64 = r.entries().iter().map(|e| e.size).sum();
    let last = events.last().expect("至少一次进度");
    assert_eq!(last.total_bytes, Some(total));
    assert_eq!(last.done_bytes, total);
    assert_eq!(last.total_entries, Some(r.entries().len() as u64));
}

/// sink 失败时错误原样返回（不是误报成包损坏）。
#[test]
fn sink_error_propagates() {
    let mut r = open("solid.7z");
    let err = r.extract(None, &mut FailingSink, &OpContext::silent()).unwrap_err();
    assert!(matches!(err, Error::Other(ref m) if m == "sink failed"), "{err}");
}

/// 不是 7z 的文件报损坏而不是 panic。
#[test]
fn garbage_is_corrupt() {
    let path = fixture("tar", "plain.tar");
    assert!(matches!(SevenZReader::open(&path), Err(Error::Corrupt(_))));
}
