//! 7z 写入器：sevenz-rust2 做容器与 LZMA2（多线程分块），solid 块按大小上限分组。

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::rc::Rc;

use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options};
use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderConfiguration, Password, SourceReader};

use crate::archive::EntryKind;
use crate::create::{CreateOptions, SourceItem, Sources};
use crate::progress::{OpContext, Throttle};
use crate::{Error, Result};

/// 7-Zip 约定：attributes 的 bit 15 置位时，高 16 位存 Unix 权限。
const ATTR_UNIX_EXTENSION: u32 = 0x8000;
const ATTR_DIRECTORY: u32 = 0x10;

/// 边读边报进度、顺便当取消检查点的读取器。
/// 同一批 solid 条目的读取器要放进一个 Vec，所以共享 Rc<RefCell<Throttle>>（写入器单线程顺序读）。
struct ProgressRead<'a> {
    inner: File,
    throttle: Rc<RefCell<Throttle<'a>>>,
}

impl Read for ProgressRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.throttle.borrow_mut().add_bytes(n as u64).map_err(io::Error::other)?;
        Ok(n)
    }
}

/// 把 0–9 的通用级别映射成 LZMA2 级别和字典大小（字典按内存上限自动收紧）。
fn lzma2_options(level: u8, threads: usize) -> Lzma2Options {
    let level = level.clamp(1, 9) as u32;
    let threads = if threads == 0 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) } else { threads };
    // 字典：和 7-Zip 的档位对应（5 → 16 MB，7 → 64 MB，9 → 256 MB），再按可用内存的 1/4 封顶
    let dict: u64 = match level {
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
    let dict = dict.min(memory_budget() / 4).max(1 << 20) as u32;
    // 分块大小 = 4 × 字典，和 7-Zip 的多线程切块一致；块越大压缩率越好，线程数越多内存越高
    let chunk = (dict as u64) * 4;
    let mut opts = Lzma2Options::from_level_mt(level, threads as u32, chunk);
    opts.set_dictionary_size(dict);
    opts
}

/// 可用内存估计（字节）；macOS 用 sysctl hw.memsize，失败按 8 GB。
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

/// 写出一个 7z。
pub fn write_7z(dest: &Path, sources: &Sources, opts: &CreateOptions, ctx: &OpContext<'_>) -> Result<()> {
    let mut writer = ArchiveWriter::create(dest).map_err(Error::other)?;
    let mut methods: Vec<EncoderConfiguration> = Vec::new();
    if let Some(pw) = &opts.password {
        methods.push(AesEncoderOptions::new(Password::from(pw.as_str())).into());
        writer.set_encrypt_header(opts.encrypt_names);
    } else {
        writer.set_encrypt_header(false);
    }
    if opts.level == 0 {
        methods.push(EncoderConfiguration::new(sevenz_rust2::EncoderMethod::COPY));
    } else {
        methods.push(lzma2_options(opts.level, opts.threads).into());
    }
    writer.set_content_methods(methods);

    let throttle = Rc::new(RefCell::new(Throttle::new(ctx, Some(sources.total_bytes), Some(sources.items.len() as u64))));

    // 目录和符号链接没有数据流或数据很小，逐个写；文件按 solid 块分组
    let mut block: Vec<&SourceItem> = Vec::new();
    let mut block_bytes = 0u64;
    let flush_block = |writer: &mut ArchiveWriter<File>, block: &mut Vec<&SourceItem>| -> Result<()> {
        if block.is_empty() {
            return Ok(());
        }
        let mut entries = Vec::with_capacity(block.len());
        let mut readers = Vec::with_capacity(block.len());
        for item in block.iter() {
            entries.push(make_entry(item));
            let inner = File::open(&item.path)?;
            readers.push(SourceReader::new(ProgressRead { inner, throttle: throttle.clone() }));
        }
        writer.push_archive_entries(entries, readers).map_err(Error::other)?;
        block.clear();
        Ok(())
    };

    for item in &sources.items {
        throttle.borrow_mut().begin_entry(&item.rel)?;
        match item.kind {
            EntryKind::Dir => {
                flush_block(&mut writer, &mut block)?;
                writer.push_archive_entry::<&[u8]>(make_entry(item), None).map_err(Error::other)?;
            }
            EntryKind::Symlink => {
                flush_block(&mut writer, &mut block)?;
                let target = item.link_target.clone().unwrap_or_default();
                writer.push_archive_entry(make_entry(item), Some(target.as_slice())).map_err(Error::other)?;
            }
            EntryKind::File => {
                if opts.solid_block_bytes == 0 {
                    let inner = File::open(&item.path)?;
                    writer
                        .push_archive_entry(make_entry(item), Some(ProgressRead { inner, throttle: throttle.clone() }))
                        .map_err(Error::other)?;
                } else {
                    if !block.is_empty() && block_bytes + item.size > opts.solid_block_bytes {
                        flush_block(&mut writer, &mut block)?;
                        block_bytes = 0;
                    }
                    block.push(item);
                    block_bytes += item.size;
                }
            }
        }
    }
    flush_block(&mut writer, &mut block)?;
    writer.finish()?;
    throttle.borrow_mut().finish();
    Ok(())
}
