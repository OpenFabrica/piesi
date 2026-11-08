//! 自写 ZIP 写入器：UTF-8 + bit 11、NFC（上游已做）、zip64、Unix 权限、UT 时间戳，
//! 多文件并行 deflate，可选 WinZip AES-256 加密（AE-1 / AE-2），可选「Mac 专用」的 `__MACOSX/._名字` AppleDouble 条目。
//! 不用 `zip` crate 的原因：它没有「接收已压缩数据」的接口，做不了并行。

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, BufWriter, IoSlice, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{mpsc, Mutex};

use rayon::prelude::*;

use flate2::write::DeflateEncoder;
use flate2::Compression;

use crate::archive::EntryKind;
use crate::create::apple_double;
use crate::create::{CreateOptions, SourceItem, Sources};
use crate::fmt::zip::aes as zipaes;
use crate::progress::{OpContext, Throttle};
use crate::warning::{Warning, WarningKind};
use crate::{Error, Result};

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_EOCD64: u32 = 0x0606_4b50;
const SIG_EOCD64_LOC: u32 = 0x0706_4b50;
const FLAG_UTF8: u16 = 1 << 11;
const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
/// WinZip AES 包装方法号，真实方法在 0x9901 扩展字段里
const METHOD_AES: u16 = 99;
const FLAG_ENCRYPTED: u16 = 1 << 0;
/// 只写 AES-256（WinZip 规范里的强度 3）
const AES_STRENGTH: u8 = 3;
/// 明文短于这个长度用 AE-2（CRC 字段写 0）：CRC 能被穷举反推很短的明文，这是 WinZip 自己的规则
const AE2_BELOW: u64 = 20;
const VERSION_MADE_BY: u16 = (3 << 8) | 63; // 主机 Unix，规范 6.3
const ZIP64_LIMIT: u64 = 0xFFFF_FFFF;
/// 大文件直接分块编码到最终 ZIP，不经临时文件中转。
const STREAM_THRESHOLD: u64 = 8 << 20;

/// 小文件压缩完成后的产物。
struct Compressed {
    method: u16,
    crc: u32,
    size: u64,
    data: Vec<u8>,
}

/// 中央目录需要的每条目信息。
struct CentralRecord {
    name: Vec<u8>,
    method: u16,
    flags: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    csize: u64,
    usize_: u64,
    mtime: Option<i64>,
    external_attrs: u32,
    local_offset: u64,
    /// 加密条目：(AE 版本, 真实方法)，中央目录要再写一次 0x9901
    aes: Option<(u16, u16)>,
    /// 本地头已为流式输出预留 ZIP64 大小字段。
    local_zip64: bool,
}

