//! 修复扫描：中央目录缺失或损坏时，从头扫描本地文件头重建条目列表（供「修复」功能）。
//! 本地头没有外部属性，类型只能靠名字尾部的 `/` 判断；带 data descriptor 的条目要往后找描述符才知道长度。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::bytes;
use super::cd::{self, HeaderFields, RawEntry, SIG_CD, SIG_DESC, SIG_LOCAL};
use super::local;
use super::{split, ZipReader};
use crate::archive::{Entry, EntryId};
use crate::{DetailKind, Error, Result};

/// 扫描块大小。
const CHUNK: usize = 256 * 1024;

impl ZipReader {
    /// 不信任中央目录，直接扫描本地头重建条目。数据不完整（被截断）的条目会被丢弃。
    pub fn open_by_scan(path: &Path) -> Result<Self> {
        let mut source = split::probe_pkware(path)?;
        let len = source.len();
        let mut entries = Vec::new();
        let mut raws = Vec::new();
        let mut pos = 0u64;
        while let Some(p) = find_sig(&mut source.reader, pos, len, &SIG_LOCAL)? {
            // 这个签名可能只是数据里的巧合，或者条目被截断：解析失败就从它后面继续找
            match scan_one(&mut source.reader, p, len, entries.len()) {
                Ok(Some((entry, raw, next))) => {
                    entries.push(entry);
                    raws.push(raw);
                    pos = next.max(p + 4);
                }
                _ => pos = p + 4,
            }
        }
        if entries.is_empty() {
            return Err(Error::Corrupt(DetailKind::RepairNoEntries.into()));
        }
        Ok(Self::from_parts(source, entries, raws, Vec::new()))
    }
}

/// 读到 `buf` 满或流尽，返回读到的字节数。
fn read_up_to<R: Read>(src: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match src.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(n)
}

/// 从 `from` 起找签名，返回逻辑偏移。按块读，块间回退 3 字节防签名跨块。
fn find_sig<S: Read + Seek>(src: &mut S, from: u64, len: u64, sig: &[u8; 4]) -> Result<Option<u64>> {
    let mut buf = vec![0u8; CHUNK];
    let mut pos = from;
    while pos < len {
        src.seek(SeekFrom::Start(pos))?;
        let n = read_up_to(src, &mut buf)?;
        if n < sig.len() {
            break;
        }
        if let Some(i) = bytes::find(&buf[..n], sig, 0) {
            return Ok(Some(pos + i as u64));
        }
        if n < CHUNK {
            break;
        }
        pos += (n - (sig.len() - 1)) as u64;
    }
    Ok(None)
}

/// 解析 `p` 处的本地头并确定数据范围；返回条目与下一个扫描位置。数据越界返回 None。
fn scan_one<S: Read + Seek>(src: &mut S, p: u64, len: u64, idx: usize) -> Result<Option<(Entry, RawEntry, u64)>> {
    let (lh, data_start) = local::read_local_header(src, p, len)?;
    let mut name = vec![0u8; lh.name_len];
    let mut extra = vec![0u8; lh.extra_len];
    src.read_exact(&mut name)?;
    src.read_exact(&mut extra)?;
    let id = EntryId::try_from(idx).map_err(|_| Error::Corrupt(DetailKind::TooManyEntries.into()))?;
    let mut fields = HeaderFields {
        version_made_by: 0,
        flags: lh.flags,
        method: lh.method,
        dos_time: lh.dos_time,
        dos_date: lh.dos_date,
        crc: lh.crc,
        packed: lh.packed as u64,
        size: lh.size as u64,
        disk: 0,
        external: 0,
        offset: p,
    };

    // 带描述符且头里三个字段都是 0：长度只能靠描述符。头里有值（比如 Info-ZIP 事后回写过）就直接信它。
    let sizes_unknown = lh.has_descriptor() && lh.packed == 0 && lh.size == 0 && lh.crc == 0;
    if sizes_unknown {
        let Some(desc) = find_descriptor(src, data_start, len)? else {
            return Ok(None);
        };
        fields.crc = desc.crc;
        fields.packed = desc.packed;
        fields.size = desc.size;
        let (entry, raw) = cd::build_entry(id, fields, &name, &extra, &|_, o| Ok(o))?;
        return Ok(Some((entry, raw, desc.next)));
    }

    let (entry, raw) = cd::build_entry(id, fields, &name, &extra, &|_, o| Ok(o))?;
    let packed = entry.packed_size.unwrap_or(0);
    let Some(end) = data_start.checked_add(packed) else {
        return Ok(None);
    };
    if end > len {
        return Ok(None);
    }
    Ok(Some((entry, raw, end)))
}

/// 从描述符里读到的值。
struct Descriptor {
    crc: u32,
    packed: u64,
    size: u64,
    /// 描述符之后的位置
    next: u64,
}

/// 往后找 data descriptor。先找带签名的 `PK\x07\x08`（压缩长度要正好等于它到数据起点的距离，
/// 否则是数据里的巧合）；找不到就在下一个本地头/目录头前面按无签名布局试。
fn find_descriptor<S: Read + Seek>(src: &mut S, data_start: u64, len: u64) -> Result<Option<Descriptor>> {
    let mut from = data_start;
    while let Some(q) = find_sig(src, from, len, &SIG_DESC)? {
        let mut buf = [0u8; 24];
        src.seek(SeekFrom::Start(q))?;
        let n = read_up_to(src, &mut buf)?;
        let csize = q - data_start;
        // 32 位布局：签名 + crc + 压缩长度(4) + 原始长度(4)
        if n >= 16 && bytes::le_u32(&buf, 8).map(u64::from) == Some(csize) {
            return Ok(Some(Descriptor {
                crc: bytes::le_u32(&buf, 4).unwrap_or(0),
                packed: csize,
                size: bytes::le_u32(&buf, 12).unwrap_or(0) as u64,
                next: q + 16,
            }));
        }
        // zip64 布局：签名 + crc + 压缩长度(8) + 原始长度(8)
        if n >= 24 && bytes::le_u64(&buf, 8) == Some(csize) {
            return Ok(Some(Descriptor {
                crc: bytes::le_u32(&buf, 4).unwrap_or(0),
                packed: csize,
                size: bytes::le_u64(&buf, 16).unwrap_or(0),
                next: q + 24,
            }));
        }
        from = q + 4;
    }

    // 无签名描述符：紧贴在下一条记录前面
    let next_local = find_sig(src, data_start, len, &SIG_LOCAL)?;
    let next_cd = find_sig(src, data_start, len, &SIG_CD)?;
    let boundary = match (next_local, next_cd) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => len,
    };
    // (描述符长度, 压缩长度是否 8 字节)
    for (desc_len, wide) in [(12u64, false), (20u64, true)] {
        let Some(q) = boundary.checked_sub(desc_len) else { continue };
        if q < data_start {
            continue;
        }
        let mut buf = [0u8; 20];
        src.seek(SeekFrom::Start(q))?;
        let n = read_up_to(src, &mut buf)?;
        if n < desc_len as usize {
            continue;
        }
        let csize = q - data_start;
        let (stored, size) = if wide {
            (bytes::le_u64(&buf, 4), bytes::le_u64(&buf, 12).unwrap_or(0))
        } else {
            (bytes::le_u32(&buf, 4).map(u64::from), bytes::le_u32(&buf, 8).unwrap_or(0) as u64)
        };
        if stored == Some(csize) {
            return Ok(Some(Descriptor { crc: bytes::le_u32(&buf, 0).unwrap_or(0), packed: csize, size, next: boundary }));
        }
    }
    Ok(None)
}
