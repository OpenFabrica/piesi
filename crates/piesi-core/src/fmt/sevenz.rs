//! 7z 驱动：容器解析与解码交给 sevenz-rust2（纯 Rust，含 BCJ/BCJ2/ARM64/Delta 过滤器与 AES-256），
//! 本文件把它的 Archive/Block 模型映射成统一的 Entry，并实现按 folder（块）顺序解压与 solid 跳过。
//! `.7z.001` 分卷用 volumes 模块串成一个 Read + Seek 交给它。

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use sevenz_rust2::{Archive as SzArchive, Block, BlockDecoder, EncoderMethod, Error as SzError, NtTime, Password};

use super::tar::{progress_name, selection_mask, CountingReader, WriterSink};
use crate::archive::{ArchiveReader, Entry, EntryId, EntryKind, EntrySink, ExtractReport, Format, Method, PasswordNeed};
use crate::progress::{OpContext, Throttle};
use crate::volumes::{collect_volumes, ChainedReader};
use crate::{Error, Result};

/// 符号链接内容（目标路径）的上限：超过它就不是正常的链接，按损坏处理。
const MAX_SYMLINK_TARGET: u64 = 64 * 1024;

/// 7z 的 Windows 属性里带 Unix mode 的标记位（高 16 位是 st_mode）。
const FILE_ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;

/// 7z 读取器。
pub struct SevenZReader {
    source: ChainedReader,
    /// 解析好的头；头加密且还没拿到密码时为 None。
    archive: Option<SzArchive>,
    password: Password,
    header_encrypted: bool,
    entries: Vec<Entry>,
    /// 每个块是否带 AES 编码器，与 `archive.blocks` 对齐；判断「密码错」还是「包坏了」要用。
    block_encrypted: Vec<bool>,
    thread_count: u32,
}

