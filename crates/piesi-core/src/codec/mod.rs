//! 编解码层：把各压缩算法包成统一的 `Read` 适配器，供 zip/tar 驱动使用。
//! 这里只做「压缩流 → 明文流」，不认识任何容器；容器层负责限长（take）、CRC 与进度。

use std::io::{BufReader, Read};

use crate::archive::Method;
use crate::{DetailKind, Error, ErrorDetail, Result};

/// LZMA 字典上限：超过它的属性基本都是畸形输入，直接拒绝，避免一次性申请几 GB 内存。
const MAX_LZMA_DICT: u32 = 1536 << 20;

/// 按压缩方法把原始压缩流 `raw` 包成明文 `Read`。
/// `props` 是 7z 风格的编码器属性（LZMA 5 字节、LZMA2 1 字节、PPMd 5 字节）；
/// 传空时 LZMA/PPMd 按 zip 的约定从流头部自行读取参数（zip 把参数放在数据流开头，不放扩展字段）。
/// `unpacked` 是容器声明的解压后大小（不知道传 u64::MAX）：LZMA 的字典按它收紧，
/// 因为匹配距离不可能超过已产出的字节数，字典没必要大于整个条目；不收紧就是几十字节的恶意头换 1.5 GB 分配。
pub fn decoder<'a>(method: &Method, raw: Box<dyn Read + 'a>, props: &[u8], unpacked: u64) -> Result<Box<dyn Read + 'a>> {
    Ok(match method {
        Method::Store => raw,
        Method::Deflate => Box::new(flate2::read::DeflateDecoder::new(raw)),
        Method::Deflate64 => Box::new(deflate64::Deflate64Decoder::new(raw)),
        Method::Bzip2 => Box::new(bzip2::read::BzDecoder::new(raw)),
        Method::Lzma => lzma_decoder(raw, props, unpacked)?,
        Method::Lzma2 => lzma2_decoder(raw, props, unpacked)?,
        Method::Xz => {
            // 带内存上限：.xz 头可声明 4 GB 字典，解码器开场就按它分配
            let stream = liblzma::stream::Stream::new_stream_decoder(crate::sys::decode_memlimit(), 0).map_err(Error::corrupt)?;
            Box::new(liblzma::read::XzDecoder::new_stream(raw, stream))
        }
        Method::Zstd => Box::new(zstd::stream::read::Decoder::new(raw).map_err(Error::corrupt)?),
        Method::Ppmd => ppmd_decoder(raw, props)?,
        other => return Err(Error::Unsupported(ErrorDetail::with(DetailKind::CompressionMethod, other))),
    })
}

/// 读满 `buf`，读不满按「压缩包损坏」处理（流头部的参数必须完整）。
fn read_header(raw: &mut dyn Read, buf: &mut [u8], what: &str) -> Result<()> {
    raw.read_exact(buf).map_err(|_| Error::Corrupt(ErrorDetail::with(DetailKind::StreamHeaderIncomplete, what)))
}

/// 解码器实际用的字典：声明值与「解压后大小」取小，再不低于 liblzma 的下限 4 KB。
fn clamp_dict(declared: u32, unpacked: u64) -> u32 {
    let needed = unpacked.clamp(4096, u64::from(u32::MAX)) as u32;
    declared.min(needed).max(4096)
}

/// 校验 5 字节 LZMA 属性：首字节 lc/lp/pb 编码必须 < 225，字典大小不能离谱。
fn check_lzma_props(props: &[u8; 5]) -> Result<()> {
    if props[0] >= 9 * 5 * 5 {
        return Err(Error::Corrupt(DetailKind::LzmaPropsInvalid.into()));
    }
    let dict = u32::from_le_bytes([props[1], props[2], props[3], props[4]]);
    if dict > MAX_LZMA_DICT {
        return Err(Error::Unsupported(ErrorDetail::with(DetailKind::LzmaDictTooLarge, dict >> 20)));
    }
    Ok(())
}

