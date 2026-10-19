//! tar 系（tar / tar.gz / tar.bz2 / tar.xz / tar.zst）与单文件压缩流（gz / bz2 / xz / zst）驱动。
//! tar 没有目录区：打开时顺序扫一遍只读头建索引，解压时再扫一遍只写选中的条目；
//! 压缩外壳全部流式解码，永不整包进内存。文件底部还放了三个驱动共用的小工具。

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::archive::{ArchiveReader, Entry, EntryId, EntryKind, EntrySink, ExtractReport, Format, IndexState, Method};
use crate::progress::{OpContext, Throttle};
use crate::{Error, Result};

/// 文件读缓冲大小：解码器一次喂 256 KB，减少系统调用。
const STREAM_BUF: usize = 256 * 1024;

/// tar 条目在驱动内部的细分类型：对外的 `EntryKind` 只有文件/目录/符号链接，
/// 硬链接和设备文件对外都算 File，但解压时要跳过并告警，所以这里单独记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TarKind {
    Regular,
    Dir,
    Symlink,
    HardLink,
    Special,
}

/// tar / gz / bz2 / xz / zst 读取器。
pub struct TarReader {
    path: PathBuf,
    format: Format,
    entries: Vec<Entry>,
    /// 与 `entries` 对齐的内部类型。
    kinds: Vec<TarKind>,
}

impl TarReader {
    /// 打开：tar 系顺序扫一遍建完整索引（只读头、跳过数据）；单文件流只有一个条目。
    pub fn open(path: &Path, format: Format) -> Result<Self> {
        let (entries, kinds) = if format.is_tar() {
            scan_tar(path, format)?
        } else if format.is_single_stream() {
            (vec![single_stream_entry(path, format)?], vec![TarKind::Regular])
        } else {
            return Err(Error::Unsupported(format!("tar 驱动不处理 {}", format.display_name())));
        };
        Ok(Self { path: path.to_path_buf(), format, entries, kinds })
    }

    /// 单文件流的解压：只有一个条目，直接流式解码喂给接收端。
    fn extract_single(&self, selected: &[bool], sink: &mut dyn EntrySink, throttle: &mut Throttle<'_>) -> Result<ExtractReport> {
        let mut report = ExtractReport::default();
        if selected.first().copied().unwrap_or(false) {
            let entry = &self.entries[0];
            throttle.begin_entry(&progress_name(entry))?;
            let mut stream = open_stream(&self.path, self.format)?;
            // 展开大小可能未知（entry.size 为 0），进度只能按已解码字节走
            let mut counting = CountingReader::new(stream.as_mut(), throttle);
            let r = sink.file(entry, &mut counting);
            counting.finish(r, Error::Corrupt)?;
            report.extracted += 1;
        }
        Ok(report)
    }

    /// tar 系的解压：再扫一遍，只把选中的条目交给接收端，处理完最后一个选中条目就停。
    fn extract_tar(&self, selected: &[bool], mut remaining: u64, sink: &mut dyn EntrySink, throttle: &mut Throttle<'_>) -> Result<ExtractReport> {
        let mut report = ExtractReport::default();
        let mut src = TarSource::open(&self.path, self.format)?;
        let entries = &self.entries;
        let kinds = &self.kinds;
        src.walk(&mut |item: TarItem<'_>| {
            // 选中的都处理完就提前结束，省得把后面的数据白白解一遍
            if remaining == 0 {
                return Ok(false);
            }
            let i = item.index;
            if i >= entries.len() {
                return Err(Error::Corrupt("压缩包内容与打开时建立的索引不一致".into()));
            }
            if !selected[i] {
                return Ok(true);
            }
            remaining -= 1;
            let entry = &entries[i];
            throttle.begin_entry(&progress_name(entry))?;
            match kinds[i] {
                TarKind::Dir => {
                    sink.dir(entry)?;
                    report.extracted += 1;
                }
                TarKind::Symlink => {
                    sink.symlink(entry, entry.link_target.as_deref().unwrap_or_default())?;
                    report.extracted += 1;
                }
                TarKind::HardLink => {
                    report.warnings.push(format!("{}：硬链接暂不支持，已跳过", progress_name(entry)));
                    report.skipped += 1;
                }
                TarKind::Special => {
                    report.warnings.push(format!("{}：设备文件或管道，已跳过", progress_name(entry)));
                    report.skipped += 1;
                }
                TarKind::Regular => {
                    let mut counting = CountingReader::new(item.data, &mut *throttle);
                    let r = sink.file(entry, &mut counting);
                    counting.finish(r, Error::Corrupt)?;
                    report.extracted += 1;
                }
            }
            Ok(true)
        })?;
        Ok(report)
    }
}

