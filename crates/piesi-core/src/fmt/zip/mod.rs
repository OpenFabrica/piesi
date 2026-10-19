//! ZIP 读取器：自写容器解析（EOCD / zip64 / 中央目录 / 本地头、字节分卷与 PKWARE 多盘分卷、
//! ZipCrypto 与 WinZip AES），解压交给 `codec`，落盘交给 `EntrySink`。zip 条目彼此独立，可随机访问。

mod aes;
mod bytes;
mod cd;
mod extra;
mod local;
mod repair;
mod split;
mod time;
mod zipcrypto;

use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::archive::{ArchiveReader, Entry, EntryId, EntryKind, EntrySink, ExtractReport, Format, PasswordNeed};
use crate::codec;
use crate::progress::{OpContext, Throttle};
use crate::{Error, Result};

/// 符号链接目标长度上限，超过它的 symlink 条目一定不正常。
const MAX_SYMLINK_LEN: u64 = 64 * 1024;
/// 读包内原始数据的缓冲大小。
const RAW_BUF: usize = 64 * 1024;

/// ZIP 读取器。
pub struct ZipReader {
    source: split::Source,
    entries: Vec<Entry>,
    raws: Vec<cd::RawEntry>,
    comment: Vec<u8>,
    password: Option<String>,
}

impl ZipReader {
    /// 打开并解析中央目录。只读目录区，不碰条目数据。
    pub fn open(path: &Path) -> Result<Self> {
        let mut source = split::open_initial(path)?;
        let len = source.len();
        let mut eocd = cd::find_eocd(&mut source.reader, len)?;

        // PKWARE 多盘：EOCD 在最后一盘（就是传入的文件）里，读到它才知道盘数；
        // 按盘串联后再找一次 EOCD，拿到它在逻辑流里的位置
        let total_disks = eocd.total_disks();
        if total_disks > 1 && !source.is_multi_disk() {
            source = split::open_pkware(path, total_disks)?;
            let len = source.len();
            eocd = cd::find_eocd(&mut source.reader, len)?;
        }
        let zip64_pos = cd::apply_zip64(&mut source.reader, &mut eocd, |d, o| resolve(&source.disk_base, d, o))?;

        let len = source.len();
        let cd_size = eocd.cd_size;
        let mut cd_start = resolve(&source.disk_base, eocd.cd_disk, eocd.cd_offset)?;
        if cd_start.checked_add(cd_size).is_none_or(|end| end > len) {
            return Err(Error::Corrupt("中央目录越界".into()));
        }

        // 前缀修正（仅单盘）：自解压程序 stub 或别的垃圾前缀会让包里所有偏移整体偏小。
        // 真正的目录起点 = 结束记录位置 - 目录长度；它与声明值的差就是前缀长度，所有本地头偏移都要加上。
        let mut delta = 0u64;
        if !source.is_multi_disk() {
            let record_pos = zip64_pos.unwrap_or(eocd.pos);
            if let Some(expected) = record_pos.checked_sub(cd_size) {
                if expected > cd_start
                    && !has_sig(&mut source.reader, cd_start, &cd::SIG_CD)
                    && has_sig(&mut source.reader, expected, &cd::SIG_CD)
                {
                    delta = expected - cd_start;
                    cd_start = expected;
                }
            }
        }

        source.reader.seek(SeekFrom::Start(cd_start))?;
        let (entries, raws) = cd::parse_cd(&mut source.reader, cd_size, eocd.entries, |d, o| {
            resolve(&source.disk_base, d, o)?
                .checked_add(delta)
                .ok_or_else(|| Error::Corrupt("偏移溢出".into()))
        })?;
        if entries.is_empty() && eocd.entries != 0 {
            return Err(Error::Corrupt("中央目录位置不对，读不到任何条目".into()));
        }
        Ok(Self { source, entries, raws, comment: eocd.comment, password: None })
    }

    /// 由已解析好的部件组装（修复扫描用）。
    fn from_parts(source: split::Source, entries: Vec<Entry>, raws: Vec<cd::RawEntry>, comment: Vec<u8>) -> Self {
        Self { source, entries, raws, comment, password: None }
    }
}

/// （盘号, 盘内偏移）→ 逻辑偏移。
fn resolve(disk_base: &[u64], disk: u32, offset: u64) -> Result<u64> {
    let base = disk_base
        .get(disk as usize)
        .ok_or_else(|| Error::Corrupt(format!("引用了不存在的分卷 #{disk}")))?;
    base.checked_add(offset).ok_or_else(|| Error::Corrupt("偏移溢出".into()))
}

/// `pos` 处的 4 字节是否等于 `sig`；读不到就当不是。
fn has_sig<S: Read + Seek>(src: &mut S, pos: u64, sig: &[u8; 4]) -> bool {
    let mut b = [0u8; 4];
    src.seek(SeekFrom::Start(pos)).is_ok() && src.read_exact(&mut b).is_ok() && &b == sig
}

