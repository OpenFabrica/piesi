//! 三个驱动测试共用的内存接收端、夹具定位与期望内容。
//! 期望内容必须与 fixtures/{sevenz,rar,tar}/gen.sh 里 python 生成的源文件树一致。
//! 这个文件也会被 cargo 当成独立测试 crate 编译一次（没有测试，无害）。
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

use piesi_core::archive::{ArchiveReader, Entry, EntryId, EntrySink, ExtractReport};
use piesi_core::progress::{CancelToken, OpContext, Progress, ProgressEvent};
use piesi_core::{Error, Result};

/// 期望内容：hello.txt。
pub const HELLO: &[u8] = b"hello piesi\n";
/// 期望内容：中文名.txt。
pub const CHINESE: &[u8] = "中文内容\n".as_bytes();

/// 夹具路径：tests/fixtures/<kind>/<name>。
pub fn fixture(kind: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(kind).join(name)
}

/// 与 gen.sh 相同参数的 64 位 LCG，取高 8 位。
pub fn lcg(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        out.push((x >> 56) as u8);
    }
    out
}

/// 期望内容：bin/tiny.exe（16 KB LCG，每第 7 字节是 0xE8）。
pub fn tiny_exe() -> Vec<u8> {
    let mut v = lcg(16384, 1);
    for i in (0..v.len()).step_by(7) {
        v[i] = 0xE8;
    }
    v
}

/// 期望内容：random.bin（48 KB LCG）。
pub fn random_bin() -> Vec<u8> {
    lcg(49152, 2)
}

/// 期望内容：dir/nested.txt。
pub fn nested_txt() -> Vec<u8> {
    let mut v = b"nested ".repeat(300);
    v.push(b'\n');
    v
}

/// 把条目名按 UTF-8 宽松解码并去掉尾部 '/'，作为内存接收端的 key。
pub fn key_of(entry: &Entry) -> String {
    String::from_utf8_lossy(&entry.raw_name).trim_end_matches('/').to_owned()
}

/// 收集到内存的接收端。
#[derive(Default, Debug)]
pub struct MemSink {
    pub files: BTreeMap<String, Vec<u8>>,
    pub dirs: Vec<String>,
    pub links: Vec<(String, Vec<u8>)>,
}

impl EntrySink for MemSink {
    fn dir(&mut self, entry: &Entry) -> Result<()> {
        self.dirs.push(key_of(entry));
        Ok(())
    }

    fn file(&mut self, entry: &Entry, data: &mut dyn Read) -> Result<u64> {
        let mut buf = Vec::new();
        data.read_to_end(&mut buf)?;
        let n = buf.len() as u64;
        self.files.insert(key_of(entry), buf);
        Ok(n)
    }

    fn symlink(&mut self, entry: &Entry, target: &[u8]) -> Result<()> {
        self.links.push((key_of(entry), target.to_vec()));
        Ok(())
    }
}

/// 读到第一个文件就失败的接收端：验证驱动在落盘失败时能干净退出（不卡死、不吞错）。
pub struct FailingSink;

impl EntrySink for FailingSink {
    fn dir(&mut self, _entry: &Entry) -> Result<()> {
        Ok(())
    }

    fn file(&mut self, _entry: &Entry, data: &mut dyn Read) -> Result<u64> {
        let mut one = [0u8; 1];
        let _ = data.read(&mut one)?;
        Err(Error::Other("sink failed".into()))
    }

    fn symlink(&mut self, _entry: &Entry, _target: &[u8]) -> Result<()> {
        Ok(())
    }
}

/// 记录所有进度事件的接收方。
#[derive(Default)]
pub struct RecordingProgress(pub Mutex<Vec<ProgressEvent>>);