impl ArchiveReader for TarReader {
    fn format(&self) -> Format {
        self.format
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 打开时已经扫完整个包，所以索引总是完整的。
    fn index_state(&self) -> IndexState {
        IndexState::Complete
    }

    fn extract(&mut self, ids: Option<&[EntryId]>, sink: &mut dyn EntrySink, ctx: &OpContext<'_>) -> Result<ExtractReport> {
        let selected = selection_mask(ids, self.entries.len())?;
        let mut total_bytes = 0u64;
        let mut total_entries = 0u64;
        for (i, e) in self.entries.iter().enumerate() {
            if selected[i] {
                total_entries += 1;
                if self.kinds[i] == TarKind::Regular {
                    total_bytes += e.size;
                }
            }
        }
        let mut throttle = Throttle::new(ctx, Some(total_bytes), Some(total_entries));
        let report = if self.format.is_single_stream() {
            self.extract_single(&selected, sink, &mut throttle)?
        } else {
            self.extract_tar(&selected, total_entries, sink, &mut throttle)?
        };
        throttle.finish();
        Ok(report)
    }

    fn read_entry(&mut self, id: EntryId, out: &mut dyn Write, ctx: &OpContext<'_>) -> Result<()> {
        self.extract(Some(&[id]), &mut WriterSink(out), ctx).map(|_| ())
    }
}

/// 按格式把文件包成一个顺序 Read：压缩外壳用对应的多流解码器，裸 tar 直接给带缓冲的文件。
fn open_stream(path: &Path, format: Format) -> Result<Box<dyn Read>> {
    let file = BufReader::with_capacity(STREAM_BUF, File::open(path)?);
    Ok(match format {
        Format::Tar => Box::new(file),
        // 多成员 gzip（pigz、--rsyncable 产物）要用 MultiGzDecoder 才能连续解到底
        Format::TarGz | Format::Gz => Box::new(flate2::bufread::MultiGzDecoder::new(file)),
        Format::TarBz2 | Format::Bz2 => Box::new(bzip2::bufread::MultiBzDecoder::new(file)),
        // 带内存上限的自动解码器（.xz 与 .lzma 都认，多流连续解）：头里声明的字典超过上限就报错，
        // 不然 42 字节的恶意输入就能让解码器分配 4 GB（模糊测试抓到的）
        Format::TarXz | Format::Xz => {
            let stream = liblzma::stream::Stream::new_auto_decoder(crate::sys::decode_memlimit(), liblzma::stream::CONCATENATED)
                .map_err(|e| Error::Io(e.into()))?;
            Box::new(liblzma::bufread::XzDecoder::new_stream(file, stream))
        }
        Format::TarZst | Format::Zst => Box::new(zstd::stream::read::Decoder::with_buffer(file)?),
        other => return Err(Error::Unsupported(other.display_name().to_owned())),
    })
}

/// 遍历回调看到的条目视图：头信息、名字、链接目标和数据流，屏蔽 tar crate 的泛型参数。
struct TarItem<'a> {
    index: usize,
    header: &'a tar::Header,
    path: Vec<u8>,
    link: Option<Vec<u8>>,
    size: u64,
    data: &'a mut dyn Read,
}

/// tar 数据源：裸 tar 用可 seek 的文件（跳过未选条目只需 seek），压缩外壳只能顺序读过去。
enum TarSource {
    Seekable(tar::Archive<BufReader<File>>),
    Stream(tar::Archive<Box<dyn Read>>),
}

impl TarSource {
    /// 打开数据源；每次遍历都要从头开始，所以调用方每次都新建一个。
    fn open(path: &Path, format: Format) -> Result<Self> {
        Ok(if format == Format::Tar {
            let file = BufReader::with_capacity(STREAM_BUF, File::open(path)?);
            Self::Seekable(tar::Archive::new(file))
        } else {
            Self::Stream(tar::Archive::new(open_stream(path, format)?))
        })
    }

