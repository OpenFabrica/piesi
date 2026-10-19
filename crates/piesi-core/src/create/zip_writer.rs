//! 自写 ZIP 写入器：UTF-8 + bit 11、NFC（上游已做）、zip64、Unix 权限、UT 时间戳，
//! 多文件并行 deflate，可选「Mac 专用」的 `__MACOSX/._名字` AppleDouble 条目。
//! 不用 `zip` crate 的原因：它没有「接收已压缩数据」的接口，做不了并行。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, Write};
use std::path::Path;
use std::sync::mpsc;

use flate2::write::DeflateEncoder;
use flate2::Compression;

use crate::archive::EntryKind;
use crate::create::apple_double;
use crate::create::{CreateOptions, SourceItem, Sources};
use crate::progress::{OpContext, Throttle};
use crate::{Error, Result};

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_EOCD64: u32 = 0x0606_4b50;
const SIG_EOCD64_LOC: u32 = 0x0706_4b50;
const FLAG_UTF8: u16 = 1 << 11;
const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const VERSION_MADE_BY: u16 = (3 << 8) | 63; // 主机 Unix，规范 6.3
const ZIP64_LIMIT: u64 = 0xFFFF_FFFF;
/// 超过这个大小的压缩结果落到临时文件，避免一堆 worker 同时把大文件压在内存里
const SPILL_THRESHOLD: u64 = 64 * 1024 * 1024;

/// 一个条目压缩完成后的产物。
struct Compressed {
    index: usize,
    method: u16,
    crc: u32,
    size: u64,
    data: Payload,
}

/// 压缩后的数据存放处。
enum Payload {
    Memory(Vec<u8>),
    Spilled(tempfile_path::TempPath),
}

/// 极简临时文件：放在输出文件同目录，drop 时删除。
mod tempfile_path {
    use std::path::{Path, PathBuf};

    /// 自动删除的临时文件路径。
    pub struct TempPath(pub PathBuf);

    impl TempPath {
        /// 在 `dir` 下生成唯一临时文件名。
        pub fn new_in(dir: &Path, index: usize) -> Self {
            TempPath(dir.join(format!(".piesi-zip-{}-{index}.tmp", std::process::id())))
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
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
}

/// 写出一个 ZIP，返回写入过程中产生的 warning（目前只有 Mac 元数据读取/跳过的提示）。
pub fn write_zip(dest: &Path, sources: &Sources, opts: &CreateOptions, ctx: &OpContext<'_>) -> Result<Vec<String>> {
    let threads = if opts.threads == 0 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) } else { opts.threads };
    let level = Compression::new(zip_level(opts.level));
    let spill_dir = dest.parent().unwrap_or(Path::new(".")).to_path_buf();
    let file = File::create(dest)?;
    let mut out = CountingWriter { inner: BufWriter::with_capacity(1 << 20, file), offset: 0 };
    let mut throttle = Throttle::new(ctx, Some(sources.total_bytes), Some(sources.items.len() as u64));
    let mut records: Vec<CentralRecord> = Vec::with_capacity(sources.items.len());
    let mut warnings: Vec<String> = Vec::new();

