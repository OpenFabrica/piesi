//! 编解码层：把各压缩算法包成统一的 `Read` 适配器，供 zip/tar 驱动使用。
//! 这里只做「压缩流 → 明文流」，不认识任何容器；容器层负责限长（take）、CRC 与进度。

use std::io::{BufReader, Read};

use crate::archive::Method;
use crate::{Error, Result};

/// LZMA 字典上限：超过它的属性基本都是畸形输入，直接拒绝，避免一次性申请几 GB 内存。
const MAX_LZMA_DICT: u32 = 1536 << 20;

/// 按压缩方法把原始压缩流 `raw` 包成明文 `Read`。
/// `props` 是 7z 风格的编码器属性（LZMA 5 字节、LZMA2 1 字节、PPMd 5 字节）；
/// 传空时 LZMA/PPMd 按 zip 的约定从流头部自行读取参数（zip 把参数放在数据流开头，不放扩展字段）。
pub fn decoder<'a>(method: &Method, raw: Box<dyn Read + 'a>, props: &[u8]) -> Result<Box<dyn Read + 'a>> {
    Ok(match method {
        Method::Store => raw,
        Method::Deflate => Box::new(flate2::read::DeflateDecoder::new(raw)),
        Method::Deflate64 => Box::new(deflate64::Deflate64Decoder::new(raw)),
        Method::Bzip2 => Box::new(bzip2::read::BzDecoder::new(raw)),
        Method::Lzma => lzma_decoder(raw, props)?,
        Method::Lzma2 => lzma2_decoder(raw, props)?,
        Method::Xz => Box::new(liblzma::read::XzDecoder::new(raw)),
        Method::Zstd => Box::new(zstd::stream::read::Decoder::new(raw).map_err(Error::corrupt)?),
        Method::Ppmd => ppmd_decoder(raw, props)?,
        other => return Err(Error::Unsupported(format!("压缩方法 {other}"))),
    })
}

/// 读满 `buf`，读不满按「压缩包损坏」处理（流头部的参数必须完整）。
fn read_header(raw: &mut dyn Read, buf: &mut [u8], what: &str) -> Result<()> {
    raw.read_exact(buf).map_err(|_| Error::Corrupt(format!("{what}：流头部不完整")))
}

/// 校验 5 字节 LZMA 属性：首字节 lc/lp/pb 编码必须 < 225，字典大小不能离谱。
fn check_lzma_props(props: &[u8; 5]) -> Result<()> {
    if props[0] >= 9 * 5 * 5 {
        return Err(Error::Corrupt("LZMA 属性字节非法".into()));
    }
    let dict = u32::from_le_bytes([props[1], props[2], props[3], props[4]]);
    if dict > MAX_LZMA_DICT {
        return Err(Error::Unsupported(format!("LZMA 字典过大：{} MB", dict >> 20)));
    }
    Ok(())
}

/// LZMA1 原始流。zip（方法 14）的流头是「2 字节版本 + 2 字节属性长度 + 属性」，之后才是 LZMA 数据；
/// 7z 则把 5 字节属性放在 folder 里，经 `props` 传入。
fn lzma_decoder<'a>(mut raw: Box<dyn Read + 'a>, props: &[u8]) -> Result<Box<dyn Read + 'a>> {
    let mut p = [0u8; 5];
    if props.is_empty() {
        // zip 头：版本号只做记录，属性长度理论上恒为 5，但有些写入器会多写，所以按声明长度跳过多余字节
        let mut head = [0u8; 4];
        read_header(&mut *raw, &mut head, "LZMA")?;
        let props_len = u16::from_le_bytes([head[2], head[3]]) as usize;
        if !(5..=64).contains(&props_len) {
            return Err(Error::Corrupt(format!("LZMA 属性长度非法：{props_len}")));
        }
        let mut all = vec![0u8; props_len];
        read_header(&mut *raw, &mut all, "LZMA")?;
        p.copy_from_slice(&all[..5]);
    } else if props.len() >= 5 {
        p.copy_from_slice(&props[..5]);
    } else {
        return Err(Error::Corrupt("LZMA 属性不足 5 字节".into()));
    }
    check_lzma_props(&p)?;
    let mut filters = liblzma::stream::Filters::new();
    filters.lzma1_properties(&p).map_err(Error::corrupt)?;
    let stream = liblzma::stream::Stream::new_raw_decoder(&filters).map_err(Error::corrupt)?;
    Ok(Box::new(liblzma::read::XzDecoder::new_stream(raw, stream)))
}

