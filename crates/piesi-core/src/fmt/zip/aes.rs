//! WinZip AES（AE-1 / AE-2）加解密流：PBKDF2-HMAC-SHA1（1000 轮）派生密钥，
//! AES-CTR（128 位小端计数器从 1 起）加解密，末尾 10 字节 HMAC-SHA1 做完整性校验。
//! 读写两边共用密钥派生和 CTR 密钥流，写入器给 zip 写入器的「密码」选项用。

use std::io::{self, Read, Write};

use aes::cipher::{BlockCipherEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use super::extra::AesInfo;
use crate::{Error, Result};

type HmacSha1 = Hmac<Sha1>;

/// 认证码长度（HMAC-SHA1 截前 10 字节）。
pub const AUTH_LEN: usize = 10;
/// 口令校验值长度。
pub const VERIFY_LEN: usize = 2;
/// PBKDF2 轮数，WinZip 规范固定 1000。
const PBKDF2_ROUNDS: u32 = 1000;

/// 强度 1/2/3 的盐长度：8/12/16 字节。
pub fn salt_len(strength: u8) -> usize {
    4 + 4 * strength as usize
}

/// 强度 1/2/3 的密钥长度：16/24/32 字节。
fn key_len(strength: u8) -> usize {
    8 + 8 * strength as usize
}

/// 一个加密条目在包里的固定开销：盐 + 口令校验值 + 认证码。
pub fn overhead(strength: u8) -> u64 {
    (salt_len(strength) + VERIFY_LEN + AUTH_LEN) as u64
}

/// 三种强度共用一个枚举，免得把泛型参数一路传到管线。
enum AnyAes {
    A128(aes::Aes128),
    A192(aes::Aes192),
    A256(aes::Aes256),
}

impl AnyAes {
    /// 按强度建块密码。
    fn new(strength: u8, key: &[u8]) -> Result<Self> {
        let bad = |_| Error::Corrupt("AES 密钥长度错误".into());
        Ok(match strength {
            1 => Self::A128(aes::Aes128::new_from_slice(key).map_err(bad)?),
            2 => Self::A192(aes::Aes192::new_from_slice(key).map_err(bad)?),
            3 => Self::A256(aes::Aes256::new_from_slice(key).map_err(bad)?),
            s => return Err(Error::Unsupported(format!("AES 强度 {s}"))),
        })
    }

    /// 原地加密一个 16 字节块。
    fn encrypt(&self, block: &mut [u8; 16]) {
        let mut b = aes::Block::from(*block);
        match self {
            Self::A128(c) => c.encrypt_block(&mut b),
            Self::A192(c) => c.encrypt_block(&mut b),
            Self::A256(c) => c.encrypt_block(&mut b),
        }
        block.copy_from_slice(b.as_slice());
    }
}

/// 从口令和盐派生块密码、HMAC 和 2 字节口令校验值（PBKDF2 一次输出三段）。
fn derive(password: &[u8], salt: &[u8], strength: u8) -> Result<(AnyAes, HmacSha1, [u8; VERIFY_LEN])> {
    let key_len = key_len(strength);
    let mut dk = vec![0u8; 2 * key_len + VERIFY_LEN];
    pbkdf2::pbkdf2_hmac::<Sha1>(password, salt, PBKDF2_ROUNDS, &mut dk);
    let cipher = AnyAes::new(strength, &dk[..key_len])?;
    let hmac = HmacSha1::new_from_slice(&dk[key_len..2 * key_len]).map_err(|_| Error::Corrupt("HMAC 密钥长度错误".into()))?;
    let mut verify = [0u8; VERIFY_LEN];
    verify.copy_from_slice(&dk[2 * key_len..]);
    Ok((cipher, hmac, verify))
}

/// AES-CTR 密钥流：WinZip 的计数器是 128 位小端整数，首块为 1，每块加一后做 AES 加密。
/// 加密和解密都是「数据异或密钥流」，读写两边共用。
struct Ctr {
    cipher: AnyAes,
    counter: [u8; 16],
    keystream: [u8; 16],
    ks_used: usize,
}

impl Ctr {
    fn new(cipher: AnyAes) -> Self {
        Self { cipher, counter: [0u8; 16], keystream: [0u8; 16], ks_used: 16 }
    }

    /// 生成下一块密钥流。
    fn next_keystream(&mut self) {
        for b in self.counter.iter_mut() {
            *b = b.wrapping_add(1);
            if *b != 0 {
                break;
            }
        }
        self.keystream = self.counter;
        self.cipher.encrypt(&mut self.keystream);
        self.ks_used = 0;
    }

    /// 原地异或密钥流。
    fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            if self.ks_used == 16 {
                self.next_keystream();
            }
            *b ^= self.keystream[self.ks_used];
            self.ks_used += 1;
        }
    }
}

