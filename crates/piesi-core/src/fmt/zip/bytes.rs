//! 小端字节游标：所有从压缩包读出来的定长字段都经它取，越界一律返回 `Corrupt`，永不 panic。

use crate::{Error, Result};

/// 只读游标，按小端读定长整数与定长切片。
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// 从切片开头建游标。
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// 剩余未读字节数。
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// 取 `n` 字节切片，不够则报损坏。
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(short());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// 跳过 `n` 字节。
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    /// 读 u8。
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// 读小端 u16。
    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// 读小端 u32。
    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// 读小端 u64。
    pub fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
}

/// 统一的「字段越界」错误。
fn short() -> Error {
    Error::Corrupt("记录不完整".into())
}

/// 小端读 u16（已知切片足够长时用，长度不足返回 None）。
pub fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

/// 小端读 u32。
pub fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// 小端读 u64。
pub fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8).map(|s| {
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        u64::from_le_bytes(a)
    })
}

/// 在 `hay` 里从 `from` 起正向找 `needle`，返回起始下标。
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// 在 `hay` 里从尾部往前找 `needle`，只看 `[..=end]` 范围内的起点，返回起始下标。
pub fn rfind(hay: &[u8], needle: &[u8], end: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let last = end.min(hay.len() - needle.len());
    (0..=last).rev().find(|&i| &hay[i..i + needle.len()] == needle)
}