/// 给进度与错误信息用的名字：优先 0x7075 里的 UTF-8 名，否则按 UTF-8 宽松解码。
fn display_name(entry: &Entry) -> String {
    entry
        .unicode_name
        .clone()
        .unwrap_or_else(|| String::from_utf8_lossy(&entry.raw_name).into_owned())
}

/// 解密层：按条目加密方式三选一。AES 的状态（密钥、HMAC、计数器）比另外两个大得多，装箱免得枚举跟着变胖。
enum Decrypt<R: Read> {
    Plain(R),
    ZipCrypto(zipcrypto::ZipCryptoReader<R>),
    Aes(Box<aes::AesReader<R>>),
}

impl<R: Read> Read for Decrypt<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Decrypt::Plain(r) => r.read(buf),
            Decrypt::ZipCrypto(r) => r.read(buf),
            Decrypt::Aes(r) => r.read(buf),
        }
    }
}

impl<R: Read> Decrypt<R> {
    /// 收尾：AES 要读完密文并验认证码，其它无事。
    fn finish(&mut self) -> Result<()> {
        match self {
            Decrypt::Aes(r) => r.finish(),
            _ => Ok(()),
        }
    }
}

/// 交给 sink 的条目明文流：解压输出 → 限长 → CRC → 进度/取消检查点。
/// 下游出错时把归类后的错误记在 `failure`，不管 sink 怎么包装 io 错误，驱动都能报出真正原因。
struct EntryReader<'r, 't> {
    inner: Box<dyn Read + 'r>,
    remaining: u64,
    hasher: crc32fast::Hasher,
    throttle: &'r mut Throttle<'t>,
    failure: Option<Error>,
    hit_eof: bool,
}

/// 流水线跑完后的结算信息。
struct Outcome {
    failure: Option<Error>,
    remaining: u64,
    hit_eof: bool,
    crc: u32,
}

impl EntryReader<'_, '_> {
    /// 消耗自身（释放对解密层的借用），交出结算信息。
    fn into_outcome(self) -> Outcome {
        Outcome { failure: self.failure, remaining: self.remaining, hit_eof: self.hit_eof, crc: self.hasher.finalize() }
    }
}

impl Read for EntryReader<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other("条目流已失败"));
        }
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.remaining) as usize;
        match self.inner.read(&mut buf[..want]) {
            Ok(0) => {
                self.hit_eof = true;
                Ok(0)
            }
            Ok(n) => {
                self.hasher.update(&buf[..n]);
                self.remaining -= n as u64;
                if let Err(e) = self.throttle.add_bytes(n as u64) {
                    self.failure = Some(e);
                    return Err(io::Error::other("已取消"));
                }
                Ok(n)
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Err(e),
            Err(e) => {
                let err = classify_io(e);
                let msg = err.to_string();
                self.failure = Some(err);
                Err(io::Error::other(msg))
            }
        }
    }
}

/// 把解压/解密层抛出的 io 错误归类：数据类错误算「压缩包损坏」，其余是真 I/O 错误。
fn classify_io(e: io::Error) -> Error {
    use io::ErrorKind::*;
    match e.kind() {
        InvalidData | InvalidInput | UnexpectedEof | Other | OutOfMemory => Error::Corrupt(format!("解压失败：{e}")),
        _ => Error::Io(e),
    }
}

/// 搭一条条目流水线交给 `f`：本地头 → take(packed) → 解密 → 解压 → `EntryReader`。
/// `f` 返回后收尾：记录的失败优先；明文不够长算损坏；CRC 不符算损坏；AES 还要验认证码。
fn process_entry<T>(
    source: &mut split::Source,
    entry: &Entry,
    raw: &cd::RawEntry,
    password: Option<&str>,
    th: &mut Throttle<'_>,
    f: impl FnOnce(&mut EntryReader<'_, '_>) -> Result<T>,
) -> Result<T> {
    let name = display_name(entry);
    let len = source.len();
    let (lh, data_start) = local::read_local_header(&mut source.reader, raw.local_offset, len)?;
    let packed = entry.packed_size.unwrap_or(0);
    if data_start.checked_add(packed).is_none_or(|end| end > len) {
        return Err(Error::Corrupt(format!("条目数据越界：{name}")));
    }
    source.reader.seek(SeekFrom::Start(data_start))?;
    let raw_reader = BufReader::with_capacity(RAW_BUF, (&mut source.reader).take(packed));

    let mut dec = if entry.encrypted {
        let pw = password.ok_or(Error::PasswordRequired)?.as_bytes();
        if raw.flags & 0x0040 != 0 {
            return Err(Error::Unsupported("PKWARE 强加密".into()));
        }
        if raw.method_raw == 99 {
            let info = raw.aes.as_ref().ok_or_else(|| Error::Corrupt("AES 条目缺少 0x9901 扩展字段".into()))?;
            Decrypt::Aes(Box::new(aes::AesReader::new(raw_reader, pw, info, packed)?))
        } else {
            // 校验字节：正常是 CRC 高字节；有 data descriptor 时写入方还不知道 CRC，用的是 DOS 时间高字节
            let check = if lh.has_descriptor() {
                (lh.dos_time >> 8) as u8
            } else {
                (entry.crc32.unwrap_or(lh.crc) >> 24) as u8
            };
            Decrypt::ZipCrypto(zipcrypto::ZipCryptoReader::new(raw_reader, pw, check)?)
        }
    } else {
        Decrypt::Plain(raw_reader)
    };

    let decoder = codec::decoder(&entry.method, Box::new(&mut dec), &[], entry.size)?;
    let mut er = EntryReader {
        inner: decoder,
        remaining: entry.size,
        hasher: crc32fast::Hasher::new(),
        throttle: th,
        failure: None,
        hit_eof: false,
    };
    let result = f(&mut er);
    let outcome = er.into_outcome();
    if let Some(e) = outcome.failure {
        return Err(e);
    }
    let value = result?;
    if outcome.remaining > 0 {
        if outcome.hit_eof {
            return Err(Error::Corrupt(format!("数据提前结束：{name}")));
        }
        // 接收端没读完就返回了（它自己决定跳过），没法校验 CRC；AES 认证码照验
    } else if let Some(expected) = entry.crc32 {
        if outcome.crc != expected {
            return Err(Error::Corrupt(format!("CRC 不匹配：{name}（期望 {expected:08x}，实际 {:08x}）", outcome.crc)));
        }
    }
    dec.finish()?;
    Ok(value)
}

