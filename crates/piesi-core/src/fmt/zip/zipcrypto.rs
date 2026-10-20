//! ZipCrypto（PKWARE 传统加密）解密流。算法很弱但存量极多；只做解密，不做加密。

use std::io::{self, Read};

use crate::{DetailKind, Error, Result};

/// 标准 CRC-32 查表（反射多项式 0xEDB88320），密钥更新要逐字节查它。
const CRC_TABLE: [u32; 256] = build_crc_table();

/// 编译期生成 CRC 表。
const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// 三个 32 位密钥组成的状态机（appnote 6.1.x 的 "traditional PKWARE encryption"）。
#[derive(Clone)]
pub struct Keys([u32; 3]);

impl Keys {
    /// 用口令初始化密钥。
    pub fn new(password: &[u8]) -> Self {
        let mut k = Keys([0x1234_5678, 0x2345_6789, 0x3456_7890]);
        for &b in password {
            k.update(b);
        }
        k
    }

    /// 单字节 CRC 更新。
    fn crc32(crc: u32, b: u8) -> u32 {
        CRC_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8)
    }

    /// 用一个明文字节推进密钥。
    fn update(&mut self, b: u8) {
        self.0[0] = Self::crc32(self.0[0], b);
        self.0[1] = self.0[1].wrapping_add(self.0[0] & 0xFF).wrapping_mul(134_775_813).wrapping_add(1);
        self.0[2] = Self::crc32(self.0[2], (self.0[1] >> 24) as u8);
    }

    /// 当前密钥流字节。规范里 temp 是 16 位整数，乘法在 32 位里做，所以先截成 u16 再扩回 u32。
    fn stream_byte(&self) -> u8 {
        let t = (self.0[2] | 2) as u16 as u32;
        (t.wrapping_mul(t ^ 1) >> 8) as u8
    }

    /// 解密一个字节并推进密钥。
    pub fn decrypt(&mut self, c: u8) -> u8 {
        let p = c ^ self.stream_byte();
        self.update(p);
        p
    }

    /// 加密一个字节并推进密钥（只给测试造样本用）。
    #[cfg(test)]
    pub fn encrypt(&mut self, p: u8) -> u8 {
        let c = p ^ self.stream_byte();
        self.update(p);
        c
    }
}

/// 解密流：先吃掉 12 字节加密头并校验口令，之后逐字节解密透传。
pub struct ZipCryptoReader<R> {
    inner: R,
    keys: Keys,
}

impl<R: Read> ZipCryptoReader<R> {
    /// 读 12 字节加密头并校验口令。`check` 是期望的校验字节：
    /// 正常是 CRC32 的最高字节；有 data descriptor（写入时还不知道 CRC）则是 DOS 时间的高字节。
    pub fn new(mut inner: R, password: &[u8], check: u8) -> Result<Self> {
        let mut keys = Keys::new(password);
        let mut head = [0u8; 12];
        inner.read_exact(&mut head).map_err(|_| Error::Corrupt(DetailKind::ZipCryptoHeaderIncomplete.into()))?;
        let mut last = 0u8;
        for b in head {
            last = keys.decrypt(b);
        }
        // 只有第 12 字节是可靠的校验位（PKZIP 2.0 起），1/256 的误判交给后面的 CRC 兜底
        if last != check {
            return Err(Error::WrongPassword);
        }
        Ok(Self { inner, keys })
    }
}

impl<R: Read> Read for ZipCryptoReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        for b in &mut buf[..n] {
            *b = self.keys.decrypt(*b);
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用同一套密钥机加密再解密，校验字节对得上，错口令被拒。
    #[test]
    fn roundtrip_and_check_byte() {
        let plain = b"The quick brown fox jumps over the lazy dog".repeat(3);
        let crc = crc32fast::hash(&plain);
        let check = (crc >> 24) as u8;
        let mut head = [0x5Au8; 12];
        head[11] = check;
        let mut keys = Keys::new(b"TEST");
        let mut packed: Vec<u8> = head.iter().map(|&b| keys.encrypt(b)).collect();
        packed.extend(plain.iter().map(|&b| keys.encrypt(b)));

        let mut r = ZipCryptoReader::new(&packed[..], b"TEST", check).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, plain);

        assert!(matches!(ZipCryptoReader::new(&packed[..], b"WRONG", check), Err(Error::WrongPassword)));
        assert!(matches!(ZipCryptoReader::new(&packed[..3], b"TEST", check), Err(Error::Corrupt(_))));
    }
}
