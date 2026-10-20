//! 中央目录解析：从尾部找 EOCD → zip64 定位记录与 zip64 EOCD → 逐条读目录头，
//! 产出统一的 `Entry` 与驱动私有的 `RawEntry`。所有定长字段经 `Cursor` 读，越界即 `Corrupt`。

use std::io::{BufReader, Read, Seek, SeekFrom};

use super::bytes::{self, Cursor};
use super::extra::{self, AesInfo, Zip64Need};
use super::time;
use crate::archive::{Entry, EntryId, EntryKind, Method};
use crate::{DetailKind, Error, Result};

/// 本地文件头签名。
pub const SIG_LOCAL: [u8; 4] = *b"PK\x03\x04";
/// 中央目录头签名。
pub const SIG_CD: [u8; 4] = *b"PK\x01\x02";
/// 中央目录结束记录签名。
pub const SIG_EOCD: [u8; 4] = *b"PK\x05\x06";
/// zip64 中央目录结束记录签名。
pub const SIG_ZIP64_EOCD: [u8; 4] = *b"PK\x06\x06";
/// zip64 定位记录签名。
pub const SIG_ZIP64_LOC: [u8; 4] = *b"PK\x06\x07";
/// data descriptor 签名（可选）。
pub const SIG_DESC: [u8; 4] = *b"PK\x07\x08";

/// EOCD 定长部分长度。
const EOCD_LEN: usize = 22;
/// EOCD 最远可能离文件尾多少字节：定长 22 + 最长 65535 字节注释。
const EOCD_SEARCH: u64 = EOCD_LEN as u64 + 65_535;
/// zip64 定位记录长度。
const LOC_LEN: usize = 20;
/// zip64 EOCD 记录定长部分长度。
const ZIP64_EOCD_LEN: usize = 56;
/// 中央目录头定长部分长度。
const CD_HEADER_LEN: usize = 46;

/// 条目在 `Entry` 之外还需要的驱动私有信息。
#[derive(Clone, Debug)]
pub struct RawEntry {
    /// 本地文件头在逻辑流里的偏移（已含分卷基址与前缀修正）
    pub local_offset: u64,
    pub flags: u16,
    /// 头里原始的方法号（99 表示 AES 包装）
    pub method_raw: u16,
    pub aes: Option<AesInfo>,
    /// version made by 的高字节（0 = MS-DOS，3 = Unix，19 = OS X），给编码识别当先验
    pub host: u8,
}

/// zip64 定位记录。
#[derive(Clone, Copy, Debug)]
pub struct Locator {
    pub disk: u32,
    pub offset: u64,
    pub total_disks: u32,
}

/// 中央目录结束记录（已尽量用 zip64 记录补齐）。
#[derive(Clone, Debug)]
pub struct Eocd {
    pub this_disk: u32,
    pub cd_disk: u32,
    pub entries: u64,
    pub cd_size: u64,
    pub cd_offset: u64,
    pub comment: Vec<u8>,
    /// EOCD 记录在逻辑流里的位置
    pub pos: u64,
    pub locator: Option<Locator>,
}

impl Eocd {
    /// 总盘数：zip64 定位记录有就用它，否则「本盘号 + 1」。
    pub fn total_disks(&self) -> u32 {
        self.locator
            .map(|l| l.total_disks)
            .filter(|&n| n > 0)
            .unwrap_or_else(|| self.this_disk.saturating_add(1))
    }

    /// 是否有字段饱和（需要 zip64 记录补充）。
    fn saturated(&self) -> bool {
        self.this_disk == 0xFFFF
            || self.cd_disk == 0xFFFF
            || self.entries == 0xFFFF
            || self.cd_size == 0xFFFF_FFFF
            || self.cd_offset == 0xFFFF_FFFF
    }
}