    // 并行压缩：worker 池按 index 乱序产出，主线程用 BTreeMap 按序回收后顺序写入。
    // 有界通道限制同时在飞的结果数，内存不会随文件数线性增长。
    let items = &sources.items;
    let (tx, rx) = mpsc::sync_channel::<Result<Compressed>>(threads * 2);
    let cancel = ctx.cancel.clone();
    // scope 的闭包会迁移到线程池里执行，必须 Send：Receiver 按值移入，写入器等按显式可变引用捕获
    let out_ref = &mut out;
    let records_ref = &mut records;
    let throttle_ref = &mut throttle;
    let warnings_ref = &mut warnings;
    let spill_dir_ref = &spill_dir;
    let result: Result<()> = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(Error::other)?
        .scope(move |scope| {
            let out = out_ref;
            let records = records_ref;
            let throttle = throttle_ref;
            let warnings = warnings_ref;
            let spill_dir = spill_dir_ref;
            // 生产者：只有文件需要压缩；目录和符号链接在主线程直接写
            let tx_files = tx.clone();
            drop(tx);
            scope.spawn(move |s| {
                for (index, item) in items.iter().enumerate() {
                    if item.kind != EntryKind::File {
                        continue;
                    }
                    if cancel.is_cancelled() {
                        break;
                    }
                    let tx = tx_files.clone();
                    let spill_dir = spill_dir.clone();
                    s.spawn(move |_| {
                        let r = compress_file(index, item, level, opts.level == 0, &spill_dir);
                        let _ = tx.send(r);
                    });
                }
            });

            // 消费者（当前线程）：按 index 顺序写入；Mac 元数据的 `._` 条目紧跟在各自的条目后面，
            // 和 ditto 一样，`__MACOSX/…/` 目录条目只在第一次用到时补一条
            let mut pending: BTreeMap<usize, Compressed> = BTreeMap::new();
            let mut macosx_dirs: BTreeSet<String> = BTreeSet::new();
            for (index, item) in items.iter().enumerate() {
                throttle.begin_entry(&item.rel)?;
                match item.kind {
                    EntryKind::Dir => {
                        let name = format!("{}/", item.rel).into_bytes();
                        let rec = write_local(out, name, METHOD_STORE, 0, 0, 0, item, 0x10)?;
                        records.push(rec);
                        if opts.mac_metadata {
                            write_apple_double(out, records, item, &mut macosx_dirs, level, opts.level == 0, warnings)?;
                        }
                    }
                    EntryKind::Symlink => {
                        let target = item.link_target.clone().unwrap_or_default();
                        let crc = crc32fast::hash(&target);
                        let rec = write_local(out, item.rel.clone().into_bytes(), METHOD_STORE, crc, target.len() as u64, target.len() as u64, item, 0)?;
                        out.write_all(&target)?;
                        records.push(rec);
                    }
                    EntryKind::File => {
                        let comp = loop {
                            if let Some(c) = pending.remove(&index) {
                                break c;
                            }
                            match rx.recv() {
                                Ok(Ok(c)) => {
                                    pending.insert(c.index, c);
                                }
                                Ok(Err(e)) => return Err(e),
                                Err(_) => return Err(Error::Other("压缩线程提前退出".into())),
                            }
                        };
                        let csize = match &comp.data {
                            Payload::Memory(v) => v.len() as u64,
                            Payload::Spilled(p) => std::fs::metadata(&p.0)?.len(),
                        };
                        let rec = write_local(out, item.rel.clone().into_bytes(), comp.method, comp.crc, csize, comp.size, item, 0)?;
                        match comp.data {
                            Payload::Memory(v) => out.write_all(&v)?,
                            Payload::Spilled(p) => {
                                let mut f = File::open(&p.0)?;
                                io::copy(&mut f, out)?;
                            }
                        }
                        throttle.add_bytes(comp.size)?;
                        records.push(rec);
                        if opts.mac_metadata {
                            write_apple_double(out, records, item, &mut macosx_dirs, level, opts.level == 0, warnings)?;
                        }
                    }
                }
            }
            Ok(())
        });
    result?;

    // 中央目录
    let cd_start = out.offset;
    for rec in &records {
        write_central(&mut out, rec)?;
    }
    let cd_size = out.offset - cd_start;
    let need_zip64 = records.len() as u64 > 0xFFFF || cd_start > ZIP64_LIMIT || cd_size > ZIP64_LIMIT;
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
    throttle.finish();
    Ok(warnings)
}