/// LZMA2 原始流（7z 用），属性 1 字节编码字典大小。
fn lzma2_decoder<'a>(raw: Box<dyn Read + 'a>, props: &[u8]) -> Result<Box<dyn Read + 'a>> {
    if props.is_empty() {
        return Err(Error::Corrupt("LZMA2 缺少属性字节".into()));
    }
    // 属性 > 40 在 LZMA2 规范里非法；40 表示 4 GB 字典，同样拒绝
    if props[0] >= 40 {
        return Err(Error::Unsupported("LZMA2 字典过大".into()));
    }
    let mut filters = liblzma::stream::Filters::new();
    filters.lzma2_properties(&props[..1]).map_err(Error::corrupt)?;
    let stream = liblzma::stream::Stream::new_raw_decoder(&filters).map_err(Error::corrupt)?;
    Ok(Box::new(liblzma::read::XzDecoder::new_stream(raw, stream)))
}

/// PPMd。zip（方法 98）用 PPMd var.I rev.1（Ppmd8），参数是流头 2 字节：
/// order = (w & 0xF) + 1，mem = ((w >> 4) & 0xFF) + 1 MB，restore = w >> 12；
/// 7z 用 PPMd var.H（Ppmd7），`props` 为 1 字节 order + 4 字节 mem。
fn ppmd_decoder<'a>(mut raw: Box<dyn Read + 'a>, props: &[u8]) -> Result<Box<dyn Read + 'a>> {
    // 区间解码器按字节取输入，底下垫一层缓冲避免每字节一次系统调用
    if props.is_empty() {
        let mut head = [0u8; 2];
        read_header(&mut *raw, &mut head, "PPMd")?;
        let w = u16::from_le_bytes(head) as u32;
        let order = (w & 0x0F) + 1;
        let mem = (((w >> 4) & 0xFF) + 1) << 20;
        let restore = ppmd_rust::RestoreMethod::from(w >> 12);
        if restore == ppmd_rust::RestoreMethod::Unsupported {
            return Err(Error::Unsupported(format!("PPMd 恢复模式 {}", w >> 12)));
        }
        let dec = ppmd_rust::Ppmd8Decoder::new(BufReader::new(raw), order, mem, restore)
            .map_err(|e| Error::Corrupt(format!("PPMd 参数非法：{e}")))?;
        Ok(Box::new(dec))
    } else if props.len() >= 5 {
        let order = props[0] as u32;
        let mem = u32::from_le_bytes([props[1], props[2], props[3], props[4]]);
        let dec = ppmd_rust::Ppmd7Decoder::new(BufReader::new(raw), order, mem)
            .map_err(|e| Error::Corrupt(format!("PPMd 参数非法：{e}")))?;
        Ok(Box::new(dec))
    } else {
        Err(Error::Corrupt("PPMd 属性不足 5 字节".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Deflate 往返：flate2 压缩后经 decoder 解回原文。
    #[test]
    fn deflate_roundtrip() {
        let data = b"hello hello hello hello piesi".repeat(100);
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&data).unwrap();
        let packed = enc.finish().unwrap();
        let mut dec = decoder(&Method::Deflate, Box::new(&packed[..]), &[]).unwrap();
        let mut out = Vec::new();
        dec.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    /// 不认识的方法要返回 Unsupported 而不是 panic。
    #[test]
    fn unknown_method_is_unsupported() {
        let r = decoder(&Method::Rar(5), Box::new(&b""[..]), &[]);
        assert!(matches!(r, Err(Error::Unsupported(_))));
    }

    /// zip LZMA 流头不完整时返回 Corrupt。
    #[test]
    fn lzma_short_header_is_corrupt() {
        let r = decoder(&Method::Lzma, Box::new(&b"\x09\x14\x05"[..]), &[]);
        assert!(matches!(r, Err(Error::Corrupt(_))));
    }
}
