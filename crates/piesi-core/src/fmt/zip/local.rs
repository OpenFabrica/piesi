//! 本地文件头：解压前定位数据起点。大小与 CRC 一律以中央目录为准，
//! 本地头只贡献 name/extra 的长度（它们可能和目录头不同）与标志位。

use std::io::{Read, Seek, SeekFrom};

use super::bytes::Cursor;
use super::cd::SIG_LOCAL;
use crate::{DetailKind, Error, ErrorDetail, Result};

/// 本地头定长部分长度。
pub const LOCAL_HEADER_LEN: usize = 30;

/// 本地头定长部分。
#[derive(Clone, Copy, Debug)]
pub struct LocalHeader {
    pub flags: u16,
    pub method: u16,
    pub dos_time: u16,
    pub dos_date: u16,
    pub crc: u32,
    pub packed: u32,
    pub size: u32,
    pub name_len: usize,
    pub extra_len: usize,
}

impl LocalHeader {
    /// 整个头（定长 + 名字 + 扩展字段）的长度，数据紧随其后。
    pub fn header_len(&self) -> u64 {
        (LOCAL_HEADER_LEN + self.name_len + self.extra_len) as u64
    }

    /// 是否带 data descriptor（bit 3）。
    pub fn has_descriptor(&self) -> bool {
        self.flags & 0x0008 != 0
    }
}

/// 解析 30 字节定长部分（含签名校验）。
pub fn parse_local_header(buf: &[u8]) -> Result<LocalHeader> {
    let mut c = Cursor::new(buf);
    if c.take(4)? != SIG_LOCAL {
        return Err(Error::Corrupt(DetailKind::LocalHeaderBadSignature.into()));
    }
    let _version_needed = c.u16()?;
    let flags = c.u16()?;
    let method = c.u16()?;
    let dos_time = c.u16()?;
    let dos_date = c.u16()?;
    let crc = c.u32()?;
    let packed = c.u32()?;
    let size = c.u32()?;
    let name_len = c.u16()? as usize;
    let extra_len = c.u16()? as usize;
    Ok(LocalHeader { flags, method, dos_time, dos_date, crc, packed, size, name_len, extra_len })
}

/// seek 到 `offset` 读本地头，返回头信息与数据起点（逻辑偏移）。`len` 是流总长，用来做边界检查。
pub fn read_local_header<S: Read + Seek>(src: &mut S, offset: u64, len: u64) -> Result<(LocalHeader, u64)> {
    if offset.checked_add(LOCAL_HEADER_LEN as u64).is_none_or(|end| end > len) {
        return Err(Error::Corrupt(ErrorDetail::with(DetailKind::LocalHeaderOffsetOutOfRange, offset)));
    }
    src.seek(SeekFrom::Start(offset))?;
    let mut buf = [0u8; LOCAL_HEADER_LEN];
    src.read_exact(&mut buf).map_err(|_| Error::Corrupt(DetailKind::LocalHeaderIncomplete.into()))?;
    let h = parse_local_header(&buf)?;
    let data_start = offset + h.header_len();
    if data_start > len {
        return Err(Error::Corrupt(DetailKind::LocalHeaderOutOfRange.into()));
    }
    Ok((h, data_start))
}
