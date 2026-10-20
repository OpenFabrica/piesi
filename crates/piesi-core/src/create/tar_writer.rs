//! tar.gz / tar.xz / tar.zst 写入器：tar crate 打包，外层压缩流（xz、zstd 多线程）。
//!
//! 「Mac 专用」时照 macOS 自带 bsdtar（libarchive 3.7 + copyfile）的做法把元数据写两份：
//! - 同目录的 `._名字` AppleDouble 条目，放在条目自己前面：macOS 上 `tar -x`、归档工具默认靠它还原；
//! - 条目前的 PAX 扩展头 `LIBARCHIVE.xattr.*`（base64）和 `SCHILY.xattr.*`（原字节）：
//!   `bsdtar --xattrs`、GNU tar `--xattrs` 靠它还原，Finder 信息和资源分支也作为同名 xattr 写进去。

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

use crate::archive::EntryKind;
use crate::create::apple_double::{self, MacMetadata};
use crate::create::{CreateFormat, CreateOptions, SourceItem, Sources};
use crate::progress::{OpContext, Throttle};
use crate::warning::{Warning, WarningKind};
use crate::{Error, Result};

/// 边读边报进度的读取器（tar 写入是单线程顺序的，直接借用 Throttle）。
struct ProgressRead<'a, 'b> {
    inner: File,
    throttle: &'a mut Throttle<'b>,
}

impl Read for ProgressRead<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.throttle.add_bytes(n as u64).map_err(io::Error::other)?;
        Ok(n)
    }
}

/// 写出 tar.*，返回「Mac 专用」读元数据时产生的警告。
pub fn write_tar(dest: &Path, sources: &Sources, opts: &CreateOptions, ctx: &OpContext<'_>) -> Result<Vec<Warning>> {
    let threads = if opts.threads == 0 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) } else { opts.threads } as u32;
    let file = BufWriter::with_capacity(1 << 20, File::create(dest)?);
    let encoder: Box<dyn FinishWrite> = match opts.format {
        CreateFormat::TarGz => Box::new(flate2::write::GzEncoder::new(file, flate2::Compression::new(opts.level.clamp(1, 9) as u32))),
        CreateFormat::TarXz => {
            // xz 的多线程编码：按块切分并行，preset 对应 0–9
            let stream = liblzma::stream::MtStreamBuilder::new()
                .preset(opts.level.clamp(0, 9) as u32)
                .threads(threads)
                .encoder()
                .map_err(Error::other)?;
            Box::new(liblzma::write::XzEncoder::new_stream(file, stream))
        }
        CreateFormat::TarZst => {
            let mut enc = zstd::stream::write::Encoder::new(file, zstd_level(opts.level)).map_err(Error::other)?;
            enc.multithread(threads).map_err(Error::other)?;
            Box::new(enc)
        }
        other => return Err(Error::Unsupported(format!("{other:?} 不是 tar 格式"))),
    };
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    let mut throttle = Throttle::new(ctx, Some(sources.total_bytes), Some(sources.items.len() as u64));
    let mut warnings = Vec::new();

    for item in &sources.items {
        throttle.begin_entry(&item.rel)?;
        // 符号链接不带元数据：apple_double::read 不跟随链接，读到的是链接本身，bsdtar 也不给链接写 `._`
        if opts.mac_metadata && item.kind != EntryKind::Symlink {
            write_mac_metadata(&mut builder, item, &mut warnings)?;
        }
        let mut header = tar::Header::new_gnu();
        let meta = std::fs::symlink_metadata(&item.path)?;
        header.set_metadata_in_mode(&meta, tar::HeaderMode::Complete);
        match item.kind {
            EntryKind::Dir => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                builder.append_data(&mut header, format!("{}/", item.rel), io::empty())?;
            }
            EntryKind::Symlink => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                let target = String::from_utf8_lossy(item.link_target.as_deref().unwrap_or_default()).into_owned();
                builder.append_link(&mut header, &item.rel, &target)?;
            }
            EntryKind::File => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(item.size);
                let inner = File::open(&item.path)?;
                builder.append_data(&mut header, &item.rel, ProgressRead { inner, throttle: &mut throttle })?;
            }
        }
    }
    let encoder = builder.into_inner()?;
    encoder.finish_boxed()?;
    throttle.finish();
    Ok(warnings)
}