/// 写出一个 ZIP，返回写入过程中产生的 warning（目前只有 Mac 元数据读取/跳过的提示）。
pub fn write_zip(dest: &Path, sources: &Sources, opts: &CreateOptions, ctx: &OpContext<'_>) -> Result<Vec<Warning>> {
    let threads = crate::sys::compression_threads(opts.threads, 32 << 20);
    let level = Compression::new(zip_level(opts.level));
    // 编码参数：级别 0 = 存储；有密码 = 每个文件 / 链接 / AppleDouble 条目都 AES-256 加密（目录条目没有数据，不加密）
    let enc = EncodeOpts { deflate: (opts.level != 0).then_some(level), password: opts.password.as_deref().map(str::as_bytes), parallelism: 1 };
    let file = File::create(dest)?;
    let mut out = CountingWriter { inner: BufWriter::with_capacity(1 << 20, file), offset: 0 };
    let throttle = Mutex::new(Throttle::new(ctx, Some(sources.total_bytes), Some(sources.items.len() as u64)));
    let mut records: Vec<CentralRecord> = Vec::with_capacity(sources.items.len());
    let mut warnings: Vec<Warning> = Vec::new();
    if enc.password.is_some() && opts.encrypt_names {
        warnings.push(Warning::bare(WarningKind::ZipCannotEncryptNames));
    }

    // 分批并行编码，主线程只负责写入，不占用工作线程。旧的 scope 消费者会占住唯一线程，
    // 单线程时生产者永远无法运行；批次还限制了乱序结果驻留，避免 pending 随整包增长。
    let jobs = sources.items.iter().filter(|i| i.kind == EntryKind::File).map(|i| i.size.div_ceil(DEFLATE_CHUNK as u64).max(1)).sum::<u64>();
    let threads = threads.min(jobs.max(1) as usize).max(1);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().map_err(Error::other)?;
    let mut macosx_dirs = BTreeSet::new();
    for batch in sources.items.chunks(threads) {
        ctx.cancel.check()?;
        let compressed: Result<Vec<_>> = pool.install(|| {
            batch.par_iter().map(|item| if item.kind == EntryKind::File && item.size < STREAM_THRESHOLD { compress_file(item, enc, &throttle, opts.level).map(Some) } else { Ok(None) }).collect()
        });
        let compressed = compressed.map_err(|error| if ctx.cancel.is_cancelled() { Error::Cancelled } else { error })?;
        for (item, compressed) in batch.iter().zip(compressed) {
            if item.kind != EntryKind::File {
                throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.begin_entry(&item.rel)?;
            }
            match item.kind {
                EntryKind::Dir => {
                    let name = format!("{}/", item.rel).into_bytes();
                    let rec = write_local(&mut out, name, METHOD_STORE, 0, 0, 0, item, 0x10, None)?;
                    records.push(rec);
                    if opts.mac_metadata {
                        write_apple_double(&mut out, &mut records, item, &mut macosx_dirs, enc, &mut warnings)?;
                    }
                }
                EntryKind::Symlink => {
                    let target = item.link_target.clone().unwrap_or_default();
                    let crc = crc32fast::hash(&target);
                    // 链接目标也是内容，有密码就一样加密；很短，不压缩
                    let (_, payload) = encode_bytes(&target, EncodeOpts { deflate: None, ..enc })?;
                    let rec = write_local(&mut out, item.rel.clone().into_bytes(), METHOD_STORE, crc, payload.len() as u64, target.len() as u64, item, 0, enc.ae_version(target.len() as u64))?;
                    out.write_all(&payload)?;
                    records.push(rec);
                }
                EntryKind::File => {
                    let rec = if item.size >= STREAM_THRESHOLD {
                        write_streamed_file(&mut out, item, EncodeOpts { parallelism: threads, ..enc }, &throttle, opts.level, &pool, if opts.threads == 0 { threads.min(4) } else { threads })?
                    } else {
                        let comp = compressed.expect("小文件条目必须有压缩结果");
                        let rec = write_local(&mut out, item.rel.clone().into_bytes(), comp.method, comp.crc, comp.data.len() as u64, comp.size, item, 0, enc.ae_version(comp.size))?;
                        out.write_all(&comp.data)?;
                        rec
                    };
                    records.push(rec);
                    if opts.mac_metadata {
                        write_apple_double(&mut out, &mut records, item, &mut macosx_dirs, enc, &mut warnings)?;
                    }
                }
            }
        }
    }

    ctx.cancel.check()?;
    // 中央目录
    let cd_start = out.offset;
    for rec in &records {
        write_central(&mut out, rec)?;
    }
    let cd_size = out.offset - cd_start;
    let need_zip64 = records.len() as u64 >= 0xFFFF || cd_start >= ZIP64_LIMIT || cd_size >= ZIP64_LIMIT;
    if need_zip64 {
        let eocd64_offset = out.offset;
        out.write_u32(SIG_EOCD64)?;
        out.write_u64(44)?; // 记录剩余长度
        out.write_u16(VERSION_MADE_BY)?;
        out.write_u16(45)?;
        out.write_u32(0)?;
        out.write_u32(0)?;
        out.write_u64(records.len() as u64)?;
        out.write_u64(records.len() as u64)?;
        out.write_u64(cd_size)?;
        out.write_u64(cd_start)?;
        out.write_u32(SIG_EOCD64_LOC)?;
        out.write_u32(0)?;
        out.write_u64(eocd64_offset)?;
        out.write_u32(1)?;
    }
    out.write_u32(SIG_EOCD)?;
    out.write_u16(0)?;
    out.write_u16(0)?;
    out.write_u16(records.len().min(0xFFFF) as u16)?;
    out.write_u16(records.len().min(0xFFFF) as u16)?;
    out.write_u32(cd_size.min(ZIP64_LIMIT) as u32)?;
    out.write_u32(cd_start.min(ZIP64_LIMIT) as u32)?;
    out.write_u16(0)?;
    out.inner.flush()?;
    out.inner.get_ref().sync_all()?;
    ctx.cancel.check()?;
    throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.finish();
    Ok(warnings)
}