/// 读取尾部并定位 EOCD。
/// 为什么从后往前找且优先「注释长度正好吃到文件尾」的候选：注释本身可能含有签名字节，
/// 只有长度自洽的那条才是真的；都不自洽时退而取最靠后的，兼容注释长度写错的老工具。
pub fn find_eocd<S: Read + Seek>(src: &mut S, len: u64) -> Result<Eocd> {
    if len < EOCD_LEN as u64 {
        return Err(Error::Corrupt(DetailKind::ZipTooShort.into()));
    }
    let tail_len = len.min(EOCD_SEARCH) as usize;
    let tail_start = len - tail_len as u64;
    let mut tail = vec![0u8; tail_len];
    src.seek(SeekFrom::Start(tail_start))?;
    src.read_exact(&mut tail).map_err(|_| Error::Corrupt(DetailKind::ZipTailReadFailed.into()))?;

    let mut chosen = None;
    let mut fallback = None;
    let mut end = tail_len - EOCD_LEN;
    while let Some(i) = bytes::rfind(&tail, &SIG_EOCD, end) {
        let clen = bytes::le_u16(&tail, i + 20).unwrap_or(0) as usize;
        if i + EOCD_LEN + clen == tail_len {
            chosen = Some(i);
            break;
        }
        if fallback.is_none() {
            fallback = Some(i);
        }
        if i == 0 {
            break;
        }
        end = i - 1;
    }
    let Some(i) = chosen.or(fallback) else {
        return Err(Error::Corrupt(DetailKind::EocdNotFound.into()));
    };

    let mut c = Cursor::new(&tail[i..]);
    c.skip(4)?;
    let this_disk = c.u16()? as u32;
    let cd_disk = c.u16()? as u32;
    let _entries_this_disk = c.u16()?;
    let entries = c.u16()? as u64;
    let cd_size = c.u32()? as u64;
    let cd_offset = c.u32()? as u64;
    let clen = c.u16()? as usize;
    // 注释长度写多了就取到文件尾为止
    let comment = c.take(clen.min(c.remaining()))?.to_vec();

    // zip64 定位记录紧贴在 EOCD 前面
    let locator = if i >= LOC_LEN && tail[i - LOC_LEN..i - LOC_LEN + 4] == SIG_ZIP64_LOC {
        let mut c = Cursor::new(&tail[i - LOC_LEN..i]);
        c.skip(4)?;
        Some(Locator { disk: c.u32()?, offset: c.u64()?, total_disks: c.u32()? })
    } else {
        None
    };

    Ok(Eocd { this_disk, cd_disk, entries, cd_size, cd_offset, comment, pos: tail_start + i as u64, locator })
}

/// 读 zip64 EOCD 记录，补齐 EOCD 里饱和的字段；返回记录的逻辑位置（前缀修正要用）。
/// `resolve` 把（盘号, 盘内偏移）换算成逻辑偏移。
pub fn apply_zip64<S: Read + Seek>(
    src: &mut S,
    eocd: &mut Eocd,
    resolve: impl Fn(u32, u64) -> Result<u64>,
) -> Result<Option<u64>> {
    let Some(loc) = eocd.locator else {
        if eocd.saturated() {
            return Err(Error::Corrupt(DetailKind::Zip64LocatorMissing.into()));
        }
        return Ok(None);
    };
    // 定位记录可能是误判（前一条目录头的注释恰好像签名）：它指向的地方读不到 zip64 记录时，
    // 只要 EOCD 自身没饱和就当没有这回事
    let mut buf = [0u8; ZIP64_EOCD_LEN];
    let pos = resolve(loc.disk, loc.offset).ok();
    let ok = pos.is_some_and(|p| {
        src.seek(SeekFrom::Start(p)).is_ok() && src.read_exact(&mut buf).is_ok() && buf[..4] == SIG_ZIP64_EOCD
    });
    if !ok {
        if eocd.saturated() {
            return Err(Error::Corrupt(DetailKind::Zip64EocdBad.into()));
        }
        return Ok(None);
    }
    let pos = pos.unwrap_or(0);
    let mut c = Cursor::new(&buf[4..]);
    let _record_size = c.u64()?;
    let _version_made_by = c.u16()?;
    let _version_needed = c.u16()?;
    let this_disk = c.u32()?;
    let cd_disk = c.u32()?;
    let _entries_this_disk = c.u64()?;
    let entries = c.u64()?;
    let cd_size = c.u64()?;
    let cd_offset = c.u64()?;
    if eocd.this_disk == 0xFFFF {
        eocd.this_disk = this_disk;
    }
    if eocd.cd_disk == 0xFFFF {
        eocd.cd_disk = cd_disk;
    }
    if eocd.entries == 0xFFFF {
        eocd.entries = entries;
    }
    if eocd.cd_size == 0xFFFF_FFFF {
        eocd.cd_size = cd_size;
    }
    if eocd.cd_offset == 0xFFFF_FFFF {
        eocd.cd_offset = cd_offset;
    }
    Ok(Some(pos))
}

/// 目录头与本地头共有的定长字段，`build_entry` 的输入。
#[derive(Clone, Copy, Debug)]
pub struct HeaderFields {
    pub version_made_by: u16,
    pub flags: u16,
    pub method: u16,
    pub dos_time: u16,
    pub dos_date: u16,
    pub crc: u32,
    pub packed: u64,
    pub size: u64,
    pub disk: u32,
    pub external: u32,
    pub offset: u64,
}