impl Progress for RecordingProgress {
    fn report(&self, event: &ProgressEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

/// 全部解压到内存。
pub fn extract_all(reader: &mut dyn ArchiveReader) -> (MemSink, ExtractReport) {
    let mut sink = MemSink::default();
    let report = reader.extract(None, &mut sink, &OpContext::silent()).expect("extract all");
    (sink, report)
}

/// 只解压指定条目到内存。
pub fn extract_ids(reader: &mut dyn ArchiveReader, ids: &[EntryId]) -> (MemSink, ExtractReport) {
    let mut sink = MemSink::default();
    let report = reader.extract(Some(ids), &mut sink, &OpContext::silent()).expect("extract ids");
    (sink, report)
}

/// 按名字找条目（忽略尾部 '/'）。
pub fn find<'a>(reader: &'a dyn ArchiveReader, name: &str) -> &'a Entry {
    reader
        .entries()
        .iter()
        .find(|e| key_of(e) == name)
        .unwrap_or_else(|| panic!("条目 {name} 不存在，现有：{:?}", reader.entries().iter().map(key_of).collect::<Vec<_>>()))
}

/// 校验公共源文件树（hello / dir/nested / empty / emptydir / 中文名 / bin/tiny.exe）都解对了。
pub fn check_common_tree(sink: &MemSink) {
    assert_eq!(sink.files.get("hello.txt").map(Vec::as_slice), Some(HELLO));
    assert_eq!(sink.files.get("dir/nested.txt"), Some(&nested_txt()));
    assert_eq!(sink.files.get("empty.txt").map(Vec::len), Some(0));
    assert_eq!(sink.files.get("中文名.txt").map(Vec::as_slice), Some(CHINESE));
    assert_eq!(sink.files.get("bin/tiny.exe"), Some(&tiny_exe()));
    assert!(sink.dirs.iter().any(|d| d == "emptydir"), "缺少 emptydir：{:?}", sink.dirs);
}

/// 用已取消的令牌调用 extract，必须立刻返回 Cancelled。
pub fn assert_cancelled(reader: &mut dyn ArchiveReader) {
    let token = CancelToken::new();
    token.cancel();
    let progress = RecordingProgress::default();
    let ctx = OpContext::new(&progress, &token);
    let mut sink = MemSink::default();
    assert!(matches!(reader.extract(None, &mut sink, &ctx), Err(Error::Cancelled)));
    assert!(sink.files.is_empty());
}

/// 带进度解压全部，返回收到的事件。
pub fn extract_with_progress(reader: &mut dyn ArchiveReader) -> (MemSink, Vec<ProgressEvent>) {
    extract_ids_with_progress(reader, None)
}

/// 带进度解压指定条目（None 为全部），返回收到的事件。
pub fn extract_ids_with_progress(reader: &mut dyn ArchiveReader, ids: Option<&[EntryId]>) -> (MemSink, Vec<ProgressEvent>) {
    let token = CancelToken::new();
    let progress = RecordingProgress::default();
    let ctx = OpContext::new(&progress, &token);
    let mut sink = MemSink::default();
    reader.extract(ids, &mut sink, &ctx).expect("extract with progress");
    let events = progress.0.into_inner().unwrap();
    (sink, events)
}

/// 在 solid 组里只选最后一个文件条目，验证进度把跳过的（解码丢弃的）字节也算进去：
/// total 等于该条目及其前面所有同组文件的展开大小之和，结束时 done == total。
pub fn check_solid_skip_progress(reader: &mut dyn ArchiveReader, group_entries: &[EntryId]) {
    let last = *group_entries.iter().max().expect("solid 组非空");
    let expected: u64 = reader.entries().iter().filter(|e| group_entries.contains(&e.id) && e.id <= last).map(|e| e.size).sum();
    let last_size = reader.entries()[last as usize].size;
    assert!(expected > last_size, "组里应该有要跳过的前置条目：expected={expected} last={last_size}");
    let (sink, events) = extract_ids_with_progress(reader, Some(&[last]));
    assert_eq!(sink.files.len(), 1);
    let final_event = events.last().expect("至少一次进度");
    assert_eq!(final_event.total_bytes, Some(expected));
    assert_eq!(final_event.done_bytes, expected);
}