/// 「Mac 专用」：读 item 的 Finder 信息、扩展属性、资源分支，打成 AppleDouble 写成
/// `__MACOSX/<同目录>/._<名字>` 条目，紧跟在 item 自己的条目后面；没有元数据就什么都不写。
/// 读元数据失败只记 warning 不中断——丢一个文件的标签不值得让整个包失败。符号链接由调用方跳过。
fn write_apple_double(
    out: &mut CountingWriter,
    records: &mut Vec<CentralRecord>,
    item: &SourceItem,
    macosx_dirs: &mut BTreeSet<String>,
    enc: EncodeOpts<'_>,
    warnings: &mut Vec<Warning>,
) -> Result<()> {
    let meta = match apple_double::read(&item.path, warnings) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(Warning::with_detail(WarningKind::MacMetadataReadFailed, item.path.display().to_string(), e.to_string()));
            return Ok(());
        }
    };
    if meta.is_empty() {
        return Ok(());
    }
    let ad_rel = apple_double::apple_double_rel(&item.rel);
    ensure_macosx_dirs(out, records, &ad_rel, item, macosx_dirs)?;
    let raw = apple_double::encode(&meta);
    let (method, payload) = encode_bytes(&raw, enc)?;
    // `._` 条目和 ditto 一样：普通文件、0644、时间跟原条目
    let ad_item = SourceItem { rel: ad_rel.clone(), path: item.path.clone(), kind: EntryKind::File, size: raw.len() as u64, mtime: item.mtime, mode: Some(0o644), link_target: None };
    let rec = write_local(out, ad_rel.into_bytes(), method, crc32fast::hash(&raw), payload.len() as u64, raw.len() as u64, &ad_item, 0, enc.ae_version(raw.len() as u64))?;
    out.write_all(&payload)?;
    records.push(rec);
    Ok(())
}

/// 补 `__MACOSX/`、`__MACOSX/a/`、`__MACOSX/a/b/` 这些目录条目（ditto 也会写），每个只写一次。
fn ensure_macosx_dirs(out: &mut CountingWriter, records: &mut Vec<CentralRecord>, ad_rel: &str, item: &SourceItem, done: &mut BTreeSet<String>) -> Result<()> {
    let parent = ad_rel.rsplit_once('/').map(|(p, _)| p).unwrap_or_default();
    let mut acc = String::with_capacity(parent.len());
    for comp in parent.split('/') {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(comp);
        if done.insert(acc.clone()) {
            let dir_item = SourceItem { rel: acc.clone(), path: item.path.clone(), kind: EntryKind::Dir, size: 0, mtime: item.mtime, mode: Some(0o755), link_target: None };
            let rec = write_local(out, format!("{acc}/").into_bytes(), METHOD_STORE, 0, 0, 0, &dir_item, 0x10, None)?;
            records.push(rec);
        }
    }
    Ok(())
}

/// 一个条目的编码参数：是否 deflate（None = 存储）、口令（Some = AES-256 加密）。
#[derive(Clone, Copy)]
struct EncodeOpts<'a> {
    deflate: Option<Compression>,
    password: Option<&'a [u8]>,
    /// 大文件共享当前 Rayon 池，批次内每个文件只占分给自己的并行额度。
    parallelism: usize,
}

impl EncodeOpts<'_> {
    /// 加密时每个条目多出的固定字节：盐 + 口令校验值 + 认证码。
    fn overhead(&self) -> u64 {
        if self.password.is_some() {
            zipaes::overhead(AES_STRENGTH)
        } else {
            0
        }
    }

    /// 条目要用的 AE 版本：不加密 None；明文短于 20 字节用 AE-2（CRC 写 0），其余 AE-1（保留 CRC）。
    /// 这是 WinZip 自己的规则：CRC 能被穷举反推很短的明文，长文件则靠 CRC 多一道完整性校验。
    fn ae_version(&self, plain_len: u64) -> Option<u16> {
        self.password.map(|_| if plain_len < AE2_BELOW { 2 } else { 1 })
    }
}

/// 把输入压缩后按需加密；加密包装只创建一次，各块保持标准 WinZip AES 流。
fn encode_stream<W: Write>(input: &mut (dyn Read + Send), sink: W, opts: EncodeOpts<'_>) -> Result<(W, u32, u64)> {
    with_encryption(sink, opts.password, |out| {
        let (_, crc, size) = encode_payload(input, out, opts)?;
        Ok((crc, size))
    })
}

/// 加密只包装输出流一次，文件按偏移读取与小条目顺序编码共用同一套 AES 收尾。
fn with_encryption<W: Write>(mut sink: W, password: Option<&[u8]>, encode: impl FnOnce(&mut dyn Write) -> Result<(u32, u64)>) -> Result<(W, u32, u64)> {
    if let Some(pw) = password {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).map_err(|e| Error::Other(format!("取不到随机盐：{e}")))?;
        let mut enc = zipaes::AesWriter::new(sink, pw, AES_STRENGTH, &salt)?;
        let (crc, size) = encode(&mut enc)?;
        Ok((enc.finish()?, crc, size))
    } else {
        let (crc, size) = encode(&mut sink)?;
        Ok((sink, crc, size))
    }
}

/// 小条目压缩或原样拷贝；文件流式路径单独按偏移并行读取。
fn encode_payload<W: Write>(input: &mut (dyn Read + Send), mut sink: W, opts: EncodeOpts<'_>) -> Result<(W, u32, u64)> {
    if let Some(level) = opts.deflate {
        let mut hasher = crc32fast::Hasher::new();
        let mut size = 0;
        let mut deflate = DeflateEncoder::new(sink, level);
        copy_with_crc(input, &mut deflate, &mut hasher, &mut size)?;
        Ok((deflate.finish()?, hasher.finalize(), size))
    } else {
        let mut hasher = crc32fast::Hasher::new();
        let mut size = 0;
        copy_with_crc(input, &mut sink, &mut hasher, &mut size)?;
        Ok((sink, hasher.finalize(), size))
    }
}