/// AES 解密流。读到的密文先喂 HMAC 再异或密钥流，密文读完后由 `finish` 校验认证码。
pub struct AesReader<R> {
    inner: R,
    ctr: Ctr,
    hmac: Option<HmacSha1>,
    /// 还没读的密文字节数（不含末尾认证码）
    remaining: u64,
    verified: bool,
}

impl<R: Read> AesReader<R> {
    /// 读盐与口令校验值，派生密钥并核对口令。`packed_size` 是条目在包里的总长（盐 + 校验值 + 密文 + 认证码）。
    pub fn new(mut inner: R, password: &[u8], info: &AesInfo, packed_size: u64) -> Result<Self> {
        let salt_len = info.salt_len();
        let overhead = (salt_len + VERIFY_LEN + AUTH_LEN) as u64;
        if packed_size < overhead {
            return Err(Error::Corrupt("AES 条目长度不足".into()));
        }
        let mut salt = [0u8; 16];
        inner.read_exact(&mut salt[..salt_len]).map_err(|_| Error::Corrupt("AES 盐不完整".into()))?;
        let mut verify = [0u8; VERIFY_LEN];
        inner.read_exact(&mut verify).map_err(|_| Error::Corrupt("AES 口令校验值不完整".into()))?;

        let (cipher, hmac, expected) = derive(password, &salt[..salt_len], info.strength)?;
        if expected != verify {
            return Err(Error::WrongPassword);
        }
        Ok(Self { inner, ctr: Ctr::new(cipher), hmac: Some(hmac), remaining: packed_size - overhead, verified: false })
    }

    /// 读完剩余密文并校验认证码。
    /// 为什么要先排空：解压器常在凑够明文后就不再拉输入，最后几字节密文还没过 HMAC；
    /// 不读完就校验等于没校验。
    pub fn finish(&mut self) -> Result<()> {
        if self.verified {
            return Ok(());
        }
        let mut sink = [0u8; 4096];
        while self.remaining > 0 {
            match self.read(&mut sink) {
                Ok(0) => return Err(Error::Corrupt("AES 密文提前结束".into())),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(Error::Corrupt("AES 密文提前结束".into()))
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
        let mut tag = [0u8; AUTH_LEN];
        self.inner.read_exact(&mut tag).map_err(|_| Error::Corrupt("AES 认证码缺失".into()))?;
        self.verified = true;
        let Some(hmac) = self.hmac.take() else {
            return Ok(());
        };
        let full = hmac.finalize().into_bytes();
        if full[..AUTH_LEN] != tag {
            return Err(Error::Corrupt("AES 认证失败：数据被改动或密码错误".into()));
        }
        Ok(())
    }
}

impl<R: Read> Read for AesReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.remaining) as usize;
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "AES 密文提前结束"));
        }
        if let Some(h) = self.hmac.as_mut() {
            h.update(&buf[..n]);
        }
        self.ctr.apply(&mut buf[..n]);
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// AES 加密流：`new` 先写盐和口令校验值，之后写入的明文按 CTR 加密、密文喂 HMAC，`finish` 追加认证码。
/// 盐由调用方提供（必须随机、长度等于 `salt_len(strength)`），这里不碰随机源，测试也能复现。
pub struct AesWriter<W: Write> {
    inner: W,
    ctr: Ctr,
    hmac: HmacSha1,
    /// 密文暂存：调用方的明文不能改，密文又要整块喂 HMAC 和写出
    buf: Vec<u8>,
}

impl<W: Write> AesWriter<W> {
    /// 派生密钥并写出盐 + 口令校验值。
    pub fn new(mut inner: W, password: &[u8], strength: u8, salt: &[u8]) -> Result<Self> {
        if !(1..=3).contains(&strength) || salt.len() != salt_len(strength) {
            return Err(Error::Other("AES 强度或盐长度不合法".into()));
        }
        let (cipher, hmac, verify) = derive(password, salt, strength)?;
        inner.write_all(salt)?;
        inner.write_all(&verify)?;
        Ok(Self { inner, ctr: Ctr::new(cipher), hmac, buf: Vec::new() })
    }