impl SevenZReader {
    /// 打开并读目录区。`.7z.001` 自动找齐同目录分卷串联；头加密的包打开成功但 entries 为空。
    pub fn open(path: &Path) -> Result<Self> {
        let volumes = collect_volumes(path)?;
        let source = ChainedReader::open(&volumes)?;
        let threads = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1);
        let mut reader = Self {
            source,
            archive: None,
            password: Password::empty(),
            header_encrypted: false,
            entries: Vec::new(),
            block_encrypted: Vec::new(),
            thread_count: threads,
        };
        match SzArchive::read(&mut reader.source, &reader.password) {
            Ok(archive) => reader.install(archive),
            // 头被 AES 加密：连文件名都没有，等 set_password 后重新读头
            Err(SzError::PasswordRequired) => reader.header_encrypted = true,
            Err(e) => return Err(map_header_error(e)),
        }
        Ok(reader)
    }

    /// 把解析好的头翻译成统一条目列表。
    fn install(&mut self, archive: SzArchive) {
        let block_info: Vec<(Method, bool)> = archive.blocks.iter().map(classify_block).collect();
        let mut files_per_block = vec![0usize; archive.blocks.len()];
        for b in archive.stream_map.file_block_index.iter().flatten() {
            if let Some(n) = files_per_block.get_mut(*b) {
                *n += 1;
            }
        }

        let mut entries = Vec::with_capacity(archive.files.len());
        for (i, f) in archive.files.iter().enumerate() {
            // 7-Zip 自己存的是 '/'，但保险起见把 Windows 风格的 '\' 也统一掉
            let raw_name = f.name.replace('\\', "/").into_bytes();
            let (kind, unix_mode) = kind_and_mode(f);
            let mut e = Entry::new(i as EntryId, raw_name, kind, f.size);
            e.name_is_utf8 = true;
            e.mtime = f.has_last_modified_date.then(|| nt_to_unix(f.last_modified_date));
            e.crc32 = f.has_crc.then_some(f.crc as u32);
            e.unix_mode = unix_mode;
            match archive.stream_map.file_block_index.get(i).copied().flatten() {
                Some(b) => {
                    e.solid_group = Some(b as u32);
                    if let Some((method, encrypted)) = block_info.get(b) {
                        e.method = method.clone();
                        e.encrypted = *encrypted;
                    }
                    // 一个块只装一个文件时，块的打包大小就是它的；solid 块里分不出各文件的份额
                    if files_per_block.get(b) == Some(&1) {
                        e.packed_size = block_packed_size(&archive, b);
                    }
                }
                None => {
                    e.method = Method::Store;
                    e.packed_size = Some(0);
                }
            }
            entries.push(e);
        }
        self.block_encrypted = block_info.into_iter().map(|(_, enc)| enc).collect();
        self.entries = entries;
        self.archive = Some(archive);
    }

    /// 按块解压。未选中的块整个跳过不解码；选中的块顺序解到最后一个选中条目为止。
    fn extract_blocks(&mut self, selected: &[bool], sink: &mut dyn EntrySink, ctx: &OpContext<'_>) -> Result<ExtractReport> {
        let archive = self.archive.as_ref().ok_or(Error::PasswordRequired)?;
        let block_first_file = &archive.stream_map.block_first_file_index;
        let file_block = &archive.stream_map.file_block_index;

        // 每块要解到哪个条目为止（块内最后一个被选中的文件下标）
        let mut last_needed: Vec<Option<usize>> = vec![None; archive.blocks.len()];
        let mut total_entries = 0u64;
        for (i, &sel) in selected.iter().enumerate() {
            if !sel {
                continue;
            }
            total_entries += 1;
            if let Some(b) = file_block.get(i).copied().flatten() {
                // 头里的块号是不可信数据，越界就当包坏了
                *last_needed.get_mut(b).ok_or_else(|| Error::Corrupt("7z：条目引用了不存在的块".into()))? = Some(i);
            }
        }
        // 进度总量 = 真正要解码的字节：每块从块首到最后一个选中条目，中间跳过的也算
        // （跳过条目解码丢弃时同样走 CountingReader 计入 done_bytes，进度条不会长时间不动）；
        // 最后一个选中条目之后的不解，所以不计入
        let mut total_bytes = 0u64;
        for (b, last) in last_needed.iter().enumerate() {
            if let Some(last) = last {
                let first = *block_first_file.get(b).ok_or_else(|| Error::Corrupt("7z：块与文件表对不上".into()))?;
                total_bytes += archive.files.get(first..=*last).unwrap_or_default().iter().map(|f| f.size).sum::<u64>();
            }
        }
        let mut throttle = Throttle::new(ctx, Some(total_bytes), Some(total_entries));
        let mut report = ExtractReport::default();

        // 1) 没有数据流的条目（目录、空文件）先交给接收端，目录先建好再写文件
        for (i, entry) in self.entries.iter().enumerate() {
            if !selected[i] || file_block.get(i).copied().flatten().is_some() {
                continue;
            }
            throttle.begin_entry(&progress_name(entry))?;
            match entry.kind {
                EntryKind::Dir => sink.dir(entry)?,
                EntryKind::Symlink => sink.symlink(entry, b"")?,
                EntryKind::File => {
                    sink.file(entry, &mut std::io::empty())?;
                }
            }
            report.extracted += 1;
        }

        // 2) 按块顺序解码
        for (b, last) in last_needed.iter().enumerate() {
            let Some(last) = *last else { continue };
            let encrypted = self.block_encrypted.get(b).copied().unwrap_or(false);
            if encrypted && self.password.is_empty() {
                return Err(Error::PasswordRequired);
            }
            let mut next_index = *block_first_file.get(b).ok_or_else(|| Error::Corrupt("7z：块与文件表对不上".into()))?;
            // sink 的错误不能穿过 sevenz 的闭包签名，先存在这里，返回 Ok(false) 让它停下
            let mut failure: Option<Error> = None;
            let entries = &self.entries;
            let decoder = BlockDecoder::new(self.thread_count, b, archive, &self.password, &mut self.source);
            let outcome = decoder.for_each_entries(&mut |sz_entry, reader| {
                let i = next_index;
                next_index += 1;
                // 最后一个选中条目之后的不用再解
                if i > last {
                    return Ok(false);
                }
                let Some(entry) = entries.get(i) else {
                    failure = Some(Error::Corrupt("块与文件表对不上".into()));
                    return Ok(false);
                };
                // 名字核对：stream_map 与 files 不一致说明包坏了，别把数据写错地方
                if sz_entry.name.replace('\\', "/").as_bytes() != entry.raw_name.as_slice() {
                    failure = Some(Error::Corrupt(format!("块内条目与目录不一致：{}", sz_entry.name)));
                    return Ok(false);
                }
                if selected[i] {
                    if let Err(e) = throttle.begin_entry(&progress_name(entry)) {
                        failure = Some(e);
                        return Ok(false);
                    }
                }
                let mut counting = CountingReader::new(reader, &mut throttle);
                let result = if selected[i] {
                    deliver_entry(entry, &mut counting, sink, &mut report)
                } else {
                    // solid 块里跳过的条目也必须解出来丢掉：后面条目的 LZMA 状态依赖前面的输出，
                    // 这是 7z solid 格式的代价（只花时间，内存仍只占字典）
                    counting.drain().map(|_| ()).map_err(Error::from)
                };
                let on_inner = |msg: String| if encrypted { Error::WrongPassword } else { Error::Corrupt(msg) };
                match counting.finish(result, on_inner) {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        failure = Some(e);
                        Ok(false)
                    }
                }
            });
            if let Some(e) = failure {
                return Err(e);
            }
            if let Err(e) = outcome {
                return Err(map_data_error(e, encrypted));
            }
        }
        throttle.finish();
        Ok(report)
    }
}