/// 4 MiB 足以保留 ZIP 的 32 KiB 字典收益，同时让较小的大文件也能分配到多个核心。
const DEFLATE_CHUNK: usize = 4 << 20;

/// 独立块用 Sync Flush 保持字节对齐且不写 BFINAL；拼接后只在末尾补一个结束块，仍是标准 DEFLATE。
fn deflate_chunk(raw: &[u8], level: Compression, finish: bool) -> Result<Vec<u8>> {
    let mut encoder = flate2::Compress::new(level, false);
    let mut packed = Vec::with_capacity(raw.len() + raw.len() / 16 + 128);
    let flush = if finish { flate2::FlushCompress::Finish } else { flate2::FlushCompress::Sync };
    loop {
        let status = encoder.compress_vec(&raw[encoder.total_in() as usize..], &mut packed, flush).map_err(Error::other)?;
        if status == flate2::Status::StreamEnd || (!finish && encoder.total_in() == raw.len() as u64 && packed.len() < packed.capacity()) {
            return Ok(packed);
        }
        packed.reserve(DEFLATE_CHUNK);
    }
}

/// 直接写 DEFLATE 原样块，通过 writev 避免把整块输入再拷贝到另一个输出缓冲。
fn write_stored_blocks(out: &mut dyn Write, raw: &[u8]) -> io::Result<()> {
    let headers: Vec<[u8; 5]> = raw
        .chunks(u16::MAX as usize)
        .map(|block| {
            let len = block.len() as u16;
            let a = len.to_le_bytes();
            let b = (!len).to_le_bytes();
            [0, a[0], a[1], b[0], b[1]]
        })
        .collect();
    let mut slices: Vec<IoSlice<'_>> = headers.iter().zip(raw.chunks(u16::MAX as usize)).flat_map(|(header, block)| [IoSlice::new(header), IoSlice::new(block)]).collect();
    let mut remaining = slices.as_mut_slice();
    while !remaining.is_empty() {
        let n = out.write_vectored(remaining)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        IoSlice::advance_slices(&mut remaining, n);
    }
    Ok(())
}

/// 一个编码块持有原样输入或已压缩数据，不为原样块创建第二份全尺寸副本。
struct EncodedChunk {
    data: Vec<u8>,
    stored: bool,
    crc: crc32fast::Hasher,
}

/// 根据块内头中尾样本跳过无收益的匹配搜索，高级别始终完整压缩。
fn encode_chunk(raw: Vec<u8>, level: Compression) -> Result<EncodedChunk> {
    let mut crc = crc32fast::Hasher::new();
    crc.update(&raw);
    if level.level() <= 6 && raw.len() >= 3 * 65536 {
        let mut probe = DeflateEncoder::new(Vec::new(), Compression::fast());
        for start in [0, raw.len() / 2 - 32768, raw.len() - 65536] {
            probe.write_all(&raw[start..start + 65536])?;
        }
        if probe.finish()?.len() >= 3 * 65536 * 99 / 100 {
            return Ok(EncodedChunk { data: raw, stored: true, crc });
        }
    }
    Ok(EncodedChunk { data: deflate_chunk(&raw, level, false)?, stored: false, crc })
}

/// 按偏移并发读入及编码，主线程按顺序写盘；一个排队批次限制预读内存。
fn parallel_file(input: &File, input_size: u64, out: &mut dyn Write, opts: EncodeOpts<'_>, pool: &rayon::ThreadPool, throttle: &Mutex<Throttle<'_>>) -> Result<(u32, u64)> {
    pool.in_place_scope(|scope| {
        let (tx, rx) = mpsc::sync_channel::<Result<Vec<EncodedChunk>>>(1);
        scope.spawn(move |_| {
            let blocks = input_size.div_ceil(DEFLATE_CHUNK as u64);
            for first in (0..blocks).step_by(opts.parallelism) {
                let end = (first + opts.parallelism as u64).min(blocks);
                let encoded: Result<Vec<_>> = (first..end)
                    .into_par_iter()
                    .map(|block| {
                        throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.add_bytes(0)?;
                        let offset = block * DEFLATE_CHUNK as u64;
                        let mut raw = vec![0; (input_size - offset).min(DEFLATE_CHUNK as u64) as usize];
                        read_exact_at(input, &mut raw, offset)?;
                        throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.add_bytes(raw.len() as u64)?;
                        if let Some(level) = opts.deflate {
                            encode_chunk(raw, level)
                        } else {
                            let mut crc = crc32fast::Hasher::new();
                            crc.update(&raw);
                            Ok(EncodedChunk { data: raw, stored: true, crc })
                        }
                    })
                    .collect();
                let failed = encoded.is_err();
                if tx.send(encoded).is_err() || failed {
                    break;
                }
            }
        });
        let result = (|| -> Result<_> {
            let mut crc = crc32fast::Hasher::new();
            for batch in &rx {
                for chunk in batch? {
                    throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.add_bytes(0)?;
                    if opts.deflate.is_none() {
                        out.write_all(&chunk.data)?;
                    } else if chunk.stored {
                        write_stored_blocks(out, &chunk.data)?;
                    } else {
                        out.write_all(&chunk.data)?;
                    }
                    crc.combine(&chunk.crc);
                }
            }
            if let Some(level) = opts.deflate {
                out.write_all(&deflate_chunk(&[], level, true)?)?;
            }
            Ok((crc.finalize(), input_size))
        })();
        drop(rx);
        result
    })
}

