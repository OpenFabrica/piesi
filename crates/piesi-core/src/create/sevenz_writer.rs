//! 7z 写入器：sevenz-rust2 做容器与 LZMA2。多个 solid 块用 `prepare_block` 在工作线程里并行压缩，
//! 主线程按原顺序写入；块内再按 chunk 多线程。这样小文件多的目录能吃满核心，又保持 solid 块上限。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};

use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options};
use sevenz_rust2::{prepare_block, ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod, Password, PreparedBlock, SourceReader};

use crate::archive::EntryKind;
use crate::create::{CreateOptions, SourceItem, Sources};
use crate::progress::{OpContext, Throttle};
use crate::{Error, Result};

/// 7-Zip 约定：attributes 的 bit 15 置位时，高 16 位存 Unix 权限。
const ATTR_UNIX_EXTENSION: u32 = 0x8000;
const ATTR_DIRECTORY: u32 = 0x10;

/// 边读边报进度、顺便当取消检查点的读取器。多个工作线程共用一个节流器，所以用 Arc<Mutex>。
struct ProgressRead<'a> {
    inner: File,
    throttle: Arc<Mutex<Throttle<'a>>>,
}

impl Read for ProgressRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.throttle.lock().map_err(|_| io::Error::other("进度锁中毒"))?.add_bytes(n as u64).map_err(io::Error::other)?;
        Ok(n)
    }
}

/// 写入单元：目录、符号链接、空文件直接写；有内容的文件按 solid 块分组。
enum Unit<'a> {
    Meta(&'a SourceItem),
    Block(Vec<&'a SourceItem>),
}

/// 按 solid 块上限把文件分组；上限为 0 表示非 solid，每个文件单独一块。
fn split_units(items: &[SourceItem], block_limit: u64) -> Vec<Unit<'_>> {
    let mut units = Vec::new();
    let mut block: Vec<&SourceItem> = Vec::new();
    let mut block_bytes = 0u64;
    for item in items {
        if item.kind != EntryKind::File || item.size == 0 {
            units.push(Unit::Meta(item));
            continue;
        }
        if block_limit == 0 {
            units.push(Unit::Block(vec![item]));
            continue;
        }
        if !block.is_empty() && block_bytes + item.size > block_limit {
            units.push(Unit::Block(std::mem::take(&mut block)));
            block_bytes = 0;
        }
        block.push(item);
        block_bytes += item.size;
    }
    if !block.is_empty() {
        units.push(Unit::Block(block));
    }
    units
}

/// 0–9 的通用级别对应的 LZMA2 字典（和 7-Zip 档位一致：5 → 16 MB，7 → 64 MB，9 → 256 MB），再按内存收紧。
fn dict_for_level(level: u8) -> u32 {
    let dict: u64 = match level.clamp(1, 9) {
        1 => 1 << 20,
        2 => 2 << 20,
        3 => 4 << 20,
        4 => 8 << 20,
        5 => 16 << 20,
        6 => 32 << 20,
        7 => 64 << 20,
        8 => 128 << 20,
        _ => 256 << 20,
    };
    dict.min(memory_budget() / 4).max(1 << 20) as u32
}

/// LZMA2 选项：块内按 chunk = 4 × 字典切分并行，和 7-Zip 的多线程切块一致。
fn lzma2_options(level: u8, threads: usize) -> Lzma2Options {
    let dict = dict_for_level(level);
    let mut opts = Lzma2Options::from_level_mt(level.clamp(1, 9) as u32, threads.max(1) as u32, (dict as u64) * 4);
    opts.set_dictionary_size(dict);
    opts
}

/// 本机内存（字节）；macOS 用 sysctl hw.memsize，失败按 8 GB。
fn memory_budget() -> u64 {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).output() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                if let Ok(v) = s.trim().parse::<u64>() {
                    return v;
                }
            }
        }
    }
    8 << 30
}

/// 并行度：同时压几个块、每块几个线程。总线程 ≈ CPU 核数；内存按「每块 ≈ 字典×12×线程 + 块本身」估算，封顶在内存的 1/4。
fn plan_parallelism(threads: usize, block_count: usize, level: u8, block_limit: u64) -> (usize, usize) {
    if block_count == 0 {
        return (1, threads.max(1));
    }
    let mut in_flight = threads.min(block_count).max(1);
    let per_block_threads = (threads / in_flight).max(1);
    let dict = dict_for_level(level) as u64;
    let block_bytes = if block_limit == 0 { 256 << 20 } else { block_limit.min(1 << 30) };
    let per_block_cost = dict * 12 * per_block_threads as u64 + block_bytes;
    let budget = memory_budget() / 4;
    let by_memory = (budget / per_block_cost.max(1)).max(1) as usize;
    in_flight = in_flight.min(by_memory);
    (in_flight, (threads / in_flight).max(1))
}

/// 构造 7z 条目：名字、时间、属性（目录标志 + Unix 权限）。
fn make_entry(item: &SourceItem) -> ArchiveEntry {
    let mut e = if item.kind == EntryKind::Dir { ArchiveEntry::new_directory(&item.rel) } else { ArchiveEntry::new_file(&item.rel) };
    if let Some(m) = item.mtime {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(m.max(0) as u64);
        if let Ok(nt) = sevenz_rust2::NtTime::try_from(t) {
            e.has_last_modified_date = true;
            e.last_modified_date = nt;
        }
    }
    let mode = match item.kind {
        EntryKind::Dir => (item.mode.unwrap_or(0o755) & 0o7777) | 0o040000,
        EntryKind::Symlink => 0o120777,
        EntryKind::File => (item.mode.unwrap_or(0o644) & 0o7777) | 0o100000,
    };
    e.has_windows_attributes = true;
    e.windows_attributes = (mode << 16) | ATTR_UNIX_EXTENSION | if item.kind == EntryKind::Dir { ATTR_DIRECTORY } else { 0 };
    e
}

