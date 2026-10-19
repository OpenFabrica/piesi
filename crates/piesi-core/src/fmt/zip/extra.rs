//! ZIP 扩展字段解析：zip64（0x0001）、Info-ZIP Unicode 路径（0x7075）、
//! Info-ZIP 时间戳（0x5455）、NTFS 时间（0x000a）、WinZip AES（0x9901）。
//! 扩展字段是「尽力解析」区：格式不对就忽略那一块，不让一个坏字段毁掉整条目录。

use super::bytes::Cursor;
use super::time;

/// zip64 字段的读取需求：只有定长字段饱和（0xFFFF/0xFFFFFFFF）时对应项才存在于 zip64 块里。
#[derive(Clone, Copy, Debug, Default)]
pub struct Zip64Need {
    pub size: bool,
    pub packed: bool,
    pub offset: bool,
    pub disk: bool,
}

/// zip64 块里实际读到的值。
#[derive(Clone, Copy, Debug, Default)]
pub struct Zip64Fields {
    pub size: Option<u64>,
    pub packed: Option<u64>,
    pub offset: Option<u64>,
    pub disk: Option<u32>,
}

/// WinZip AES 扩展字段内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AesInfo {
    /// 1 = AE-1（CRC 有效），2 = AE-2（CRC 字段为 0，不校验）
    pub vendor_version: u16,
    /// 1 = AES-128，2 = AES-192，3 = AES-256
    pub strength: u8,
    /// 真实压缩方法
    pub method: u16,
}

impl AesInfo {
    /// 盐长度：强度 1/2/3 对应 8/12/16 字节。
    pub fn salt_len(&self) -> usize {
        4 + 4 * self.strength as usize
    }

    /// 密钥长度：强度 1/2/3 对应 16/24/32 字节。
    pub fn key_len(&self) -> usize {
        8 + 8 * self.strength as usize
    }
}

/// 一次解析的全部结果。
#[derive(Clone, Debug, Default)]
pub struct ParsedExtra {
    pub zip64: Zip64Fields,
    pub unicode_name: Option<String>,
    /// 0x5455 里的修改时间（Unix 秒）
    pub mtime_ut: Option<i64>,
    /// 0x000a 里的修改时间（Unix 秒）
    pub mtime_ntfs: Option<i64>,
    pub aes: Option<AesInfo>,
}

/// 解析扩展字段区。`raw_name` 是头里原样的名字字节（用来校验 0x7075 的 CRC）。
pub fn parse_extra(extra: &[u8], raw_name: &[u8], need: Zip64Need) -> ParsedExtra {
    let mut out = ParsedExtra::default();
    let mut cur = Cursor::new(extra);
    // 每块 = id(2) + len(2) + data；长度越界就停，后面的内容已不可信
    while cur.remaining() >= 4 {
        let id = cur.u16().unwrap_or(0);
        let len = cur.u16().unwrap_or(0) as usize;
        let Ok(data) = cur.take(len) else { break };
        match id {
            0x0001 => out.zip64 = parse_zip64(data, need),
            0x7075 => out.unicode_name = parse_unicode_path(data, raw_name),
            0x5455 => out.mtime_ut = parse_ut(data),
            0x000a => out.mtime_ntfs = parse_ntfs(data),
            0x9901 => out.aes = parse_aes(data),
            _ => {}
        }
    }
    out
}

/// zip64 块：按需求顺序读 size/packed/offset/disk。
/// 为什么还要看总长度：有的写入器不管定长字段饱和与否都写全三个 u64，
/// 若只按需求顺序读就会把 size 当成 offset；长度正好是全量布局时按全量解析。
fn parse_zip64(data: &[u8], need: Zip64Need) -> Zip64Fields {
    let mut f = Zip64Fields::default();
    let needed_len = 8 * (need.size as usize + need.packed as usize + need.offset as usize) + 4 * need.disk as usize;
    let full_layout = needed_len < data.len() && (data.len() == 24 || data.len() == 28);
    let mut cur = Cursor::new(data);
    if full_layout {
        let size = cur.u64().ok();
        let packed = cur.u64().ok();
        let offset = cur.u64().ok();
        let disk = cur.u32().ok();
        f.size = if need.size { size } else { None };
        f.packed = if need.packed { packed } else { None };
        f.offset = if need.offset { offset } else { None };
        f.disk = if need.disk { disk } else { None };
        return f;
    }
    if need.size {
        f.size = cur.u64().ok();
    }
    if need.packed {
        f.packed = cur.u64().ok();
    }
    if need.offset {
        f.offset = cur.u64().ok();
    }
    if need.disk {
        f.disk = cur.u32().ok();
    }
    f
}

