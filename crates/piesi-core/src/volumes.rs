//! 分卷支持：把 `name.7z.001`、`name.zip.001` 这类字节分卷串成一个连续的 Read + Seek。
//! 7-Zip 风格分卷就是把输出流按固定大小切开，所以串联即可；PKWARE 的 `.z01` 另由 zip 驱动处理。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 多个文件首尾相接后的逻辑流。
pub struct ChainedReader {
    parts: Vec<Part>,
    /// 各段起始偏移的前缀和，`offsets[i]` 是第 i 段在逻辑流里的起点
    offsets: Vec<u64>,
    total: u64,
    pos: u64,
    current: usize,
}

struct Part {
    file: File,
}

impl ChainedReader {
    /// 打开一组分卷，按给定顺序串联。
    pub fn open(paths: &[PathBuf]) -> io::Result<Self> {
        let mut parts = Vec::with_capacity(paths.len());
        let mut offsets = Vec::with_capacity(paths.len());
        let mut total = 0u64;
        for p in paths {
            let file = File::open(p)?;
            let len = file.metadata()?.len();
            offsets.push(total);
            total += len;
            parts.push(Part { file });
        }
        if parts.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "没有分卷文件"));
        }
        let mut r = Self { parts, offsets, total, pos: 0, current: 0 };
        r.seek(SeekFrom::Start(0))?;
        Ok(r)
    }

    /// 逻辑流总长度。
    pub fn len(&self) -> u64 {
        self.total
    }

    /// 是否为空流。
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// 各分段在逻辑流里的起始偏移（前缀和），多盘 zip 换算 (盘号, 盘内偏移) 时用。
    pub fn part_offsets(&self) -> &[u64] {
        &self.offsets
    }

    /// 根据逻辑偏移定位到对应分段并 seek 文件。
    fn locate(&mut self, pos: u64) -> io::Result<()> {
        let pos = pos.min(self.total);
        // 二分找到包含 pos 的段；pos == total 时停在最后一段末尾
        let idx = match self.offsets.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let idx = idx.min(self.parts.len() - 1);
        let inner = pos - self.offsets[idx];
        self.parts[idx].file.seek(SeekFrom::Start(inner))?;
        self.current = idx;
        self.pos = pos;
        Ok(())
    }
}

impl Read for ChainedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.pos >= self.total {
                return Ok(0);
            }
            let n = self.parts[self.current].file.read(buf)?;
            if n > 0 {
                self.pos += n as u64;
                return Ok(n);
            }
            // 当前段读完，切到下一段开头
            if self.current + 1 >= self.parts.len() {
                return Ok(0);
            }
            self.current += 1;
            self.parts[self.current].file.seek(SeekFrom::Start(0))?;
        }
    }
}

impl Seek for ChainedReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.total as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek 到负偏移"));
        }
        self.locate(target as u64)?;
        Ok(self.pos)
    }
}

/// 给定首卷路径，找齐同目录下的全部分卷（`.001`、`.002`…），按序号排序。
/// 传入的不是 `.001` 时原样返回单元素列表。
pub fn collect_volumes(first: &Path) -> io::Result<Vec<PathBuf>> {
    let name = first.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let Some(stem) = name.strip_suffix(".001") else {
        return Ok(vec![first.to_path_buf()]);
    };
    let dir = first.parent().unwrap_or_else(|| Path::new("."));
    let mut vols = Vec::new();
    for i in 1u32.. {
        let p = dir.join(format!("{stem}.{i:03}"));
        if !p.is_file() {
            break;
        }
        vols.push(p);
    }
    if vols.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "找不到分卷首段"));
    }
    Ok(vols)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 三段串联后顺序读与随机 seek 都应与整体字节一致。
    #[test]
    fn chained_read_and_seek() {
        let dir = std::env::temp_dir().join(format!("piesi-vol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mut paths = Vec::new();
        for (i, chunk) in data.chunks(400).enumerate() {
            let p = dir.join(format!("a.bin.{:03}", i + 1));
            File::create(&p).unwrap().write_all(chunk).unwrap();
            paths.push(p);
        }
        let vols = collect_volumes(&paths[0]).unwrap();
        assert_eq!(vols.len(), 3);
        let mut r = ChainedReader::open(&vols).unwrap();
        assert_eq!(r.len(), 1000);
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        assert_eq!(all, data);
        r.seek(SeekFrom::Start(398)).unwrap();
        let mut buf = [0u8; 5];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, &data[398..403]);
        r.seek(SeekFrom::End(-3)).unwrap();
        let mut tail = Vec::new();
        r.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, &data[997..]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