/// 每个块各建一份编码链：AES 的 IV/salt 由 `AesEncoderOptions::new` 随机生成，按块各建一份，避免多个块共用同一 IV。
fn build_methods(opts: &CreateOptions, per_block_threads: usize) -> Vec<EncoderConfiguration> {
    let mut methods: Vec<EncoderConfiguration> = Vec::new();
    if let Some(pw) = &opts.password {
        methods.push(AesEncoderOptions::new(Password::from(pw.as_str())).into());
    }
    if opts.level == 0 {
        methods.push(EncoderConfiguration::new(EncoderMethod::COPY));
    } else {
        methods.push(lzma2_options(opts.level, per_block_threads).into());
    }
    methods
}

/// 在工作线程里压一个 solid 块。
fn prepare_one<'a>(files: &[&'a SourceItem], methods: Vec<EncoderConfiguration>, throttle: Arc<Mutex<Throttle<'a>>>) -> Result<PreparedBlock> {
    let mut entries = Vec::with_capacity(files.len());
    let mut readers = Vec::with_capacity(files.len());
    for item in files {
        entries.push(make_entry(item));
        let inner = File::open(&item.path)?;
        readers.push(SourceReader::new(ProgressRead { inner, throttle: throttle.clone() }));
    }
    prepare_block(Arc::new(methods), entries, readers).map_err(Error::other)
}

/// 写出一个 7z。
pub fn write_7z(dest: &Path, sources: &Sources, opts: &CreateOptions, ctx: &OpContext<'_>) -> Result<()> {
    let threads = if opts.threads == 0 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) } else { opts.threads };
    let units = split_units(&sources.items, opts.solid_block_bytes);
    let block_count = units.iter().filter(|u| matches!(u, Unit::Block(_))).count();
    let (in_flight, per_block_threads) = plan_parallelism(threads, block_count, opts.level, opts.solid_block_bytes);

    let mut writer = ArchiveWriter::create(dest).map_err(Error::other)?;
    writer.set_content_methods(build_methods(opts, per_block_threads));
    writer.set_encrypt_header(opts.password.is_some() && opts.encrypt_names);

    let throttle = Arc::new(Mutex::new(Throttle::new(ctx, Some(sources.total_bytes), Some(sources.items.len() as u64))));

    // 工作线程从任务通道取块下标，压完经结果通道送回；主线程按单元顺序写入，乱序到达的先存起来。
    // 结果通道有界（容量 = in_flight），压完等着写的块不会无限堆积在内存里。
    let (job_tx, job_rx) = mpsc::channel::<usize>();
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (res_tx, res_rx) = mpsc::sync_channel::<Result<(usize, PreparedBlock)>>(in_flight);
    for (idx, unit) in units.iter().enumerate() {
        if matches!(unit, Unit::Block(_)) {
            let _ = job_tx.send(idx);
        }
    }
    drop(job_tx);

    let outcome: Result<()> = std::thread::scope(|scope| {
        for _ in 0..in_flight {
            let job_rx = job_rx.clone();
            let res_tx = res_tx.clone();
            let throttle = throttle.clone();
            let units = &units;
            scope.spawn(move || loop {
                let idx = match job_rx.lock() {
                    Ok(rx) => match rx.recv() {
                        Ok(i) => i,
                        Err(_) => return,
                    },
                    Err(_) => return,
                };
                let Unit::Block(files) = &units[idx] else { continue };
                let r = prepare_one(files, build_methods(opts, per_block_threads), throttle.clone()).map(|b| (idx, b));
                if res_tx.send(r).is_err() {
                    return;
                }
            });
        }
        drop(res_tx);

        let mut pending: BTreeMap<usize, PreparedBlock> = BTreeMap::new();
        for (idx, unit) in units.iter().enumerate() {
            match unit {
                Unit::Meta(item) => {
                    throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.begin_entry(&item.rel)?;
                    match item.kind {
                        EntryKind::Symlink => {
                            let target = item.link_target.clone().unwrap_or_default();
                            writer.push_archive_entry(make_entry(item), Some(target.as_slice())).map_err(Error::other)?;
                        }
                        _ => {
                            writer.push_archive_entry::<&[u8]>(make_entry(item), None).map_err(Error::other)?;
                        }
                    }
                }
                Unit::Block(files) => {
                    let block = loop {
                        if let Some(b) = pending.remove(&idx) {
                            break b;
                        }
                        match res_rx.recv() {
                            Ok(Ok((i, b))) => {
                                pending.insert(i, b);
                            }
                            Ok(Err(e)) => return Err(if ctx.cancel.is_cancelled() { Error::Cancelled } else { e }),
                            Err(_) => return Err(Error::Other("压缩线程提前退出".into())),
                        }
                    };
                    for item in files {
                        throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.begin_entry(&item.rel)?;
                    }
                    writer.push_prepared_block(block).map_err(Error::other)?;
                }
            }
        }
        Ok(())
    });
    outcome?;
    writer.finish()?;
    throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.finish();
    Ok(())
}