/// 不共享可变文件游标；普通读取失败（含源文件被截短）直接向调用方返回。
fn read_exact_at(input: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        input.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = input.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
}

/// 在内存里编码一小段数据（符号链接目标、AppleDouble，最多 64 MB 量级），返回 (方法, 包内字节)。
/// 先试 deflate，没变小就改存储，和文件条目的规则一致；比较时不算加密开销。
fn encode_bytes(raw: &[u8], opts: EncodeOpts<'_>) -> Result<(u16, Vec<u8>)> {
    if opts.deflate.is_some() {
        let (packed, _, _) = encode_stream(&mut &raw[..], Vec::with_capacity(raw.len() / 2 + 64), opts)?;
        if packed.len() as u64 - opts.overhead() < raw.len() as u64 {
            return Ok((METHOD_DEFLATE, packed));
        }
    }
    let (packed, _, _) = encode_stream(&mut &raw[..], Vec::with_capacity(raw.len() + 64), EncodeOpts { deflate: None, ..opts })?;
    Ok((METHOD_STORE, packed))
}

/// 把 0–9 的通用级别映射成 deflate 级别（0 = 存储，由调用方单独处理）。
fn zip_level(level: u8) -> u32 {
    level.clamp(1, 9) as u32
}

/// 根据内容选编码参数；普通级别对不可压缩内容直接存储。
fn file_encoding<'a>(input: &mut File, size: u64, mut opts: EncodeOpts<'a>, level: u8) -> Result<EncodeOpts<'a>> {
    if opts.deflate.is_some() && level <= 6 && size >= 1024 * 1024 && unlikely_to_shrink(input, size)? {
        opts.deflate = None;
    }
    Ok(opts)
}

/// 小文件在池中编码，压完未变小则回退存储。
fn compress_file(item: &SourceItem, opts: EncodeOpts<'_>, throttle: &Mutex<Throttle<'_>>, level: u8) -> Result<Compressed> {
    throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.begin_entry(&item.rel)?;
    let mut file = File::open(&item.path)?;
    let opts = file_encoding(&mut file, item.size, opts, level)?;
    let mut input = ProgressRead { inner: file, throttle, count_bytes: true };
    let method = if opts.deflate.is_some() { METHOD_DEFLATE } else { METHOD_STORE };
    let (data, crc, size) = encode_stream(&mut input, Vec::with_capacity(item.size as usize + 64), opts)?;
    if method == METHOD_DEFLATE && data.len() as u64 - opts.overhead() >= size {
        input.inner.rewind()?;
        input.count_bytes = false;
        let (data, crc, size) = encode_stream(&mut input, Vec::with_capacity(item.size as usize + 64), EncodeOpts { deflate: None, ..opts })?;
        return Ok(Compressed { method: METHOD_STORE, crc, size, data });
    }
    Ok(Compressed { method, crc, size, data })
}