    /// 写认证码并交回底层写入器。
    pub fn finish(mut self) -> io::Result<W> {
        let tag = self.hmac.finalize().into_bytes();
        self.inner.write_all(&tag[..AUTH_LEN])?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for AesWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        for chunk in data.chunks(64 * 1024) {
            self.buf.clear();
            self.buf.extend_from_slice(chunk);
            self.ctr.apply(&mut self.buf);
            self.hmac.update(&self.buf);
            self.inner.write_all(&self.buf)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 独立于 AesWriter 的参考实现：用原语在测试里造一个 AES-256 条目（盐 + 校验值 + CTR 密文 + HMAC 前 10 字节），
    /// 既验读取器，也当写入器的已知答案。
    fn make_entry(password: &[u8], plain: &[u8]) -> Vec<u8> {
        let salt = [7u8; 16];
        let mut dk = vec![0u8; 66];
        pbkdf2::pbkdf2_hmac::<Sha1>(password, &salt, PBKDF2_ROUNDS, &mut dk);
        let cipher = AnyAes::new(3, &dk[..32]).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(&salt);
        out.extend_from_slice(&dk[64..66]);
        let mut counter = [0u8; 16];
        let mut ct = Vec::with_capacity(plain.len());
        for chunk in plain.chunks(16) {
            for b in counter.iter_mut() {
                *b = b.wrapping_add(1);
                if *b != 0 {
                    break;
                }
            }
            let mut ks = counter;
            cipher.encrypt(&mut ks);
            ct.extend(chunk.iter().zip(ks.iter()).map(|(p, k)| p ^ k));
        }
        let mut h = HmacSha1::new_from_slice(&dk[32..64]).unwrap();
        h.update(&ct);
        out.extend_from_slice(&ct);
        out.extend_from_slice(&h.finalize().into_bytes()[..AUTH_LEN]);
        out
    }

    /// 正确口令解出原文并通过认证；错口令、篡改、截断各自报对应错误。
    #[test]
    fn decrypt_verify_and_reject() {
        let plain = b"piesi aes test data 0123456789".repeat(5);
        let packed = make_entry(b"TEST", &plain);
        let info = AesInfo { vendor_version: 2, strength: 3, method: 0 };

        let mut r = AesReader::new(&packed[..], b"TEST", &info, packed.len() as u64).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, plain);
        r.finish().unwrap();

        assert!(matches!(AesReader::new(&packed[..], b"BAD", &info, packed.len() as u64), Err(Error::WrongPassword)));

        let mut tampered = packed.clone();
        tampered[30] ^= 0x01;
        let mut r = AesReader::new(&tampered[..], b"TEST", &info, tampered.len() as u64).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert!(matches!(r.finish(), Err(Error::Corrupt(_))));

        // 只读一半就 finish：余下密文要被排空后再校验，结果仍然正确
        let mut r = AesReader::new(&packed[..], b"TEST", &info, packed.len() as u64).unwrap();
        let mut half = vec![0u8; 20];
        r.read_exact(&mut half).unwrap();
        assert_eq!(&half[..], &plain[..20]);
        r.finish().unwrap();

        let short = &packed[..packed.len() - 5];
        let mut r = AesReader::new(short, b"TEST", &info, packed.len() as u64).unwrap();
        let mut out = Vec::new();
        assert!(r.read_to_end(&mut out).is_err() || matches!(r.finish(), Err(Error::Corrupt(_))));
    }

    /// 写入器的输出必须与参考实现逐字节相同（同盐同口令），且不管怎么切块写都一样；空明文也合法。
    #[test]
    fn writer_matches_reference_and_roundtrips() {
        let plain = b"piesi aes writer 0123456789 abcdefghijklmnopqrstuvwxyz".repeat(37);
        let expected = make_entry(b"TEST", &plain);

        let mut w = AesWriter::new(Vec::new(), b"TEST", 3, &[7u8; 16]).unwrap();
        w.write_all(&plain).unwrap();
        assert_eq!(w.finish().unwrap(), expected);

        // 按奇怪的块大小写，密钥流跨块衔接要正确
        let mut w = AesWriter::new(Vec::new(), b"TEST", 3, &[7u8; 16]).unwrap();
        for chunk in plain.chunks(13) {
            w.write_all(chunk).unwrap();
        }
        let packed = w.finish().unwrap();
        assert_eq!(packed, expected);

        let info = AesInfo { vendor_version: 2, strength: 3, method: 0 };
        let mut r = AesReader::new(&packed[..], b"TEST", &info, packed.len() as u64).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        r.finish().unwrap();
        assert_eq!(out, plain);

        let empty = AesWriter::new(Vec::new(), b"TEST", 3, &[7u8; 16]).unwrap().finish().unwrap();
        assert_eq!(empty.len() as u64, overhead(3));
        let mut r = AesReader::new(&empty[..], b"TEST", &info, empty.len() as u64).unwrap();
        r.finish().unwrap();

        assert!(AesWriter::new(Vec::new(), b"TEST", 3, &[0u8; 8]).is_err(), "盐长度与强度不匹配应拒绝");
    }
}
