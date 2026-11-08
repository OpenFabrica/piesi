//! TAR 与单文件压缩共用的流编码器，统一收尾并落盘。
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use super::{CreateFormat, CreateOptions};
use crate::{Error, Result};

/// 包装外层压缩流；TAR 本身直接写入缓冲文件。
pub(super) fn encoder(dest: &Path, opts: &CreateOptions, source_bytes: u64) -> Result<Box<dyn FinishWrite>> {
    let jobs = source_bytes.div_ceil(4 << 20).max(1);
    let cpu_threads = crate::sys::compression_threads(opts.threads, 64 << 20);
    // 4–9 的 XZ 保留已有线程与块参数；仅在内存估算超预算时降低并发。
    let keep_xz = matches!(opts.format, CreateFormat::Xz | CreateFormat::TarXz) && opts.level >= 4;
    let threads = if keep_xz { cpu_threads } else { cpu_threads.min(jobs as usize) } as u32;
    let file = BufWriter::with_capacity(1 << 20, File::create(dest)?);
    Ok(match opts.format {
        CreateFormat::Tar => Box::new(file),
        CreateFormat::TarGz | CreateFormat::Gz | CreateFormat::TarBz2 | CreateFormat::Bz2 => Box::new(super::parallel_stream::ParallelStream::new(file, opts.format, opts.level, threads as usize)?),
        CreateFormat::TarXz | CreateFormat::Xz => {
            let mut config = liblzma::stream::MtStreamBuilder::new();
            config.preset(opts.level.clamp(0, 9) as u32).threads(threads);
            // 快速级别 1–3 按输入量拆块；4–9 保留完整字典窗口，避免为了线程数牺牲跨块压缩收益。
            if opts.level <= 3 {
                config.block_size((source_bytes / u64::from(threads)).clamp(4 << 20, 64 << 20));
            }
            let budget = crate::sys::compression_budget();
            let mut xz_threads = threads;
            while xz_threads > 1 && config.memusage() > budget {
                xz_threads -= 1;
                config.threads(xz_threads);
            }
            let stream = config.encoder().map_err(Error::other)?;
            Box::new(liblzma::write::XzEncoder::new_stream(file, stream))
        }
        CreateFormat::TarZst | CreateFormat::Zst => {
            let mut enc = zstd::stream::write::Encoder::new(file, zstd_level(opts.level)).map_err(Error::other)?;
            enc.multithread(threads).map_err(Error::other)?;
            Box::new(enc)
        }
        other => return Err(Error::Unsupported(crate::ErrorDetail::with(crate::DetailKind::NotTarFormat, format!("{other:?}")))),
    })
}

/// 把 0–9 映射到 zstd 的 1–19。
fn zstd_level(level: u8) -> i32 {
    [1, 1, 2, 3, 5, 7, 10, 13, 16, 19][level.min(9) as usize]
}

/// 各压缩流的 finish 签名不同，统一成可装箱的收尾接口。
pub(super) trait FinishWrite: Write {
    /// 写完编码尾部、刷新缓冲并同步输出。
    fn finish_boxed(self: Box<Self>) -> io::Result<()>;
}

impl FinishWrite for BufWriter<File> {
    fn finish_boxed(mut self: Box<Self>) -> io::Result<()> {
        self.flush()?;
        self.get_ref().sync_all()
    }
}

/// 压缩编码器收尾后都返回同一种缓冲文件。
macro_rules! finish_encoder {
    ($encoder:ty) => {
        impl FinishWrite for $encoder {
            fn finish_boxed(self: Box<Self>) -> io::Result<()> {
                let mut file = (*self).finish()?;
                file.flush()?;
                file.get_ref().sync_all()
            }
        }
    };
}

finish_encoder!(liblzma::write::XzEncoder<BufWriter<File>>);
finish_encoder!(zstd::stream::write::Encoder<'static, BufWriter<File>>);