    /// 顺序遍历每个条目；回调返回 Ok(false) 提前停止。
    fn walk(&mut self, visit: &mut dyn FnMut(TarItem<'_>) -> Result<bool>) -> Result<()> {
        match self {
            Self::Seekable(a) => walk_entries(a.entries_with_seek()?, visit),
            Self::Stream(a) => walk_entries(a.entries()?, visit),
        }
    }
}

/// 把 tar crate 的条目迭代器翻译成 `TarItem` 回调；GNU/pax 长名由 tar crate 处理。
fn walk_entries<R: Read>(entries: tar::Entries<'_, R>, visit: &mut dyn FnMut(TarItem<'_>) -> Result<bool>) -> Result<()> {
    for (index, entry) in entries.enumerate() {
        let mut entry = entry.map_err(|e| Error::Corrupt(format!("tar 第 {} 个条目读取失败：{e}", index + 1)))?;
        let path = entry.path_bytes().into_owned();
        let link = entry.link_name_bytes().map(|l| l.into_owned());
        let size = entry.size();
        // Header 只是 512 字节，克隆一份好让下面同时可变借用数据流
        let header = entry.header().clone();
        let item = TarItem { index, header: &header, path, link, size, data: &mut entry };
        if !visit(item)? {
            break;
        }
    }
    Ok(())
}

/// 打开时扫描整个 tar，只读头建索引，数据由 tar crate 跳过。
fn scan_tar(path: &Path, format: Format) -> Result<(Vec<Entry>, Vec<TarKind>)> {
    let mut src = TarSource::open(path, format)?;
    let mut entries = Vec::new();
    let mut kinds = Vec::new();
    src.walk(&mut |item: TarItem<'_>| {
        let (entry, kind) = entry_from_item(&item, format);
        entries.push(entry);
        kinds.push(kind);
        Ok(true)
    })?;
    Ok((entries, kinds))
}

/// 把 tar 头翻译成统一条目：raw_name 保留原字节（可能是任意代码页），不标记 UTF-8。
fn entry_from_item(item: &TarItem<'_>, format: Format) -> (Entry, TarKind) {
    let h = item.header;
    let (kind, tar_kind, type_bits) = match h.entry_type() {
        tar::EntryType::Directory => (EntryKind::Dir, TarKind::Dir, 0o040000),
        tar::EntryType::Symlink => (EntryKind::Symlink, TarKind::Symlink, 0o120000),
        tar::EntryType::Link => (EntryKind::File, TarKind::HardLink, 0o100000),
        tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => (EntryKind::File, TarKind::Regular, 0o100000),
        // 设备、fifo 等：列出来让用户看见，解压时跳过并告警
        _ => (EntryKind::File, TarKind::Special, 0),
    };
    let size = if tar_kind == TarKind::Regular { item.size } else { 0 };
    let mut entry = Entry::new(item.index as EntryId, item.path.clone(), kind, size);
    entry.mtime = h.mtime().ok().and_then(|t| i64::try_from(t).ok());
    // tar 的 mode 字段只有权限位，类型位按条目类型补上
    entry.unix_mode = h.mode().ok().map(|m| (m & 0o7777) | type_bits);
    // 裸 tar 里条目不压缩，包内占用就是大小；套了压缩外壳就算不出来
    entry.packed_size = (format == Format::Tar).then_some(size);
    entry.method = Method::Store;
    if matches!(tar_kind, TarKind::Symlink | TarKind::HardLink) {
        entry.link_target = item.link.clone();
    }
    (entry, tar_kind)
}

/// gzip 头里能拿到的两样东西：原文件名（FNAME）和修改时间（MTIME）。
struct GzipHead {
    name: Option<Vec<u8>>,
    mtime: Option<i64>,
}

/// 解析 gzip 成员头。FNAME 按规范是 Latin-1，这里原样返回字节交给编码层。
fn parse_gzip_header(head: &[u8]) -> GzipHead {
    let mut out = GzipHead { name: None, mtime: None };
    if head.len() < 10 || head[0] != 0x1f || head[1] != 0x8b {
        return out;
    }
    let flags = head[3];
    let mtime = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    if mtime != 0 {
        out.mtime = Some(i64::from(mtime));
    }
    let mut pos = 10usize;
    // FEXTRA：2 字节长度 + 数据
    if flags & 0x04 != 0 {
        let Some(len) = head.get(pos..pos + 2) else { return out };
        pos += 2 + usize::from(u16::from_le_bytes([len[0], len[1]]));
    }
    // FNAME：NUL 结尾
    if flags & 0x08 != 0 {
        if let Some(rest) = head.get(pos..) {
            if let Some(end) = rest.iter().position(|&b| b == 0) {
                out.name = Some(rest[..end].to_vec());
            }
        }
    }
    out
}

