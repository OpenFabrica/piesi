//! tar.gz / tar.xz / tar.zst 写入器：tar crate 打包，外层压缩流（xz、zstd 多线程）。

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

use crate::archive::EntryKind;
use crate::create::{CreateFormat, CreateOptions, Sources};
use crate::progress::{OpContext, Throttle};
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

/// 写出 tar.*。
pub fn write_tar(dest: &Path, sources: &Sources, opts: &CreateOptions, ctx: &OpContext<'_>) -> Result<()> {
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

    for item in &sources.items {
        throttle.begin_entry(&item.rel)?;
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
    Ok(())
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