/// LZMA1 原始流。zip（方法 14）的流头是「2 字节版本 + 2 字节属性长度 + 属性」，之后才是 LZMA 数据；
/// 7z 则把 5 字节属性放在 folder 里，经 `props` 传入。
fn lzma_decoder<'a>(mut raw: Box<dyn Read + 'a>, props: &[u8], unpacked: u64) -> Result<Box<dyn Read + 'a>> {
    let mut p = [0u8; 5];
    if props.is_empty() {
        // zip 头：版本号只做记录，属性长度理论上恒为 5，但有些写入器会多写，所以按声明长度跳过多余字节
        let mut head = [0u8; 4];
        read_header(&mut *raw, &mut head, "LZMA")?;
        let props_len = u16::from_le_bytes([head[2], head[3]]) as usize;
        if !(5..=64).contains(&props_len) {
            return Err(Error::Corrupt(ErrorDetail::with(DetailKind::LzmaPropsLength, props_len)));
        }
        let mut all = vec![0u8; props_len];
        read_header(&mut *raw, &mut all, "LZMA")?;
        p.copy_from_slice(&all[..5]);
    } else if props.len() >= 5 {
        p.copy_from_slice(&props[..5]);
    } else {
        return Err(Error::Corrupt(DetailKind::LzmaPropsShort.into()));
    }
    check_lzma_props(&p)?;
    // 字典按解压后大小收紧（liblzma 要求至少 4 KB），只改分配量，不改解码结果
    let dict = u32::from_le_bytes([p[1], p[2], p[3], p[4]]);
    p[1..5].copy_from_slice(&clamp_dict(dict, unpacked).to_le_bytes());
    let mut filters = liblzma::stream::Filters::new();
    filters.lzma1_properties(&p).map_err(Error::corrupt)?;
    let stream = liblzma::stream::Stream::new_raw_decoder(&filters).map_err(Error::corrupt)?;
    Ok(Box::new(liblzma::read::XzDecoder::new_stream(raw, stream)))
}

/// LZMA2 原始流（7z 用），属性 1 字节编码字典大小。
fn lzma2_decoder<'a>(raw: Box<dyn Read + 'a>, props: &[u8], unpacked: u64) -> Result<Box<dyn Read + 'a>> {
    if props.is_empty() {
        return Err(Error::Corrupt(DetailKind::Lzma2PropsMissing.into()));
    }
    // 属性 > 40 在 LZMA2 规范里非法；40 表示 4 GB 字典，同样拒绝
    if props[0] >= 40 {
        return Err(Error::Unsupported(DetailKind::Lzma2DictTooLarge.into()));
    }
    // 属性字节 → 字典：(2 | (p & 1)) << (p / 2 + 11)，再按解压后大小收紧
    let declared = (2u32 | u32::from(props[0] & 1)) << (props[0] / 2 + 11);
    let mut opts = liblzma::stream::LzmaOptions::new();
    opts.dict_size(clamp_dict(declared, unpacked));
    let mut filters = liblzma::stream::Filters::new();
    filters.lzma2(&opts);
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
            return Err(Error::Unsupported(ErrorDetail::with(DetailKind::PpmdRestoreMode, w >> 12)));
        }
        let dec = ppmd_rust::Ppmd8Decoder::new(BufReader::new(raw), order, mem, restore)
            .map_err(|e| Error::Corrupt(ErrorDetail::with(DetailKind::PpmdParamsInvalid, e)))?;
        Ok(Box::new(dec))
    } else if props.len() >= 5 {
        let order = props[0] as u32;
        let mem = u32::from_le_bytes([props[1], props[2], props[3], props[4]]);
        // PPMd 的模型内存必须和编码时一致，不能收紧，只能封顶：超过解码内存上限的声明一律拒绝
        if u64::from(mem) > crate::sys::decode_memlimit() {
            return Err(Error::Unsupported(ErrorDetail::with(DetailKind::PpmdMemTooLarge, mem >> 20)));
        }
        let dec = ppmd_rust::Ppmd7Decoder::new(BufReader::new(raw), order, mem)
            .map_err(|e| Error::Corrupt(ErrorDetail::with(DetailKind::PpmdParamsInvalid, e)))?;
        Ok(Box::new(dec))
    } else {
        Err(Error::Corrupt(DetailKind::PpmdPropsShort.into()))
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
        let mut dec = decoder(&Method::Deflate, Box::new(&packed[..]), &[], u64::MAX).unwrap();
        let mut out = Vec::new();
        dec.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    /// 不认识的方法要返回 Unsupported 而不是 panic。
    #[test]
    fn unknown_method_is_unsupported() {
        let r = decoder(&Method::Rar(5), Box::new(&b""[..]), &[], u64::MAX);
        assert!(matches!(r, Err(Error::Unsupported(_))));
    }

    /// zip LZMA 流头不完整时返回 Corrupt。
    #[test]
    fn lzma_short_header_is_corrupt() {
        let r = decoder(&Method::Lzma, Box::new(&b"\x09\x14\x05"[..]), &[], u64::MAX);
        assert!(matches!(r, Err(Error::Corrupt(_))));
    }
}
