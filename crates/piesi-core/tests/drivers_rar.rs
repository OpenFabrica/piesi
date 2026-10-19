//! RAR 驱动测试：夹具由 fixtures/rar/gen.sh 用 RARLAB rar 6.24 生成（RAR4 需要 6.x）。

#[path = "drivers_common.rs"]
mod common;

use common::*;
use piesi_core::archive::{EntryKind, Method, PasswordNeed};
use piesi_core::fmt::rar::RarReader;
use piesi_core::progress::OpContext;
use piesi_core::{ArchiveReader, Error};

/// 打开夹具。
fn open(name: &str) -> RarReader {
    RarReader::open(&fixture("rar", name)).unwrap_or_else(|e| panic!("打开 {name} 失败：{e}"))
}

/// RAR5 与 RAR4 非 solid 包：目录区字段与全部解压。
#[test]
fn rar5_and_rar4_list_and_extract() {
    for (name, version) in [("rar5.rar", 5u8), ("rar4.rar", 4u8)] {
        let mut r = open(name);
        assert_eq!(r.needs_password(), PasswordNeed::None, "{name}");
        let hello = find(&r, "hello.txt");
        assert!(hello.name_is_utf8);
        assert_eq!(hello.kind, EntryKind::File);
        assert_eq!(hello.size, 12);
        // rar 对压不动的小文件自动用 m0 存储，压缩方法要看压得动的 nested.txt
        assert_eq!(hello.method, Method::Store, "{name}");
        assert_eq!(find(&r, "dir/nested.txt").method, Method::Rar(version), "{name}");
        assert!(hello.packed_size.is_some());
        assert!(hello.crc32.is_some());
        assert!(hello.mtime.is_some_and(|t| t > 1_600_000_000), "{name}: {:?}", hello.mtime);
        assert_eq!(hello.unix_mode, Some(0o100644));
        assert_eq!(hello.solid_group, None);
        assert!(!hello.encrypted);
        assert_eq!(find(&r, "dir").kind, EntryKind::Dir);
        assert_eq!(find(&r, "emptydir").kind, EntryKind::Dir);
        assert_eq!(find(&r, "中文名.txt").raw_name, "中文名.txt".as_bytes());

        let (sink, report) = extract_all(&mut r);
        check_common_tree(&sink);
        assert_eq!(report.extracted, r.entries().len() as u64, "{name}");
        assert!(report.warnings.is_empty(), "{name}: {:?}", report.warnings);
    }
}

/// solid 包：文件条目同属一组；只选最后的条目时前面的被顺序解码丢弃。
#[test]
fn solid_selective_extract() {
    for name in ["solid5.rar", "solid4.rar"] {
        let mut r = open(name);
        for e in r.entries() {
            let expect = if e.kind == EntryKind::Dir { None } else { Some(0) };
            assert_eq!(e.solid_group, expect, "{name}: {}", key_of(e));
        }
        let id = find(&r, "bin/tiny.exe").id;
        let (sink, report) = extract_ids(&mut r, &[id]);
        assert_eq!(sink.files.len(), 1, "{name}");
        assert_eq!(sink.files.get("bin/tiny.exe"), Some(&tiny_exe()), "{name}");
        assert_eq!(report.extracted, 1);
        assert!(report.warnings.is_empty(), "{name}: {:?}", report.warnings);

        let (sink, _) = extract_all(&mut r);
        check_common_tree(&sink);
    }
}

/// solid 包里只选最后一个文件：前面跳过的条目 unrar 照样解码，丢弃的字节也计入进度，结束时 done == total。
#[test]
fn solid_skip_counts_progress() {
    for name in ["solid5.rar", "solid4.rar"] {
        let mut r = open(name);
        let members: Vec<_> = r.entries().iter().filter(|e| e.kind == EntryKind::File && e.size > 0).map(|e| e.id).collect();
        assert!(members.len() >= 2, "{name}");
        check_solid_skip_progress(&mut r, &members);
    }
}

/// 非 solid 包只选一个文件：跳过只是 seek，进度总量就是该文件大小。
#[test]
fn nonsolid_skip_is_free() {
    let mut r = open("rar5.rar");
    let id = find(&r, "bin/tiny.exe").id;
    let (sink, events) = extract_ids_with_progress(&mut r, Some(&[id]));
    assert_eq!(sink.files.get("bin/tiny.exe"), Some(&tiny_exe()));
    let last = events.last().unwrap();
    assert_eq!(last.total_bytes, Some(tiny_exe().len() as u64));
    assert_eq!(last.done_bytes, tiny_exe().len() as u64);
}

/// 存储（m0）方法映射成 Store。
#[test]
fn store_method() {
    let mut r = open("store5.rar");
    assert_eq!(find(&r, "hello.txt").method, Method::Store);
    assert_eq!(find(&r, "hello.txt").packed_size, Some(12));
    let (sink, _) = extract_all(&mut r);
    check_common_tree(&sink);
}

/// 数据加密（-p）：能列目录；没密码报 PasswordRequired，错密码报 WrongPassword，对了能解。
#[test]
fn data_password_flow() {
    for name in ["pw-data5.rar", "pw-data4.rar"] {
        let mut r = open(name);
        assert_eq!(r.needs_password(), PasswordNeed::ForData, "{name}");
        assert!(find(&r, "hello.txt").encrypted);
        let mut sink = MemSink::default();
        assert!(matches!(r.extract(None, &mut sink, &OpContext::silent()), Err(Error::PasswordRequired)), "{name}");

        r.set_password(Some("nope")).unwrap();
        let mut sink = MemSink::default();
        let err = r.extract(None, &mut sink, &OpContext::silent()).unwrap_err();
        assert!(matches!(err, Error::WrongPassword), "{name}: {err}");

        r.set_password(Some("pass123")).unwrap();
        assert_eq!(r.needs_password(), PasswordNeed::None);
        let (sink, _) = extract_all(&mut r);
        check_common_tree(&sink);
    }
}