/// 「Mac 专用」：读 item 的 Finder 信息、扩展属性、资源分支，先写 `._名字` AppleDouble 条目，再写一个
/// 只作用于下一个条目（item 本身）的 PAX 扩展头。没有元数据就什么都不写。
/// 读元数据失败只记 warning 不中断——丢一个文件的标签不值得让整个包失败。
fn write_mac_metadata<W: Write>(builder: &mut tar::Builder<W>, item: &SourceItem, warnings: &mut Vec<Warning>) -> Result<()> {
    let meta = match apple_double::read(&item.path, warnings) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(Warning::with_detail(WarningKind::MacMetadataReadFailed, item.path.display().to_string(), e.to_string()));
            return Ok(());
        }
    };
    if meta.is_empty() {
        return Ok(());
    }
    let raw = apple_double::encode(&meta);
    // `._` 条目和 bsdtar 一样：普通文件、0644、时间跟原条目
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_mtime(item.mtime.unwrap_or(0).max(0) as u64);
    header.set_size(raw.len() as u64);
    builder.append_data(&mut header, sibling_apple_double_rel(&item.rel), raw.as_slice())?;

    let pax = pax_xattr_records(&meta);
    let mut header = tar::Header::new_ustar();
    // PAX 头自己的名字读取方都会忽略，用 Python tarfile 的固定写法
    header.set_path("././@PaxHeader")?;
    header.set_entry_type(tar::EntryType::XHeader);
    header.set_mode(0o644);
    header.set_mtime(item.mtime.unwrap_or(0).max(0) as u64);
    header.set_size(pax.len() as u64);
    header.set_cksum();
    builder.append(&header, pax.as_slice())?;
    Ok(())
}

/// tar 里 AppleDouble 放在同目录：`a/b/c.txt` → `a/b/._c.txt`，`c.txt` → `._c.txt`（不同于 zip 的 `__MACOSX/`）。
fn sibling_apple_double_rel(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/._{name}"),
        None => format!("._{rel}"),
    }
}

/// 把元数据编成 PAX 扩展头正文：每个属性一条 `LIBARCHIVE.xattr.<URL 编码名>=<base64>`，名字是合法 UTF-8 时
/// 再加一条 `SCHILY.xattr.<名>=<原字节>`（PAX 键必须是 UTF-8）。Finder 信息、资源分支按 bsdtar 的习惯也当
/// xattr 写，名字分别是 `com.apple.FinderInfo`、`com.apple.ResourceFork`。
fn pax_xattr_records(meta: &MacMetadata) -> Vec<u8> {
    let mut attrs: Vec<(&[u8], &[u8])> = meta.xattrs.iter().map(|(n, v)| (n.as_slice(), v.as_slice())).collect();
    if let Some(fi) = &meta.finder_info {
        attrs.push((b"com.apple.FinderInfo", fi));
    }
    if !meta.resource_fork.is_empty() {
        attrs.push((b"com.apple.ResourceFork", &meta.resource_fork));
    }
    attrs.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = Vec::new();
    for (name, value) in attrs {
        let key = format!("LIBARCHIVE.xattr.{}", url_encode(name));
        out.extend_from_slice(&pax_record(key.as_bytes(), base64_no_pad(value).as_bytes()));
        if let Ok(name) = std::str::from_utf8(name) {
            out.extend_from_slice(&pax_record(format!("SCHILY.xattr.{name}").as_bytes(), value));
        }
    }
    out
}