impl ArchiveReader for ZipReader {
    fn format(&self) -> Format {
        Format::Zip
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn needs_password(&self) -> PasswordNeed {
        if self.entries.iter().any(|e| e.encrypted) {
            PasswordNeed::ForData
        } else {
            PasswordNeed::None
        }
    }

    fn set_password(&mut self, password: Option<&str>) -> Result<()> {
        self.password = password.map(str::to_owned);
        Ok(())
    }

    fn extract(&mut self, ids: Option<&[EntryId]>, sink: &mut dyn EntrySink, ctx: &OpContext<'_>) -> Result<ExtractReport> {
        let ids: Vec<EntryId> = match ids {
            Some(s) => s.to_vec(),
            None => (0..self.entries.len() as EntryId).collect(),
        };
        for &id in &ids {
            if id as usize >= self.entries.len() {
                return Err(Error::NoSuchEntry(id));
            }
        }
        let total: u64 = ids.iter().map(|&id| self.entries[id as usize].size).fold(0, u64::saturating_add);
        let mut th = Throttle::new(ctx, Some(total), Some(ids.len() as u64));
        let mut report = ExtractReport::default();
        let password = self.password.as_deref();
        for id in ids {
            let entry = &self.entries[id as usize];
            let raw = &self.raws[id as usize];
            th.begin_entry(&display_name(entry))?;
            match entry.kind {
                EntryKind::Dir => sink.dir(entry)?,
                EntryKind::Symlink => {
                    if entry.size > MAX_SYMLINK_LEN {
                        return Err(Error::Corrupt(format!("符号链接目标过长：{}", display_name(entry))));
                    }
                    let target = process_entry(&mut self.source, entry, raw, password, &mut th, |er| {
                        let mut v = Vec::with_capacity(entry.size as usize);
                        er.read_to_end(&mut v)?;
                        Ok(v)
                    })?;
                    sink.symlink(entry, &target)?;
                }
                EntryKind::File => {
                    process_entry(&mut self.source, entry, raw, password, &mut th, |er| sink.file(entry, er))?;
                }
            }
            report.extracted += 1;
        }
        th.finish();
        Ok(report)
    }

    fn read_entry(&mut self, id: EntryId, out: &mut dyn io::Write, ctx: &OpContext<'_>) -> Result<()> {
        let entry = self.entries.get(id as usize).ok_or(Error::NoSuchEntry(id))?;
        let raw = &self.raws[id as usize];
        let mut th = Throttle::new(ctx, Some(entry.size), Some(1));
        th.begin_entry(&display_name(entry))?;
        if entry.kind != EntryKind::Dir {
            process_entry(&mut self.source, entry, raw, self.password.as_deref(), &mut th, |er| {
                io::copy(er, out)?;
                Ok(())
            })?;
        }
        th.finish();
        Ok(())
    }

    fn comment(&self) -> Option<&[u8]> {
        if self.comment.is_empty() {
            None
        } else {
            Some(&self.comment)
        }
    }

    fn hints(&self) -> crate::encoding::Hints {
        // 过半条目的 version made by 是 MS-DOS 就当 Windows 工具产物；修复扫描没有目录头，host 全 0 也落在这条
        let dos = self.raws.iter().filter(|r| r.host == 0).count();
        crate::encoding::Hints { made_by_dos: !self.raws.is_empty() && dos * 2 > self.raws.len(), ..Default::default() }
    }
}