/// 大文件一次顺序写入最终输出，再回填本地头的 CRC 与长度，不产生临时压缩文件。
fn write_streamed_file(
    out: &mut CountingWriter,
    item: &SourceItem,
    opts: EncodeOpts<'_>,
    throttle: &Mutex<Throttle<'_>>,
    level: u8,
    pool: &rayon::ThreadPool,
    io_threads: usize,
) -> Result<CentralRecord> {
    throttle.lock().map_err(|_| Error::Other("进度锁中毒".into()))?.begin_entry(&item.rel)?;
    let mut file = File::open(&item.path)?;
    let mut opts = file_encoding(&mut file, item.size, opts, level)?;
    // 原样写入的瓶颈在存储，实测增加到全部 CPU 反而产生额外调度及内存流量；显式线程参数仍可覆盖。
    if opts.deflate.is_none() {
        opts.parallelism = io_threads;
    }
    let method = if opts.deflate.is_some() { METHOD_DEFLATE } else { METHOD_STORE };
    // 为可能越过 ZIP32 上限的压缩结果预留 ZIP64 字段；实际长度始终在编码后回填。
    let bound = item.size.saturating_add(item.size / 16).saturating_add(1024);
    let placeholder = if bound >= ZIP64_LIMIT { ZIP64_LIMIT + 1 } else { 0 };
    let header_size = if placeholder >= ZIP64_LIMIT { ZIP64_LIMIT + 1 } else { item.size };
    let mut record = write_local(out, item.rel.clone().into_bytes(), method, 0, placeholder, header_size, item, 0, opts.ae_version(item.size))?;
    let start = out.offset;
    let (_, crc, size) = with_encryption(&mut *out, opts.password, |writer| parallel_file(&file, item.size, writer, opts, pool, throttle))?;
    record.csize = out.offset - start;
    record.usize_ = size;
    record.crc = if record.aes.is_some_and(|(version, _)| version == 2) { 0 } else { crc };
    out.inner.flush()?;
    out.inner.seek(SeekFrom::Start(record.local_offset + 14))?;
    out.inner.write_all(&record.crc.to_le_bytes())?;
    if !record.local_zip64 {
        out.inner.write_all(&(record.csize as u32).to_le_bytes())?;
        out.inner.write_all(&(record.usize_ as u32).to_le_bytes())?;
    } else {
        // ZIP64 在所有本地扩展字段中排第一，跳过 tag 和长度后依次是原始长度、压缩长度。
        out.inner.seek(SeekFrom::Start(record.local_offset + 30 + record.name.len() as u64 + 4))?;
        out.inner.write_all(&record.usize_.to_le_bytes())?;
        out.inner.write_all(&record.csize.to_le_bytes())?;
    }
    out.inner.seek(SeekFrom::Start(out.offset))?;
    Ok(record)
}

/// 取三个 64 KiB 样本快速试压；收益不足 1% 时选择存储。不会根据扩展名跳过可压缩内容。
fn unlikely_to_shrink(input: &mut File, size: u64) -> Result<bool> {
    const SAMPLE: usize = 64 * 1024;
    let mut raw = vec![0; SAMPLE * 3];
    for (i, offset) in [0, size / 2 - SAMPLE as u64 / 2, size - SAMPLE as u64].into_iter().enumerate() {
        input.seek(SeekFrom::Start(offset))?;
        input.read_exact(&mut raw[i * SAMPLE..(i + 1) * SAMPLE])?;
    }
    input.rewind()?;
    // 已压缩媒体的文件头常有大量可压缩元数据，不能让头部误导主体判断；主体仍用内容试压确认。
    let media = raw.starts_with(b"\x89PNG\r\n\x1a\n") || raw.starts_with(b"\xff\xd8\xff") || raw.get(4..8) == Some(b"ftyp") || raw.starts_with(b"\x1a\x45\xdf\xa3");
    if media {
        let mut body = DeflateEncoder::new(Vec::new(), Compression::fast());
        body.write_all(&raw[SAMPLE..])?;
        if body.finish()?.len() >= SAMPLE * 2 * 99 / 100 {
            return Ok(true);
        }
    }
    let mut deflate = DeflateEncoder::new(Vec::new(), Compression::fast());
    deflate.write_all(&raw)?;
    Ok(deflate.finish()?.len() >= raw.len() * 99 / 100)
}

/// 按读取字节报告进度；回退到存储的第二遍不重复计数，仍检查取消。
struct ProgressRead<'a, 'b> {
    inner: File,
    throttle: &'a Mutex<Throttle<'b>>,
    count_bytes: bool,
}

impl Read for ProgressRead<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut throttle = self.throttle.lock().map_err(|_| io::Error::other("进度锁中毒"))?;
        throttle.add_bytes(0).map_err(io::Error::other)?;
        drop(throttle);
        let n = self.inner.read(buf)?;
        if self.count_bytes {
            self.throttle.lock().map_err(|_| io::Error::other("进度锁中毒"))?.add_bytes(n as u64).map_err(io::Error::other)?;
        }
        Ok(n)
    }
}

/// 边拷贝边算 CRC32 和字节数。
fn copy_with_crc(input: &mut dyn Read, out: &mut dyn Write, hasher: &mut crc32fast::Hasher, size: &mut u64) -> io::Result<()> {
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        *size += n as u64;
    }
}

/// 带偏移计数的写入器，中央目录要记录每个 local header 的位置。
struct CountingWriter {
    inner: BufWriter<File>,
    offset: u64,
}

impl CountingWriter {
    fn write_u16(&mut self, v: u16) -> io::Result<()> {
        self.write_all(&v.to_le_bytes())
    }
    fn write_u32(&mut self, v: u32) -> io::Result<()> {
        self.write_all(&v.to_le_bytes())
    }
    fn write_u64(&mut self, v: u64) -> io::Result<()> {
        self.write_all(&v.to_le_bytes())
    }
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.offset += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let n = self.inner.write_vectored(bufs)?;
        self.offset += n as u64;
        Ok(n)
    }
}