/// 逐条读中央目录。`reader` 已定位到目录起点；读到 `cd_size` 结束或遇到非目录头签名（数字签名记录、EOCD）即停。
pub fn parse_cd<R: Read>(
    reader: R,
    cd_size: u64,
    entries_hint: u64,
    resolve: impl Fn(u32, u64) -> Result<u64>,
) -> Result<(Vec<Entry>, Vec<RawEntry>)> {
    let mut r = BufReader::with_capacity(64 << 10, reader.take(cd_size));
    // 容量只按提示预留一小部分：条目数字段可以随便写，不能让它决定分配
    let cap = entries_hint.min(1 << 16) as usize;
    let mut entries = Vec::with_capacity(cap);
    let mut raws = Vec::with_capacity(cap);
    let mut fixed = [0u8; CD_HEADER_LEN];
    let mut name = Vec::new();
    let mut extra = Vec::new();
    let mut comment = Vec::new();
    loop {
        match r.read_exact(&mut fixed[..4]) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if fixed[..4] != SIG_CD {
            break;
        }
        r.read_exact(&mut fixed[4..]).map_err(|_| Error::Corrupt(DetailKind::CdHeaderIncomplete.into()))?;
        let mut c = Cursor::new(&fixed[4..]);
        let version_made_by = c.u16()?;
        let _version_needed = c.u16()?;
        let flags = c.u16()?;
        let method = c.u16()?;
        let dos_time = c.u16()?;
        let dos_date = c.u16()?;
        let crc = c.u32()?;
        let packed = c.u32()? as u64;
        let size = c.u32()? as u64;
        let name_len = c.u16()? as usize;
        let extra_len = c.u16()? as usize;
        let comment_len = c.u16()? as usize;
        let disk = c.u16()? as u32;
        let _internal = c.u16()?;
        let external = c.u32()?;
        let offset = c.u32()? as u64;

        name.resize(name_len, 0);
        extra.resize(extra_len, 0);
        comment.resize(comment_len, 0);
        r.read_exact(&mut name).map_err(|_| Error::Corrupt(DetailKind::CdNameIncomplete.into()))?;
        r.read_exact(&mut extra).map_err(|_| Error::Corrupt(DetailKind::CdExtraIncomplete.into()))?;
        r.read_exact(&mut comment).map_err(|_| Error::Corrupt(DetailKind::CdCommentIncomplete.into()))?;

        let id = EntryId::try_from(entries.len()).map_err(|_| Error::Corrupt(DetailKind::TooManyEntries.into()))?;
        let fields = HeaderFields { version_made_by, flags, method, dos_time, dos_date, crc, packed, size, disk, external, offset };
        let (entry, raw) = build_entry(id, fields, &name, &extra, &resolve)?;
        entries.push(entry);
        raws.push(raw);
    }
    Ok((entries, raws))
}

/// 把一条头（目录头或本地头）变成 `Entry` + `RawEntry`：解 zip64、扩展字段、类型、时间、方法。
pub fn build_entry(
    id: EntryId,
    f: HeaderFields,
    name: &[u8],
    extra: &[u8],
    resolve: &dyn Fn(u32, u64) -> Result<u64>,
) -> Result<(Entry, RawEntry)> {
    let need = Zip64Need {
        size: f.size == 0xFFFF_FFFF,
        packed: f.packed == 0xFFFF_FFFF,
        offset: f.offset == 0xFFFF_FFFF,
        disk: f.disk == 0xFFFF,
    };
    let parsed = extra::parse_extra(extra, name, need);
    let missing = |kind: DetailKind| Error::Corrupt(kind.into());
    let size = if need.size { parsed.zip64.size.ok_or_else(|| missing(DetailKind::Zip64MissingSize))? } else { f.size };
    let packed = if need.packed { parsed.zip64.packed.ok_or_else(|| missing(DetailKind::Zip64MissingPacked))? } else { f.packed };
    let offset = if need.offset { parsed.zip64.offset.ok_or_else(|| missing(DetailKind::Zip64MissingOffset))? } else { f.offset };
    let disk = if need.disk { parsed.zip64.disk.ok_or_else(|| missing(DetailKind::Zip64MissingDisk))? } else { f.disk };
    let local_offset = resolve(disk, offset)?;

    let raw_name: Vec<u8> = name.iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect();
    let host = (f.version_made_by >> 8) as u8;
    let unix_mode = if matches!(host, 3 | 19) { Some(f.external >> 16).filter(|&m| m != 0) } else { None };
    let kind = classify(&raw_name, f.external, unix_mode, size);
    // 0x9901 只对方法 99 有意义，别的方法带着它也不理
    let aes = if f.method == 99 { parsed.aes } else { None };
    let method = method_from_raw(f.method, aes.as_ref());
    // AE-2 的 CRC 字段恒为 0，不能拿来校验
    let crc32 = match aes {
        Some(a) if a.vendor_version == 2 => None,
        _ => Some(f.crc),
    };
    let mtime = parsed
        .mtime_ut
        .or(parsed.mtime_ntfs)
        .or_else(|| time::dos_to_unix(f.dos_time, f.dos_date));

    let mut entry = Entry::new(id, raw_name, kind, size);
    entry.name_is_utf8 = f.flags & 0x0800 != 0;
    entry.unicode_name = parsed.unicode_name;
    entry.packed_size = Some(packed);
    entry.mtime = mtime;
    entry.crc32 = crc32;
    entry.encrypted = f.flags & 0x0001 != 0;
    // 加密但既不是 AES（方法 99）也不是 PKWARE 强加密（bit 6）→ 老式 ZipCrypto，已知可破解
    entry.weak_encryption = entry.encrypted && f.method != 99 && f.flags & 0x0040 == 0;
    entry.method = method;
    entry.unix_mode = unix_mode;
    Ok((entry, RawEntry { local_offset, flags: f.flags, method_raw: f.method, aes, host }))
}