/// 一条 PAX 记录 `"<长度> <键>=<值>\n"`。长度是整条记录的字节数，连它自己的位数也算在内，
/// 所以先按不含长度的部分估位数，位数进位了再加一。
fn pax_record(key: &[u8], value: &[u8]) -> Vec<u8> {
    let body = key.len() + value.len() + 3; // 空格、等号、换行
    let mut len = body + body.to_string().len();
    if len.to_string().len() != body.to_string().len() {
        len += 1;
    }
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(len.to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(key);
    out.push(b'=');
    out.extend_from_slice(value);
    out.push(b'\n');
    debug_assert_eq!(out.len(), len);
    out
}

/// libarchive 写 `LIBARCHIVE.xattr.` 键时对属性名做的 URL 编码：控制字符、空格、非 ASCII、`%`、`=` 写成 `%XX`。
fn url_encode(name: &[u8]) -> String {
    let mut out = String::with_capacity(name.len());
    for &b in name {
        if b <= b' ' || b >= 0x7f || b == b'%' || b == b'=' {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// 标准字母表的 base64，不补 `=`（libarchive 写的就是这样，读的时候两种都认）。
fn base64_no_pad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &b)| acc | (b as u32) << (16 - 8 * i));
        for i in 0..chunk.len() + 1 {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
        }
    }
    out
}

/// 把 0–9 映射到 zstd 的 1–19。
fn zstd_level(level: u8) -> i32 {
    match level.clamp(1, 9) {
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 5,
        5 => 7,
        6 => 10,
        7 => 13,
        8 => 16,
        _ => 19,
    }
}

/// 能收尾的写入器：各压缩流的 finish 签名不同，这里统一成一个 trait 方便装箱。
trait FinishWrite: Write {
    /// 写出尾部并落盘。
    fn finish_boxed(self: Box<Self>) -> io::Result<()>;
}

impl FinishWrite for flate2::write::GzEncoder<BufWriter<File>> {
    fn finish_boxed(self: Box<Self>) -> io::Result<()> {
        let mut w = (*self).finish()?;
        w.flush()?;
        w.get_ref().sync_all()
    }
}

impl FinishWrite for liblzma::write::XzEncoder<BufWriter<File>> {
    fn finish_boxed(self: Box<Self>) -> io::Result<()> {
        let mut w = (*self).finish()?;
        w.flush()?;
        w.get_ref().sync_all()
    }
}

impl FinishWrite for zstd::stream::write::Encoder<'static, BufWriter<File>> {
    fn finish_boxed(self: Box<Self>) -> io::Result<()> {
        let mut w = (*self).finish()?;
        w.flush()?;
        w.get_ref().sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_record_length_counts_itself() {
        // 键值 2 字节 + 空格、等号、换行 = 5，再加 1 位长度 = 6
        assert_eq!(pax_record(b"a", b"b"), b"6 a=b\n");
        // 正文 97 字节 + 2 位 = 99；正文 98 字节 + 2 位 = 100 会进位成 3 位 → 101
        assert_eq!(pax_record(b"k", &[b'x'; 93]).len(), 99);
        let r = pax_record(b"k", &[b'x'; 94]);
        assert_eq!(r.len(), 101);
        assert!(r.starts_with(b"101 k="));
    }

    #[test]
    fn base64_matches_libarchive() {
        // 期望值取自 macOS bsdtar 对同样内容写出的 LIBARCHIVE.xattr 记录
        assert_eq!(base64_no_pad(b"v1"), "djE");
        assert_eq!(base64_no_pad(b"RSRCDATA"), "UlNSQ0RBVEE");
        assert_eq!(base64_no_pad(b"bp"), "YnA");
        assert_eq!(base64_no_pad(b"abc"), "YWJj");
        assert_eq!(base64_no_pad(b""), "");
    }

    #[test]
    fn url_encode_escapes_like_libarchive() {
        assert_eq!(url_encode(b"com.apple.metadata:_kMDItemUserTags"), "com.apple.metadata:_kMDItemUserTags");
        assert_eq!(url_encode(b"a b=c%\xff"), "a%20b%3Dc%25%FF");
    }

    #[test]
    fn sibling_rel() {
        assert_eq!(sibling_apple_double_rel("f.txt"), "._f.txt");
        assert_eq!(sibling_apple_double_rel("a/b/c.txt"), "a/b/._c.txt");
    }
}