/// Unix 秒转 DOS 时间/日期（本地时间，分辨率 2 秒；1980 前记成 1980-01-01）。
fn dos_datetime(mtime: Option<i64>) -> (u16, u16) {
    let Some(secs) = mtime else { return (0, 0x21) };
    let local = local_components(secs);
    let (y, mo, d, h, mi, s) = local;
    if y < 1980 {
        return (0, 0x21);
    }
    let date = (((y - 1980) as u16) << 9) | ((mo as u16) << 5) | d as u16;
    let time = ((h as u16) << 11) | ((mi as u16) << 5) | (s as u16 / 2);
    (time, date)
}

/// 按系统时区拆分时间；Windows 使用 CRT 的 localtime_s，Unix 使用 localtime_r。
/// 平台接口拒绝超出范围的时间时回落到 UTC，避免使用未初始化的 tm。
fn local_components(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let timestamp = secs as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY：缓冲具有 libc::tm 的平台布局；接口同步初始化它，成功后才读取。
    #[cfg(unix)]
    let valid = unsafe { !libc::localtime_r(&timestamp, tm.as_mut_ptr()).is_null() };
    #[cfg(windows)]
    let valid = unsafe { libc::localtime_s(tm.as_mut_ptr(), &timestamp) == 0 };
    #[cfg(not(any(unix, windows)))]
    let valid = false;
    if valid {
        // SAFETY：平台函数返回成功，所有 tm 字段均已初始化。
        let tm = unsafe { tm.assume_init() };
        return (tm.tm_year + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32);
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32)
}

/// 天数（自 1970-01-01）转公历日期，Howard Hinnant 的算法。
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

/// 条目的扩展字段：zip64（按需）+ UT 时间戳 + WinZip AES（0x9901：AE 版本、"AE"、强度、真实方法）。
fn extra_fields(csize: u64, usize_: u64, offset: Option<u64>, mtime: Option<i64>, aes: Option<(u16, u16)>) -> Vec<u8> {
    let mut extra = Vec::new();
    let need64 = csize >= ZIP64_LIMIT || usize_ >= ZIP64_LIMIT || offset.is_some_and(|o| o >= ZIP64_LIMIT);
    if need64 {
        let mut body = Vec::new();
        if usize_ >= ZIP64_LIMIT {
            body.extend_from_slice(&usize_.to_le_bytes());
        }
        if csize >= ZIP64_LIMIT {
            body.extend_from_slice(&csize.to_le_bytes());
        }
        if let Some(o) = offset.filter(|o| *o >= ZIP64_LIMIT) {
            body.extend_from_slice(&o.to_le_bytes());
        }
        extra.extend_from_slice(&0x0001u16.to_le_bytes());
        extra.extend_from_slice(&(body.len() as u16).to_le_bytes());
        extra.extend_from_slice(&body);
    }
    if let Some(m) = mtime {
        if let Ok(m32) = i32::try_from(m) {
            extra.extend_from_slice(&0x5455u16.to_le_bytes());
            extra.extend_from_slice(&5u16.to_le_bytes());
            extra.push(1); // 只有 mtime
            extra.extend_from_slice(&m32.to_le_bytes());
        }
    }
    if let Some((vendor_version, method)) = aes {
        extra.extend_from_slice(&0x9901u16.to_le_bytes());
        extra.extend_from_slice(&7u16.to_le_bytes());
        extra.extend_from_slice(&vendor_version.to_le_bytes());
        extra.extend_from_slice(b"AE");
        extra.push(AES_STRENGTH);
        extra.extend_from_slice(&method.to_le_bytes());
    }
    extra
}