/// 判定条目类型：Unix 模式位最可信，其次名字尾部的 `/`，最后是 DOS 目录属性。
/// DOS 属性只在没有内容时采信，免得把带数据的文件错当目录。
fn classify(raw_name: &[u8], external: u32, unix_mode: Option<u32>, size: u64) -> EntryKind {
    if let Some(m) = unix_mode {
        match m & 0xF000 {
            0xA000 => return EntryKind::Symlink,
            0x4000 => return EntryKind::Dir,
            _ => {}
        }
    }
    if raw_name.last() == Some(&b'/') {
        return EntryKind::Dir;
    }
    if external & 0x10 != 0 && size == 0 {
        return EntryKind::Dir;
    }
    EntryKind::File
}

/// 方法号 → `Method`；99 是 AES 包装，真实方法在 0x9901 字段里。
pub fn method_from_raw(raw: u16, aes: Option<&AesInfo>) -> Method {
    let m = match (raw, aes) {
        (99, Some(a)) => a.method,
        (99, None) => return Method::Other("AES（缺少参数）".into()),
        (m, _) => m,
    };
    match m {
        0 => Method::Store,
        8 => Method::Deflate,
        9 => Method::Deflate64,
        12 => Method::Bzip2,
        14 => Method::Lzma,
        20 | 93 => Method::Zstd,
        95 => Method::Xz,
        98 => Method::Ppmd,
        1 => Method::Other("Shrink".into()),
        2..=5 => Method::Other("Reduce".into()),
        6 => Method::Other("Implode".into()),
        10 => Method::Other("DCL Implode".into()),
        18 => Method::Other("IBM TERSE".into()),
        19 => Method::Other("IBM LZ77".into()),
        96 => Method::Other("JPEG".into()),
        97 => Method::Other("WavPack".into()),
        other => Method::Other(format!("方法 {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor as IoCursor;

    /// 只有 EOCD 的空包能解析；注释也能取到。
    #[test]
    fn eocd_with_comment() {
        let mut data = SIG_EOCD.to_vec();
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&5u16.to_le_bytes());
        data.extend_from_slice(b"hello");
        let len = data.len() as u64;
        let e = find_eocd(&mut IoCursor::new(data), len).unwrap();
        assert_eq!(e.comment, b"hello");
        assert_eq!(e.entries, 0);
        assert_eq!(e.pos, 0);
    }

    /// 随机字节与过短输入都只报 Corrupt。
    #[test]
    fn garbage_is_corrupt() {
        let data = vec![0x50u8; 100];
        let r = find_eocd(&mut IoCursor::new(data.clone()), 100);
        assert!(matches!(r, Err(Error::Corrupt(_))));
        let r = find_eocd(&mut IoCursor::new(vec![1, 2, 3]), 3);
        assert!(matches!(r, Err(Error::Corrupt(_))));
    }

    /// 类型判定：Unix 模式 > 名字尾斜杠 > DOS 属性。
    #[test]
    fn classify_kinds() {
        assert_eq!(classify(b"a", 0, Some(0o120_777), 5), EntryKind::Symlink);
        assert_eq!(classify(b"a", 0, Some(0o040_755), 0), EntryKind::Dir);
        assert_eq!(classify(b"a/", 0, None, 0), EntryKind::Dir);
        assert_eq!(classify(b"a", 0x10, None, 0), EntryKind::Dir);
        assert_eq!(classify(b"a", 0x10, None, 3), EntryKind::File);
        assert_eq!(classify(b"a", 0x20, None, 0), EntryKind::File);
    }
}