/// Info-ZIP Unicode 路径：版本 1 + 原名 CRC32 + UTF-8 名；CRC 对不上说明名字改过，字段作废。
fn parse_unicode_path(data: &[u8], raw_name: &[u8]) -> Option<String> {
    let mut cur = Cursor::new(data);
    if cur.u8().ok()? != 1 {
        return None;
    }
    let crc = cur.u32().ok()?;
    if crc != crc32fast::hash(raw_name) {
        return None;
    }
    let name = cur.take(cur.remaining()).ok()?;
    String::from_utf8(name.to_vec()).ok().filter(|s| !s.is_empty())
}

/// Info-ZIP 扩展时间戳：标志位 bit0 表示有 mtime，紧跟 4 字节有符号 Unix 秒。
fn parse_ut(data: &[u8]) -> Option<i64> {
    let mut cur = Cursor::new(data);
    let flags = cur.u8().ok()?;
    if flags & 1 == 0 {
        return None;
    }
    cur.u32().ok().map(|v| v as i32 as i64)
}

/// NTFS 时间：4 字节保留 + 若干 (tag, size, data) 属性，tag 1 是三个 FILETIME，取第一个（mtime）。
fn parse_ntfs(data: &[u8]) -> Option<i64> {
    let mut cur = Cursor::new(data);
    cur.skip(4).ok()?;
    while cur.remaining() >= 4 {
        let tag = cur.u16().ok()?;
        let size = cur.u16().ok()? as usize;
        let body = cur.take(size).ok()?;
        if tag == 1 && body.len() >= 8 {
            let mut a = [0u8; 8];
            a.copy_from_slice(&body[..8]);
            return time::filetime_to_unix(u64::from_le_bytes(a));
        }
    }
    None
}

/// WinZip AES：版本(2) + 厂商 "AE"(2) + 强度(1) + 真实方法(2)。
fn parse_aes(data: &[u8]) -> Option<AesInfo> {
    let mut cur = Cursor::new(data);
    let vendor_version = cur.u16().ok()?;
    let vendor = cur.take(2).ok()?;
    let strength = cur.u8().ok()?;
    let method = cur.u16().ok()?;
    if vendor != b"AE" || !(1..=3).contains(&strength) || !(1..=2).contains(&vendor_version) {
        return None;
    }
    Some(AesInfo { vendor_version, strength, method })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0x7075 的 CRC 要按原始名字算，对不上就不用。
    #[test]
    fn unicode_path_requires_crc_match() {
        let raw = b"\xd6\xd0\xce\xc4.txt"; // GBK「中文.txt」
        let crc = crc32fast::hash(raw);
        let mut data = vec![1u8];
        data.extend_from_slice(&crc.to_le_bytes());
        data.extend_from_slice("中文.txt".as_bytes());
        let mut extra = vec![0x75, 0x70];
        extra.extend_from_slice(&(data.len() as u16).to_le_bytes());
        extra.extend_from_slice(&data);
        let p = parse_extra(&extra, raw, Zip64Need::default());
        assert_eq!(p.unicode_name.as_deref(), Some("中文.txt"));
        let p = parse_extra(&extra, b"other", Zip64Need::default());
        assert_eq!(p.unicode_name, None);
    }

    /// zip64 块按需求顺序读；长度不够时留空而不是报错。
    #[test]
    fn zip64_positional_and_full_layout() {
        let mut extra = vec![0x01, 0x00, 8, 0];
        extra.extend_from_slice(&7u64.to_le_bytes());
        let need = Zip64Need { offset: true, ..Default::default() };
        assert_eq!(parse_extra(&extra, b"", need).zip64.offset, Some(7));
        // 全量布局：只要 offset，但写了三个 u64
        let mut extra = vec![0x01, 0x00, 24, 0];
        extra.extend_from_slice(&1u64.to_le_bytes());
        extra.extend_from_slice(&2u64.to_le_bytes());
        extra.extend_from_slice(&3u64.to_le_bytes());
        assert_eq!(parse_extra(&extra, b"", need).zip64.offset, Some(3));
        // 截断的块：不 panic
        let extra = [0x01, 0x00, 8, 0, 1, 2];
        assert_eq!(parse_extra(&extra, b"", need).zip64.offset, None);
    }

    /// AES 字段解析。
    #[test]
    fn aes_field() {
        let extra = [0x01, 0x99, 7, 0, 2, 0, b'A', b'E', 3, 8, 0];
        let p = parse_extra(&extra, b"", Zip64Need::default());
        assert_eq!(p.aes, Some(AesInfo { vendor_version: 2, strength: 3, method: 8 }));
        assert_eq!(p.aes.unwrap().salt_len(), 16);
        assert_eq!(p.aes.unwrap().key_len(), 32);
    }
}