/// 「Mac 专用」：读 item 的 Finder 信息、扩展属性、资源分支，打成 AppleDouble 写成
/// `__MACOSX/<同目录>/._<名字>` 条目，紧跟在 item 自己的条目后面；没有元数据就什么都不写。
/// 读元数据失败只记 warning 不中断——丢一个文件的标签不值得让整个包失败。符号链接由调用方跳过。
#[allow(clippy::too_many_arguments)]
fn write_apple_double(out: &mut CountingWriter, records: &mut Vec<CentralRecord>, item: &SourceItem, macosx_dirs: &mut BTreeSet<String>, level: Compression, store_only: bool, warnings: &mut Vec<String>) -> Result<()> {
    let meta = match apple_double::read(&item.path, warnings) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(format!("读取 Mac 元数据失败，已跳过：{}：{e}", item.path.display()));
            return Ok(());
        }
    };
    if meta.is_empty() {
        return Ok(());
    }
    let ad_rel = apple_double::apple_double_rel(&item.rel);
    ensure_macosx_dirs(out, records, &ad_rel, item, macosx_dirs)?;
    let raw = apple_double::encode(&meta);
    let (method, payload) = deflate_bytes(&raw, level, store_only)?;
    // `._` 条目和 ditto 一样：普通文件、0644、时间跟原条目
    let ad_item = SourceItem { rel: ad_rel.clone(), path: item.path.clone(), kind: EntryKind::File, size: raw.len() as u64, mtime: item.mtime, mode: Some(0o644), link_target: None };
    let rec = write_local(out, ad_rel.into_bytes(), method, crc32fast::hash(&raw), payload.len() as u64, raw.len() as u64, &ad_item, 0)?;
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
            let rec = write_local(out, format!("{acc}/").into_bytes(), METHOD_STORE, 0, 0, 0, &dir_item, 0x10)?;
            records.push(rec);
        }
    }
    Ok(())
}

/// 在内存里 deflate 一段小数据（AppleDouble 最多 64 MB 量级），没变小就改存储，和文件条目的规则一致。
fn deflate_bytes(raw: &[u8], level: Compression, store_only: bool) -> Result<(u16, Vec<u8>)> {
    if !store_only {
        let mut enc = DeflateEncoder::new(Vec::with_capacity(raw.len() / 2 + 64), level);
        enc.write_all(raw)?;
        let packed = enc.finish()?;
        if packed.len() < raw.len() {
            return Ok((METHOD_DEFLATE, packed));
        }
    }
    Ok((METHOD_STORE, raw.to_vec()))
}

/// 把 0–9 的通用级别映射成 deflate 级别（0 = 存储，由调用方单独处理）。
fn zip_level(level: u8) -> u32 {
    level.clamp(1, 9) as u32
}

/// 在 worker 线程里压缩一个文件：边读边算 CRC，结果放内存或临时文件。
/// 压完发现没变小（已压缩的媒体文件常见）就改为存储，和 7-Zip 的行为一致。
fn compress_file(index: usize, item: &SourceItem, level: Compression, store_only: bool, spill_dir: &Path) -> Result<Compressed> {
    let mut input = File::open(&item.path)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut size = 0u64;

    if item.size <= SPILL_THRESHOLD {
        // 小文件：压进内存
        let mut buf: Vec<u8> = Vec::with_capacity(item.size as usize + 64);
        let method = if store_only {
            copy_with_crc(&mut input, &mut buf, &mut hasher, &mut size)?;
            METHOD_STORE
        } else {
            let mut enc = DeflateEncoder::new(&mut buf, level);
            copy_with_crc(&mut input, &mut enc, &mut hasher, &mut size)?;
            enc.finish()?;
            if buf.len() as u64 >= size {
                buf.clear();
                input.rewind()?;
                let mut h2 = crc32fast::Hasher::new();
                let mut s2 = 0;
                copy_with_crc(&mut input, &mut buf, &mut h2, &mut s2)?;
                METHOD_STORE
            } else {
                METHOD_DEFLATE
            }
        };
        return Ok(Compressed { index, method, crc: hasher.finalize(), size, data: Payload::Memory(buf) });
    }

    // 大文件：压到临时文件。TempPath 必须活到 Payload 被消费完，它 drop 时会删文件。
    let tmp = tempfile_path::TempPath::new_in(spill_dir, index);
    let method = if store_only {
        let mut f = BufWriter::new(File::create(&tmp.0)?);
        copy_with_crc(&mut input, &mut f, &mut hasher, &mut size)?;
        f.flush()?;
        METHOD_STORE
    } else {
        let mut enc = DeflateEncoder::new(BufWriter::new(File::create(&tmp.0)?), level);
        copy_with_crc(&mut input, &mut enc, &mut hasher, &mut size)?;
        let mut w = enc.finish()?;
        w.flush()?;
        drop(w);
        if std::fs::metadata(&tmp.0)?.len() >= size {
            // 没变小：重新按存储写一遍
            let mut f = BufWriter::new(File::create(&tmp.0)?);
            input.rewind()?;
            let mut h2 = crc32fast::Hasher::new();
            let mut s2 = 0;
            copy_with_crc(&mut input, &mut f, &mut h2, &mut s2)?;
            f.flush()?;
            METHOD_STORE
        } else {
            METHOD_DEFLATE
        }
    };
    Ok(Compressed { index, method, crc: hasher.finalize(), size, data: Payload::Spilled(tmp) })
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

/// 把 Unix 秒按本地时区拆成年月日时分秒。不依赖时区库：用 libc::localtime_r 等价的纯算法会很长，
/// 这里用 `std` 没有的能力，所以借助 `date` 的通用算法：先取时区偏移（由 TZ 环境决定），再做公历换算。
fn local_components(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let offset = tz_offset_seconds(secs);
    let t = secs + offset;
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32)
}