/// 单文件流没有记录原名时，用压缩包文件名去掉扩展名（`a.txt.gz` → `a.txt`）。
fn stem_name(path: &Path, format: Format) -> Vec<u8> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "data".to_owned());
    let exts: &[&str] = match format {
        Format::Gz => &[".gz", ".z"],
        Format::Bz2 => &[".bz2", ".bz"],
        Format::Xz => &[".xz", ".lzma"],
        Format::Zst => &[".zst"],
        _ => &[],
    };
    // 后缀都是 ASCII，按小写比较后用同样的字节长度截断不会切坏 UTF-8
    let lower = name.to_ascii_lowercase();
    let stripped = exts
        .iter()
        .find(|x| lower.ends_with(*x))
        .map(|x| name[..name.len() - x.len()].to_owned())
        .filter(|s| !s.is_empty());
    stripped.unwrap_or(name).into_bytes()
}

/// 单文件压缩流的唯一条目。展开大小只有 gz（尾部 ISIZE）和 zst（帧头）能便宜地拿到，
/// bz2 / xz 填 0，UI 对单文件流的 size == 0 要显示「未知」而不是 0 字节。
fn single_stream_entry(path: &Path, format: Format) -> Result<Entry> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut head = vec![0u8; 4096];
    let n = read_up_to(&mut file, &mut head)?;
    head.truncate(n);

    let mut name = None;
    let mut mtime = None;
    let mut size = 0u64;
    match format {
        Format::Gz => {
            let gh = parse_gzip_header(&head);
            name = gh.name;
            mtime = gh.mtime;
            // ISIZE 是展开大小对 2^32 取模，多成员文件只反映最后一个成员：当作提示值用
            if len >= 18 {
                file.seek(SeekFrom::End(-4))?;
                let mut tail = [0u8; 4];
                file.read_exact(&mut tail)?;
                size = u64::from(u32::from_le_bytes(tail));
            }
        }
        Format::Zst => {
            size = zstd::zstd_safe::get_frame_content_size(&head).ok().flatten().unwrap_or(0);
        }
        _ => {}
    }

    let (raw_name, is_utf8) = match name {
        Some(n) if !n.is_empty() => (n, false),
        _ => (stem_name(path, format), true),
    };
    let mut entry = Entry::new(0, raw_name, EntryKind::File, size);
    entry.name_is_utf8 = is_utf8;
    entry.mtime = mtime;
    entry.packed_size = Some(len);
    entry.method = match format {
        Format::Gz => Method::Deflate,
        Format::Bz2 => Method::Bzip2,
        Format::Xz => Method::Xz,
        Format::Zst => Method::Zstd,
        _ => Method::Store,
    };
    Ok(entry)
}

/// 尽量把 `buf` 读满，文件比缓冲短时返回实际读到的长度。
fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

// ---------- 以下是 tar / 7z / rar 三个驱动共用的小工具 ----------
// fmt/mod.rs 不在本次改动范围内，暂时放在这里，以后可挪到 fmt/util.rs。

/// 把 `ids` 变成按条目下标索引的选中位图；None 表示全选；越界 id 报 NoSuchEntry。
pub(crate) fn selection_mask(ids: Option<&[EntryId]>, count: usize) -> Result<Vec<bool>> {
    match ids {
        None => Ok(vec![true; count]),
        Some(ids) => {
            let mut mask = vec![false; count];
            for &id in ids {
                let i = id as usize;
                if i >= count {
                    return Err(Error::NoSuchEntry(id));
                }
                mask[i] = true;
            }
            Ok(mask)
        }
    }
}

/// 进度条显示用的名字：原始字节按 UTF-8 宽松解码。真正的编码识别在 encoding 模块，这里只给进度看。
pub(crate) fn progress_name(entry: &Entry) -> String {
    String::from_utf8_lossy(&entry.raw_name).into_owned()
}

