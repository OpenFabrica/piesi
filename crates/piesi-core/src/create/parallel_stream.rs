//! GZip / BZip2 按独立成员并行编码；使用标准多成员流，现有读取器与系统工具可连续解码。
use std::fs::File;
use std::io::{self, BufWriter, Write};

use rayon::prelude::*;

use super::compression::FinishWrite;
use super::CreateFormat;

const CHUNK_BYTES: usize = 4 << 20;

/// 一个有界输入批次共用一个线程池，编码结果按原序写出。
pub(super) struct ParallelStream {
    out: BufWriter<File>,
    pool: rayon::ThreadPool,
    format: CreateFormat,
    level: u32,
    current: Vec<u8>,
    chunks: Vec<Vec<u8>>,
    wrote_member: bool,
}

impl ParallelStream {
    /// 线程数已由调用方按 CPU 与内存预算计算。
    pub(super) fn new(out: BufWriter<File>, format: CreateFormat, level: u8, threads: usize) -> io::Result<Self> {
        Ok(Self {
            out,
            pool: rayon::ThreadPoolBuilder::new().num_threads(threads).build().map_err(io::Error::other)?,
            format,
            level: level.clamp(1, 9) as u32,
            current: Vec::with_capacity(CHUNK_BYTES),
            chunks: Vec::new(),
            wrote_member: false,
        })
    }

    /// 压缩完整批次；小尾块在 flush / finish 时加入。
    fn write_batch(&mut self) -> io::Result<()> {
        if self.chunks.is_empty() {
            return Ok(());
        }
        let format = self.format;
        let level = self.level;
        let results: io::Result<Vec<_>> = self.pool.install(|| {
            self.chunks
                .par_iter()
                .map(|raw| {
                    if matches!(format, CreateFormat::Gz | CreateFormat::TarGz) {
                        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
                        encoder.write_all(raw)?;
                        encoder.finish()
                    } else {
                        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
                        encoder.write_all(raw)?;
                        encoder.finish()
                    }
                })
                .collect()
        });
        for packed in results? {
            self.out.write_all(&packed)?;
        }
        self.chunks.clear();
        self.wrote_member = true;
        Ok(())
    }
}

impl Write for ParallelStream {
    /// 累积到 4 MiB 再提交；输入批次不超过线程数，避免一次缓存整个源文件。
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        let len = buf.len();
        while !buf.is_empty() {
            let count = buf.len().min(CHUNK_BYTES - self.current.len());
            self.current.extend_from_slice(&buf[..count]);
            buf = &buf[count..];
            if self.current.len() == CHUNK_BYTES {
                self.chunks.push(std::mem::replace(&mut self.current, Vec::with_capacity(CHUNK_BYTES)));
                if self.chunks.len() == self.pool.current_num_threads() {
                    self.write_batch()?;
                }
            }
        }
        Ok(len)
    }

    /// 把尚未编码的尾块也写入，遵守 Write::flush 的可观察语义。
    fn flush(&mut self) -> io::Result<()> {
        if !self.current.is_empty() {
            self.chunks.push(std::mem::take(&mut self.current));
        }
        self.write_batch()?;
        self.out.flush()
    }
}

impl FinishWrite for ParallelStream {
    /// 空文件同样输出一个合法成员，然后同步落盘。
    fn finish_boxed(mut self: Box<Self>) -> io::Result<()> {
        self.flush()?;
        if !self.wrote_member {
            self.chunks.push(Vec::new());
            self.write_batch()?;
            self.out.flush()?;
        }
        self.out.get_ref().sync_all()
    }
}