/// 当前时区偏移（秒）。用 libc 的 localtime_r 最准确，但为避免新增依赖，这里读 `TZ`/系统设置的通用做法
/// 不可行，退而求其次：通过 `std::process` 之外的 C 调用也不行——因此直接用 `time` 系列的 FFI 声明。
fn tz_offset_seconds(secs: i64) -> i64 {
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: i64,
        tm_zone: *const u8,
    }
    extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let mut tm = std::mem::MaybeUninit::<Tm>::uninit();
    // SAFETY：localtime_r 是线程安全的 POSIX 函数，入参指针有效；macOS/Linux 的 struct tm 布局与上面一致
    unsafe {
        if localtime_r(&secs, tm.as_mut_ptr()).is_null() {
            return 0;
        }
        tm.assume_init().tm_gmtoff
    }
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

/// 条目的扩展字段：zip64（按需）+ UT 时间戳。
fn extra_fields(csize: u64, usize_: u64, offset: Option<u64>, mtime: Option<i64>) -> Vec<u8> {
    let mut extra = Vec::new();
    let need64 = csize > ZIP64_LIMIT || usize_ > ZIP64_LIMIT || offset.is_some_and(|o| o > ZIP64_LIMIT);
    if need64 {
        let mut body = Vec::new();
        body.extend_from_slice(&usize_.to_le_bytes());
        body.extend_from_slice(&csize.to_le_bytes());
        if let Some(o) = offset {
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
    extra
}

/// 写 local header，返回中央目录记录。`dos_attr` 目录为 0x10。
/// 参数就是 ZIP 头的字段，拆成结构体只会多一层壳，这里允许超过 7 个。
#[allow(clippy::too_many_arguments)]
fn write_local(out: &mut CountingWriter, name: Vec<u8>, method: u16, crc: u32, csize: u64, usize_: u64, item: &SourceItem, dos_attr: u32) -> Result<CentralRecord> {
    let local_offset = out.offset;
    let (dos_time, dos_date) = dos_datetime(item.mtime);
    let extra = extra_fields(csize, usize_, None, item.mtime);
    let need64 = csize > ZIP64_LIMIT || usize_ > ZIP64_LIMIT;
    out.write_u32(SIG_LOCAL)?;
    out.write_u16(if need64 { 45 } else { 20 })?;
    out.write_u16(FLAG_UTF8)?;
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
    Ok(CentralRecord {
        name,
        method,
        flags: FLAG_UTF8,
        dos_time,
        dos_date,
        crc,
        csize,
        usize_,
        mtime: item.mtime,
        external_attrs: (mode << 16) | dos_attr,
        local_offset,
    })
}

/// 写一条中央目录记录。
fn write_central(out: &mut CountingWriter, rec: &CentralRecord) -> Result<()> {
    let extra = extra_fields(rec.csize, rec.usize_, Some(rec.local_offset), rec.mtime);
    let need64 = rec.csize > ZIP64_LIMIT || rec.usize_ > ZIP64_LIMIT || rec.local_offset > ZIP64_LIMIT;
    out.write_u32(SIG_CENTRAL)?;
    out.write_u16(VERSION_MADE_BY)?;
    out.write_u16(if need64 { 45 } else { 20 })?;
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
