//! WinZip AES（AE-1 / AE-2）解密流：PBKDF2-HMAC-SHA1（1000 轮）派生密钥，
//! AES-CTR（128 位小端计数器从 1 起）解密，末尾 10 字节 HMAC-SHA1 做完整性校验。

use std::io::{self, Read};

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

/// AES 解密流。读到的密文先喂 HMAC 再异或密钥流，密文读完后由 `finish` 校验认证码。
pub struct AesReader<R> {
    inner: R,
    cipher: AnyAes,
    hmac: Option<HmacSha1>,
    counter: [u8; 16],
    keystream: [u8; 16],
    ks_used: usize,
    /// 还没读的密文字节数（不含末尾认证码）
    remaining: u64,
    verified: bool,
}

impl<R: Read> AesReader<R> {
    /// 读盐与口令校验值，派生密钥并核对口令。`packed_size` 是条目在包里的总长（盐 + 校验值 + 密文 + 认证码）。
    pub fn new(mut inner: R, password: &[u8], info: &AesInfo, packed_size: u64) -> Result<Self> {
        let salt_len = info.salt_len();
        let key_len = info.key_len();
        let overhead = (salt_len + VERIFY_LEN + AUTH_LEN) as u64;
        if packed_size < overhead {
            return Err(Error::Corrupt("AES 条目长度不足".into()));
        }
        let mut salt = [0u8; 16];
        inner.read_exact(&mut salt[..salt_len]).map_err(|_| Error::Corrupt("AES 盐不完整".into()))?;
        let mut verify = [0u8; VERIFY_LEN];
        inner.read_exact(&mut verify).map_err(|_| Error::Corrupt("AES 口令校验值不完整".into()))?;

        // 派生 key_len 字节加密密钥 + key_len 字节 HMAC 密钥 + 2 字节口令校验值
        let mut dk = vec![0u8; 2 * key_len + VERIFY_LEN];
        pbkdf2::pbkdf2_hmac::<Sha1>(password, &salt[..salt_len], PBKDF2_ROUNDS, &mut dk);
        if dk[2 * key_len..] != verify {
            return Err(Error::WrongPassword);
        }
        let cipher = AnyAes::new(info.strength, &dk[..key_len])?;
        let hmac = HmacSha1::new_from_slice(&dk[key_len..2 * key_len])
            .map_err(|_| Error::Corrupt("HMAC 密钥长度错误".into()))?;
        Ok(Self {
            inner,
            cipher,
            hmac: Some(hmac),
            counter: [0u8; 16],
            keystream: [0u8; 16],
            ks_used: 16,
            remaining: packed_size - overhead,
            verified: false,
        })
    }

    /// 生成下一块密钥流：计数器按 128 位小端整数加一（首块为 1），再做 AES 加密。
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
        for b in &mut buf[..n] {
            if self.ks_used == 16 {
                self.next_keystream();
            }
            *b ^= self.keystream[self.ks_used];
            self.ks_used += 1;
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用同样的原语在测试里造一个 AES-256 条目：盐 + 校验值 + CTR 密文 + HMAC 前 10 字节。
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
}