/// 把一个条目的数据交给接收端；符号链接的内容就是目标路径。sink 没读完时把剩余吃掉以对齐下一条。
fn deliver_entry(
    entry: &Entry,
    data: &mut CountingReader<'_, '_, &mut dyn Read>,
    sink: &mut dyn EntrySink,
    report: &mut ExtractReport,
) -> Result<()> {
    if entry.kind == EntryKind::Symlink {
        let mut target = Vec::new();
        data.by_ref().take(MAX_SYMLINK_TARGET + 1).read_to_end(&mut target)?;
        if target.len() as u64 > MAX_SYMLINK_TARGET {
            return Err(Error::Corrupt(format!("符号链接目标过长：{}", progress_name(entry))));
        }
        sink.symlink(entry, &target)?;
    } else {
        sink.file(entry, data)?;
        data.drain()?;
    }
    report.extracted += 1;
    Ok(())
}

impl ArchiveReader for SevenZReader {
    fn format(&self) -> Format {
        Format::SevenZ
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn needs_password(&self) -> PasswordNeed {
        if self.archive.is_none() {
            PasswordNeed::ForNames
        } else if self.password.is_empty() && self.entries.iter().any(|e| e.encrypted) {
            PasswordNeed::ForData
        } else {
            PasswordNeed::None
        }
    }

    /// 设密码。头加密的包在这里重新读头；密码不对报 WrongPassword 并保持「需要密码」状态。
    fn set_password(&mut self, password: Option<&str>) -> Result<()> {
        self.password = password.filter(|p| !p.is_empty()).map(Password::from).unwrap_or_else(Password::empty);
        if !self.header_encrypted {
            return Ok(());
        }
        self.archive = None;
        self.entries.clear();
        self.block_encrypted.clear();
        if self.password.is_empty() {
            return Ok(());
        }
        self.source.seek(SeekFrom::Start(0))?;
        match SzArchive::read(&mut self.source, &self.password) {
            Ok(archive) => {
                self.install(archive);
                Ok(())
            }
            Err(e) => Err(map_header_error(e)),
        }
    }

    fn extract(&mut self, ids: Option<&[EntryId]>, sink: &mut dyn EntrySink, ctx: &OpContext<'_>) -> Result<ExtractReport> {
        if self.archive.is_none() {
            return Err(Error::PasswordRequired);
        }
        let selected = selection_mask(ids, self.entries.len())?;
        // 选中了加密条目却没密码：早点报，别解到一半才报
        if self.password.is_empty() && self.entries.iter().enumerate().any(|(i, e)| selected[i] && e.encrypted) {
            return Err(Error::PasswordRequired);
        }
        self.extract_blocks(&selected, sink, ctx)
    }

    fn read_entry(&mut self, id: EntryId, out: &mut dyn Write, ctx: &OpContext<'_>) -> Result<()> {
        self.extract(Some(&[id]), &mut WriterSink(out), ctx).map(|_| ())
    }
}

/// 条目类型与 Unix mode：7-Zip 在 Windows 属性高 16 位带 st_mode（0x8000 标记），S_IFLNK 就是符号链接。
fn kind_and_mode(f: &sevenz_rust2::ArchiveEntry) -> (EntryKind, Option<u32>) {
    let mode = (f.has_windows_attributes && f.windows_attributes & FILE_ATTRIBUTE_UNIX_EXTENSION != 0).then_some(f.windows_attributes >> 16);
    let kind = if f.is_directory {
        EntryKind::Dir
    } else if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    (kind, mode)
}

/// 块的压缩方法与是否加密：过滤器（BCJ*/Delta）不算方法；BCJ2 块里有多个压缩编码器
/// （主流 LZMA2 + 两路小 LZMA），取输出最大的那个当主方法。
fn classify_block(block: &Block) -> (Method, bool) {
    let mut method: Option<(u64, Method)> = None;
    let mut encrypted = false;
    for (index, coder) in block.coders.iter().enumerate() {
        let id = coder.encoder_method_id();
        let m = if id == EncoderMethod::ID_COPY {
            continue;
        } else if id == EncoderMethod::ID_AES256_SHA256 {
            encrypted = true;
            continue;
        } else if id == EncoderMethod::ID_LZMA {
            Method::Lzma
        } else if id == EncoderMethod::ID_LZMA2 {
            Method::Lzma2
        } else if id == EncoderMethod::ID_PPMD {
            Method::Ppmd
        } else if id == EncoderMethod::ID_BZIP2 {
            Method::Bzip2
        } else if id == EncoderMethod::ID_DEFLATE {
            Method::Deflate
        } else if id == EncoderMethod::ID_DEFLATE64 {
            Method::Deflate64
        } else if id == EncoderMethod::ID_ZSTD {
            Method::Zstd
        } else if is_filter(id) {
            continue;
        } else {
            Method::Other(EncoderMethod::by_id(id).map(|m| m.name().to_owned()).unwrap_or_else(|| format!("7z:{id:02x?}")))
        };
        let size = block.get_unpack_size_at_index(index);
        if method.as_ref().is_none_or(|(best, _)| size > *best) {
            method = Some((size, m));
        }
    }
    (method.map(|(_, m)| m).unwrap_or(Method::Store), encrypted)
}

/// 是否是预处理过滤器（BCJ 系列、BCJ2、Delta）。
fn is_filter(id: &[u8]) -> bool {
    [
        EncoderMethod::ID_BCJ_X86,
        EncoderMethod::ID_BCJ2,
        EncoderMethod::ID_BCJ_PPC,
        EncoderMethod::ID_BCJ_IA64,
        EncoderMethod::ID_BCJ_ARM,
        EncoderMethod::ID_BCJ_ARM64,
        EncoderMethod::ID_BCJ_ARM_THUMB,
        EncoderMethod::ID_BCJ_SPARC,
        EncoderMethod::ID_BCJ_RISCV,
        EncoderMethod::ID_DELTA,
    ]
    .contains(&id)
}

/// 块的打包大小：pack_stream_offsets 是各打包流起点的前缀和，相邻块首流偏移之差就是本块大小。
fn block_packed_size(archive: &SzArchive, b: usize) -> Option<u64> {
    let firsts = archive.stream_map.block_first_pack_stream_index();
    let offsets = archive.stream_map.pack_stream_offsets();
    let first = *firsts.get(b)?;
    let start = *offsets.get(first)?;
    let end = match firsts.get(b + 1) {
        Some(&next) => *offsets.get(next)?,
        None => archive.pack_sizes().iter().sum::<u64>(),
    };
    end.checked_sub(start)
}

/// Windows FILETIME（1601 年起的 100 ns）转 Unix 秒。
fn nt_to_unix(t: NtTime) -> i64 {
    let ft: u64 = t.into();
    (ft / 10_000_000) as i64 - 11_644_473_600
}

/// 读头阶段的错误映射：密码相关单独分出来，其余都算包损坏。
fn map_header_error(e: SzError) -> Error {
    match e {
        SzError::PasswordRequired => Error::PasswordRequired,
        SzError::MaybeBadPassword(_) => Error::WrongPassword,
        SzError::UnsupportedCompressionMethod(m) => Error::Unsupported(format!("7z 压缩方法 {m}")),
        SzError::Unsupported(m) | SzError::Other(m) if m.contains("unsupported") => Error::Unsupported(format!("7z：{m}")),
        SzError::Io(e, _) | SzError::FileOpen(e, _) => Error::Io(e),
        other => Error::Corrupt(format!("7z：{other}")),
    }
}

/// 解码阶段的错误映射：加密块里的数据错误大概率是密码不对，按 WrongPassword 报。
fn map_data_error(e: SzError, encrypted: bool) -> Error {
    match e {
        SzError::PasswordRequired => Error::PasswordRequired,
        SzError::MaybeBadPassword(_) => Error::WrongPassword,
        SzError::UnsupportedCompressionMethod(m) => Error::Unsupported(format!("7z 压缩方法 {m}")),
        SzError::Unsupported(m) => Error::Unsupported(format!("7z：{m}")),
        SzError::ChecksumVerificationFailed | SzError::Io(..) if encrypted => Error::WrongPassword,
        other => Error::Corrupt(format!("7z：{other}")),
    }
}