/// 包装条目数据流：每读一块就累加进度并检查取消，并分别记下「底层解码出错」和「被取消」，
/// 这样 sink 返回错误后驱动能分清是包坏了 / 密码错了，还是落盘失败。
pub(crate) struct CountingReader<'t, 'a, R: Read> {
    inner: R,
    throttle: &'t mut Throttle<'a>,
    cancelled: bool,
    inner_error: Option<String>,
}

impl<'t, 'a, R: Read> CountingReader<'t, 'a, R> {
    /// 包装一个数据流，进度记到 `throttle` 上。
    pub(crate) fn new(inner: R, throttle: &'t mut Throttle<'a>) -> Self {
        Self { inner, throttle, cancelled: false, inner_error: None }
    }

    /// sink 处理完后调用：把 sink 的结果与本读取器记下的状态合并成最终结果。
    /// `on_inner` 把底层解码错误信息映射成驱动想报的错误（损坏 / 密码错）。
    pub(crate) fn finish<T>(&self, r: Result<T>, on_inner: impl FnOnce(String) -> Error) -> Result<T> {
        if self.cancelled {
            return Err(Error::Cancelled);
        }
        if let Some(msg) = &self.inner_error {
            return Err(on_inner(msg.clone()));
        }
        r
    }

    /// 把剩余数据读完丢掉：sink 没读完整个条目时，必须吃掉剩余字节才能对齐到下一个条目。
    pub(crate) fn drain(&mut self) -> io::Result<u64> {
        io::copy(self, &mut io::sink())
    }
}

impl<R: Read> Read for CountingReader<'_, '_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancelled || self.inner_error.is_some() {
            return Err(io::Error::other("数据流已失败"));
        }
        let n = match self.inner.read(buf) {
            Ok(n) => n,
            // Interrupted 由调用方重试，不算失败
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(e),
            Err(e) => {
                self.inner_error = Some(e.to_string());
                return Err(e);
            }
        };
        if let Err(e) = self.throttle.add_bytes(n as u64) {
            self.cancelled = true;
            return Err(io::Error::other(e.to_string()));
        }
        Ok(n)
    }
}

/// 把单个条目内容写进 Write 的接收端：`read_entry` 用它复用 `extract` 的全部逻辑。
/// 符号链接写出的是目标路径，目录什么都不写。
pub(crate) struct WriterSink<'w>(pub(crate) &'w mut dyn Write);

impl EntrySink for WriterSink<'_> {
    fn dir(&mut self, _entry: &Entry) -> Result<()> {
        Ok(())
    }

    fn file(&mut self, _entry: &Entry, data: &mut dyn Read) -> Result<u64> {
        Ok(io::copy(data, self.0)?)
    }

    fn symlink(&mut self, _entry: &Entry, target: &[u8]) -> Result<()> {
        self.0.write_all(target)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gzip 头解析：带 FNAME 与 MTIME 的头能取出名字和时间，FEXTRA 要跳过。
    #[test]
    fn gzip_header_name_and_mtime() {
        let mut head = vec![0x1f, 0x8b, 8, 0x0c, 0x78, 0x56, 0x34, 0x12, 0, 3];
        head.extend_from_slice(&[2, 0, 0xaa, 0xbb]); // FEXTRA
        head.extend_from_slice(b"a.txt\0");
        let gh = parse_gzip_header(&head);
        assert_eq!(gh.name.as_deref(), Some(&b"a.txt"[..]));
        assert_eq!(gh.mtime, Some(0x1234_5678));
        assert!(parse_gzip_header(b"xx").name.is_none());
    }

    /// 去扩展名：大小写不敏感，去空就保留原名。
    #[test]
    fn stem_name_strips_extension() {
        assert_eq!(stem_name(Path::new("/x/a.txt.GZ"), Format::Gz), b"a.txt");
        assert_eq!(stem_name(Path::new("/x/noext"), Format::Xz), b"noext");
        assert_eq!(stem_name(Path::new("/x/.gz"), Format::Gz), b".gz");
    }

    /// 选中位图：None 全选，越界报 NoSuchEntry。
    #[test]
    fn selection_mask_bounds() {
        assert_eq!(selection_mask(None, 2).unwrap(), vec![true, true]);
        assert_eq!(selection_mask(Some(&[1]), 2).unwrap(), vec![false, true]);
        assert!(matches!(selection_mask(Some(&[2]), 2), Err(Error::NoSuchEntry(2))));
    }
}