/// 写 local header，返回中央目录记录。`dos_attr` 目录为 0x10；`aes` 是 AE 版本（None = 不加密）：
/// 加密时方法字段写 99、真实方法进 0x9901 扩展字段、置 bit 0，AE-2 的 CRC 字段按规范写 0。
/// 参数就是 ZIP 头的字段，拆成结构体只会多一层壳，这里允许超过 7 个。
#[allow(clippy::too_many_arguments)]
fn write_local(out: &mut CountingWriter, name: Vec<u8>, method: u16, crc: u32, csize: u64, usize_: u64, item: &SourceItem, dos_attr: u32, aes: Option<u16>) -> Result<CentralRecord> {
    let local_offset = out.offset;
    let (dos_time, dos_date) = dos_datetime(item.mtime);
    let aes = aes.map(|version| (version, method));
    let (method, crc, flags) = match aes {
        Some((2, _)) => (METHOD_AES, 0, FLAG_UTF8 | FLAG_ENCRYPTED),
        Some(_) => (METHOD_AES, crc, FLAG_UTF8 | FLAG_ENCRYPTED),
        None => (method, crc, FLAG_UTF8),
    };
    let extra = extra_fields(csize, usize_, None, item.mtime, aes);
    let need64 = csize >= ZIP64_LIMIT || usize_ >= ZIP64_LIMIT;
    out.write_u32(SIG_LOCAL)?;
    out.write_u16(version_needed(need64, aes.is_some()))?;
    out.write_u16(flags)?;
    out.write_u16(method)?;
    out.write_u16(dos_time)?;
    out.write_u16(dos_date)?;
    out.write_u32(crc)?;
    out.write_u32(csize.min(ZIP64_LIMIT) as u32)?;
    out.write_u32(usize_.min(ZIP64_LIMIT) as u32)?;
    out.write_u16(name.len() as u16)?;
    out.write_u16(extra.len() as u16)?;
    out.write_all(&name)?;
    out.write_all(&extra)?;
    let mode = match item.kind {
        EntryKind::Dir => (item.mode.unwrap_or(0o755) & 0o7777) | 0o040000,
        EntryKind::Symlink => 0o120777,
        EntryKind::File => (item.mode.unwrap_or(0o644) & 0o7777) | 0o100000,
    };
    Ok(CentralRecord { name, method, flags, dos_time, dos_date, crc, csize, usize_, mtime: item.mtime, external_attrs: (mode << 16) | dos_attr, local_offset, aes, local_zip64: need64 })
}

/// 解压所需版本：AES 要 5.1（WinZip 规范），zip64 要 4.5，其余 2.0。
fn version_needed(need64: bool, aes: bool) -> u16 {
    if aes {
        51
    } else if need64 {
        45
    } else {
        20
    }
}

/// 写一条中央目录记录。
fn write_central(out: &mut CountingWriter, rec: &CentralRecord) -> Result<()> {
    let extra = extra_fields(rec.csize, rec.usize_, Some(rec.local_offset), rec.mtime, rec.aes);
    let need64 = rec.local_zip64 || rec.csize >= ZIP64_LIMIT || rec.usize_ >= ZIP64_LIMIT || rec.local_offset >= ZIP64_LIMIT;
    out.write_u32(SIG_CENTRAL)?;
    out.write_u16(VERSION_MADE_BY)?;
    out.write_u16(version_needed(need64, rec.aes.is_some()))?;
    out.write_u16(rec.flags)?;
    out.write_u16(rec.method)?;
    out.write_u16(rec.dos_time)?;
    out.write_u16(rec.dos_date)?;
    out.write_u32(rec.crc)?;
    out.write_u32(rec.csize.min(ZIP64_LIMIT) as u32)?;
    out.write_u32(rec.usize_.min(ZIP64_LIMIT) as u32)?;
    out.write_u16(rec.name.len() as u16)?;
    out.write_u16(extra.len() as u16)?;
    out.write_u16(0)?;
    out.write_u16(0)?;
    out.write_u16(0)?;
    out.write_u32(rec.external_attrs)?;
    out.write_u32(rec.local_offset.min(ZIP64_LIMIT) as u32)?;
    out.write_all(&rec.name)?;
    out.write_all(&extra)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ZIP64 只为饱和的定长字段写扩展值，顺序必须与读取需求一致。
    #[test]
    fn zip64_extra_is_positional() {
        let big = ZIP64_LIMIT + 1;
        for (packed, size, offset, expected) in [(1, big, None, vec![big]), (big, 2, None, vec![big]), (1, 2, Some(big), vec![big]), (big, big, Some(big), vec![big, big, big])] {
            let extra = extra_fields(packed, size, offset, None, None);
            assert_eq!(&extra[..2], &1u16.to_le_bytes());
            assert_eq!(u16::from_le_bytes(extra[2..4].try_into().unwrap()) as usize, expected.len() * 8);
            let values: Vec<_> = extra[4..].chunks_exact(8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).collect();
            assert_eq!(values, expected);
        }
    }

    /// 输出失败时接收端先关闭，单工作线程的生产者也必须退出，不能卡在有界通道。
    #[test]
    fn pipeline_write_failure_does_not_deadlock() {
        struct Fail;
        impl Write for Fail {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("test output failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let path = std::env::temp_dir().join(format!("piesi-pipeline-fail-{}", std::process::id()));
        std::fs::write(&path, vec![42; DEFLATE_CHUNK * 4]).unwrap();
        let file = File::open(&path).unwrap();
        let ctx = OpContext::silent();
        let throttle = Mutex::new(Throttle::new(&ctx, None, None));
        let opts = EncodeOpts { deflate: Some(Compression::fast()), password: None, parallelism: 1 };
        let result = parallel_file(&file, (DEFLATE_CHUNK * 4) as u64, &mut Fail, opts, &pool, &throttle);
        assert!(result.is_err());
        std::fs::remove_file(path).unwrap();
    }
}