/// 头加密（-hp）：没密码连文件名都没有；错密码重开失败；对了才有条目。
#[test]
fn header_password_flow() {
    for name in ["pw-header5.rar", "pw-header4.rar"] {
        let mut r = open(name);
        assert_eq!(r.needs_password(), PasswordNeed::ForNames, "{name}");
        assert!(r.entries().is_empty());
        let mut sink = MemSink::default();
        assert!(matches!(r.extract(None, &mut sink, &OpContext::silent()), Err(Error::PasswordRequired)));

        let err = r.set_password(Some("nope")).unwrap_err();
        assert!(matches!(err, Error::WrongPassword), "{name}: {err}");
        assert_eq!(r.needs_password(), PasswordNeed::ForNames);
        assert!(r.entries().is_empty());

        r.set_password(Some("pass123")).unwrap();
        assert_eq!(r.needs_password(), PasswordNeed::None, "{name}");
        assert!(find(&r, "hello.txt").encrypted);
        let (sink, _) = extract_all(&mut r);
        check_common_tree(&sink);
    }
}

/// 符号链接：RAR5 目标在头里（redir），RAR4 目标是文件内容；两种都交给 sink.symlink。
#[test]
fn symlink_rar5_and_rar4() {
    let mut r = open("symlink5.rar");
    let link = find(&r, "link.txt");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.link_target.as_deref(), Some(&b"hello.txt"[..]));
    let (sink, report) = extract_all(&mut r);
    assert_eq!(sink.links, vec![("link.txt".to_owned(), b"hello.txt".to_vec())]);
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
    assert_eq!(report.extracted, 2);

    let mut r = open("symlink4.rar");
    let link = find(&r, "link.txt");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.unix_mode.map(|m| m & 0o170000), Some(0o120000));
    let (sink, report) = extract_all(&mut r);
    assert_eq!(sink.links, vec![("link.txt".to_owned(), b"hello.txt".to_vec())]);
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
    assert_eq!(report.extracted, 2);
}

/// 分卷：RAR5 的 .part1.rar 与 RAR4 的 .rar/.r00/.r01，只传首卷。
#[test]
fn multi_volume() {
    for name in ["vol5.part1.rar", "vol4.rar"] {
        let mut r = open(name);
        assert_eq!(r.entries().len(), 2, "{name}");
        let (sink, _) = extract_all(&mut r);
        assert_eq!(sink.files.get("random.bin"), Some(&random_bin()), "{name}");
        assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO), "{name}");
    }
}

/// 压缩包注释。
#[test]
fn archive_comment() {
    let r = open("comment5.rar");
    let comment = r.comment().expect("有注释");
    assert!(String::from_utf8_lossy(comment).contains("Piesi RAR 注释"));
    assert!(open("rar5.rar").comment().is_none());
}

/// 只有空文件与空目录。
#[test]
fn empty_only() {
    let mut r = open("empty-only.rar");
    assert_eq!(find(&r, "empty.txt").size, 0);
    assert_eq!(find(&r, "emptydir").kind, EntryKind::Dir);
    let (sink, report) = extract_all(&mut r);
    assert_eq!(sink.files.get("empty.txt").map(Vec::len), Some(0));
    assert_eq!(sink.dirs, vec!["emptydir".to_owned()]);
    assert_eq!(report.extracted, 2);
}

/// read_entry 与越界 id。
#[test]
fn read_entry_and_bounds() {
    let mut r = open("rar5.rar");
    let mut out = Vec::new();
    r.read_entry(find(&r, "dir/nested.txt").id, &mut out, &OpContext::silent()).unwrap();
    assert_eq!(out, nested_txt());
    assert!(matches!(r.read_entry(999, &mut out, &OpContext::silent()), Err(Error::NoSuchEntry(999))));
}

/// 取消与进度。
#[test]
fn cancel_and_progress() {
    let mut r = open("solid5.rar");
    assert_cancelled(&mut r);
    let (sink, events) = extract_with_progress(&mut r);
    check_common_tree(&sink);
    let total: u64 = r.entries().iter().filter(|e| e.kind == EntryKind::File).map(|e| e.size).sum();
    let last = events.last().unwrap();
    assert_eq!(last.total_bytes, Some(total));
    assert_eq!(last.done_bytes, total);
}

/// sink 失败时错误原样返回，工作线程干净退出（不卡死）。
#[test]
fn sink_error_propagates_and_worker_stops() {
    for name in ["solid5.rar", "rar4.rar"] {
        let mut r = open(name);
        let err = r.extract(None, &mut FailingSink, &OpContext::silent()).unwrap_err();
        assert!(matches!(err, Error::Other(ref m) if m == "sink failed"), "{name}: {err}");
        // 同一个 reader 还能继续用
        let (sink, _) = extract_all(&mut r);
        check_common_tree(&sink);
    }
}

/// 不是 RAR 的文件报错而不是 panic。
#[test]
fn garbage_is_rejected() {
    let path = fixture("tar", "plain.tar");
    assert!(RarReader::open(&path).is_err());
}
