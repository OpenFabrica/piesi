//! RAR 驱动：只读，基于 RARLAB 官方 unrar 库（经 unrar_sys 静态链接）。unrar 只有顺序处理模型：
//! 列目录时逐个读头并跳过数据；解压时在工作线程里跑 RARProcessFile，用 UCM_PROCESSDATA 回调
//! 把数据块经有界通道送回调用线程喂给 sink，从不把整个文件读进内存。
//! 分卷（.part1.rar / .r00）unrar 自己按文件名找下一卷，只需传首卷。

use std::ffi::{c_char, c_int, c_uint, CString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;

use unrar_sys as sys;

use super::tar::{progress_name, selection_mask, CountingReader, WriterSink};
use crate::archive::{ArchiveReader, Entry, EntryId, EntryKind, EntrySink, ExtractReport, Format, Method, PasswordNeed};
use crate::progress::{CancelToken, OpContext, Throttle};
use crate::warning::{Warning, WarningKind};
use crate::{DetailKind, ErrorDetail};
use crate::{Error, Result};

/// 回调里把 unrar 吐出的数据切成这么大的块再过通道，限制通道占用的内存。
const CHUNK: usize = 256 * 1024;
/// 通道深度：生产者最多领先消费者这么多块，再多就阻塞（背压）。
const CHANNEL_DEPTH: usize = 8;
/// solid 包里跳过条目时，解码丢弃的字节攒到这么多才报一次进度。
const SKIP_REPORT_CHUNK: u64 = 1024 * 1024;
/// 符号链接目标长度上限（RAR4 把目标存成文件内容）。
const MAX_SYMLINK_TARGET: usize = 64 * 1024;
/// 读头时给 unrar 的链接目标缓冲长度（宽字符数），RAR 路径上限 2048。
const REDIR_BUF: usize = 2048;
/// 读头时给 unrar 的长文件名缓冲长度（宽字符数）。
const NAME_EX_BUF: usize = 4096;
/// 注释缓冲长度（宽字符数）。
const COMMENT_BUF: usize = 16 * 1024;

/// unrar 头里的 HostOS：Unix 时 file_attr 才是 st_mode。
const HOST_UNIX: u32 = 3;
/// RAR5 的 redir 类型（headers.hpp FILE_SYSTEM_REDIRECT）。
const REDIR_UNIX_SYMLINK: u32 = 1;
const REDIR_WIN_SYMLINK: u32 = 2;
const REDIR_JUNCTION: u32 = 3;
const REDIR_HARDLINK: u32 = 4;
const REDIR_FILECOPY: u32 = 5;

/// 条目在驱动内部的细分类型：决定解压时是走数据流、取 redir 目标还是跳过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RarKind {
    Regular,
    Dir,
    /// 目标在头里（RAR5 -ol）。
    SymlinkRedir,
    /// 目标是文件内容（RAR4 -ol）。
    SymlinkData,
    HardLink,
    FileCopy,
}

/// RAR 读取器。
pub struct RarReader {
    path: PathBuf,
    entries: Vec<Entry>,
    kinds: Vec<RarKind>,
    password: Option<String>,
    header_encrypted: bool,
    solid: bool,
    comment: Option<Vec<u8>>,
}

impl RarReader {
    /// 打开并列目录。头加密（-hp）且没密码时打开成功但 entries 为空，等 set_password 后重开。
    pub fn open(path: &Path) -> Result<Self> {
        let mut reader = Self {
            path: path.to_path_buf(),
            entries: Vec::new(),
            kinds: Vec::new(),
            password: None,
            header_encrypted: false,
            solid: false,
            comment: None,
        };
        reader.list()?;
        Ok(reader)
    }

    /// 在当前线程用 LIST 模式逐个读头，填充 entries。
    fn list(&mut self) -> Result<()> {
        self.entries.clear();
        self.kinds.clear();
        // 回调状态要活过整个列目录过程：打开和读头都可能回调要密码
        let mut state = CallbackState::for_listing(self.password.clone());
        let opened = match Handle::open(&self.path, sys::RAR_OM_LIST, &mut state, self.header_encrypted && self.password.is_some()) {
            Ok(opened) => opened,
            // RAR5 头加密：打开时就通过回调要密码，没有就打不开，这时列表留空等密码
            Err(Error::PasswordRequired) if self.password.is_none() => {
                self.header_encrypted = true;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        self.solid = opened.flags & sys::ROADF_SOLID != 0;
        self.header_encrypted = opened.flags & sys::ROADF_ENCHEADERS != 0;
        self.comment = opened.comment;
        let handle = opened.handle;
        let mut hd = HeaderBuf::new();
        loop {
            let raw = match hd.read(&handle) {
                Ok(Some(raw)) => raw,
                Ok(None) => break,
                // 头加密且没密码：unrar 读第一个头就报缺密码，这时列表留空，等密码
                Err(RarCode::MissingPassword) if self.password.is_none() => {
                    self.header_encrypted = true;
                    return Ok(());
                }
                Err(code) => return Err(map_code(code, Stage::Read, self.header_encrypted && self.password.is_some())),
            };
            let (entry, kind) = entry_from_header(self.entries.len() as EntryId, &raw, self.solid);
            self.entries.push(entry);
            self.kinds.push(kind);
            // LIST 模式下 SKIP 只是 seek 到下一个头
            handle.process(sys::RAR_SKIP).map_err(|c| map_code(c, Stage::Process, false))?;
        }
        Ok(())
    }
}

impl ArchiveReader for RarReader {
    fn format(&self) -> Format {
        Format::Rar
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn needs_password(&self) -> PasswordNeed {
        if self.header_encrypted && self.entries.is_empty() {
            PasswordNeed::ForNames
        } else if self.password.is_none() && self.entries.iter().any(|e| e.encrypted) {
            PasswordNeed::ForData
        } else {
            PasswordNeed::None
        }
    }

    /// 设密码。头加密的包要重新打开重列；密码不对报 WrongPassword 并保持「需要密码」状态。
    fn set_password(&mut self, password: Option<&str>) -> Result<()> {
        self.password = password.filter(|p| !p.is_empty()).map(str::to_owned);
        if self.header_encrypted {
            if let Err(e) = self.list() {
                self.entries.clear();
                self.kinds.clear();
                return Err(e);
            }
        }
        Ok(())
    }

    fn extract(&mut self, ids: Option<&[EntryId]>, sink: &mut dyn EntrySink, ctx: &OpContext<'_>) -> Result<ExtractReport> {
        if self.header_encrypted && self.entries.is_empty() {
            return Err(Error::PasswordRequired);
        }
        let selected = selection_mask(ids, self.entries.len())?;
        if self.password.is_none() && self.entries.iter().enumerate().any(|(i, e)| selected[i] && e.encrypted) {
            return Err(Error::PasswordRequired);
        }

        // 工作线程的处理计划：每个条目跳过 / 只报元数据 / 流式送数据；处理到最后一个选中条目就停
        let mut plan = Vec::with_capacity(self.entries.len());
        let mut total_entries = 0u64;
        let mut stop_after = None;
        for (i, _) in self.entries.iter().enumerate() {
            if !selected[i] {
                plan.push(Action::Skip);
                continue;
            }
            total_entries += 1;
            stop_after = Some(i);
            plan.push(match self.kinds[i] {
                RarKind::Regular => Action::Stream,
                RarKind::SymlinkData => Action::Link,
                _ => Action::Meta,
            });
        }
        // 进度总量按「需要解码的字节」算：solid 包里最后一个选中条目之前的所有文件都得解码
        // （跳过的也经回调计入进度），非 solid 包跳过只是 seek，只算选中的
        let total_bytes: u64 = self
            .entries
            .iter()
            .enumerate()
            .filter(|(i, _)| self.kinds[*i] == RarKind::Regular && (selected[*i] || (self.solid && stop_after.is_some_and(|last| *i < last))))
            .map(|(_, e)| e.size)
            .sum();

        let (tx, rx) = mpsc::sync_channel::<Msg>(CHANNEL_DEPTH);
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            path: self.path.clone(),
            password: self.password.clone(),
            plan,
            stop_after,
            solid: self.solid,
            tx,
            stop: Arc::clone(&stop),
            cancel: ctx.cancel.clone(),
        };
        let mut throttle = Throttle::new(ctx, Some(total_bytes), Some(total_entries));
        // 工作线程里跑 unrar，本线程消费数据；离开作用域前线程一定被 join，
        // 消费端提前退出时 stop 标志 + 通道断开会让回调返回 -1 中止 unrar。
        let result = std::thread::scope(|scope| {
            scope.spawn(move || worker.run());
            // rx 必须在闭包里持有：闭包返回时先置 stop 再丢掉 rx，工作线程阻塞的 send 才会失败退出，
            // 然后 scope 才 join 线程；否则消费端提前出错时会和卡在 send 上的工作线程互相等待
            let rx = rx;
            let _guard = StopOnDrop(&stop);
            consume(&rx, &self.entries, &self.kinds, sink, &mut throttle)
        });
        let report = result?;
        throttle.finish();
        Ok(report)
    }

    fn read_entry(&mut self, id: EntryId, out: &mut dyn Write, ctx: &OpContext<'_>) -> Result<()> {
        self.extract(Some(&[id]), &mut WriterSink(out), ctx).map(|_| ())
    }

    fn comment(&self) -> Option<&[u8]> {
        self.comment.as_deref()
    }
}

/// 消费端提前返回时置位 stop，让工作线程的回调尽快中止。
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// 工作线程对每个条目要做的事。
#[derive(Clone, Copy, Debug)]
enum Action {
    Skip,
    Meta,
    Stream,
    /// RAR4 符号链接：目标存成文件内容，但 unrar 在测试模式下不吐链接数据，
    /// 只能让它解到临时目录再 readlink。
    Link,
}

/// 工作线程 → 消费线程的消息。
enum Msg {
    /// 元数据条目（目录、redir 符号链接、硬链接等），没有数据。
    Meta(usize),
    /// RAR4 符号链接的目标；None 表示 unrar 拒绝创建（目标不安全）或没拿到。
    Link(usize, Option<Vec<u8>>),
    /// 条目数据开始。
    Begin(usize),
    Data(Vec<u8>),
    /// 条目数据结束，带 unrar 的处理结果。
    End(usize, std::result::Result<(), RarCode>),
    /// 跳过的条目出了错（solid 包里跳过也要解码），只当警告。
    Warning(usize, RarCode),
    /// solid 包里跳过条目时解码并丢弃的字节数，消费端计入进度，免得进度条长时间不动。
    Skipped(u64),
    /// 全部处理完，带最终结果。
    Finished(Result<()>),
}

/// 消费端：按消息顺序把条目交给接收端。
fn consume(rx: &Receiver<Msg>, entries: &[Entry], kinds: &[RarKind], sink: &mut dyn EntrySink, throttle: &mut Throttle<'_>) -> Result<ExtractReport> {
    let mut report = ExtractReport::default();
    loop {
        let msg = rx.recv().map_err(|_| Error::Other("unrar 工作线程意外退出".into()))?;
        match msg {
            Msg::Meta(i) => {
                let entry = entries.get(i).ok_or_else(|| Error::Corrupt(DetailKind::EntryIndexOutOfRange.into()))?;
                throttle.begin_entry(&progress_name(entry))?;
                match kinds[i] {
                    RarKind::Dir => {
                        sink.dir(entry)?;
                        report.extracted += 1;
                    }
                    RarKind::SymlinkRedir => {
                        sink.symlink(entry, entry.link_target.as_deref().unwrap_or_default())?;
                        report.extracted += 1;
                    }
                    RarKind::HardLink => {
                        report.warnings.push(Warning::new(WarningKind::HardLinkSkipped, progress_name(entry)));
                        report.skipped += 1;
                    }
                    RarKind::FileCopy => {
                        report.warnings.push(Warning::new(WarningKind::RarReferenceSkipped, progress_name(entry)));
                        report.skipped += 1;
                    }
                    RarKind::Regular | RarKind::SymlinkData => {
                        report.warnings.push(Warning::new(WarningKind::NoDataSkipped, progress_name(entry)));
                        report.skipped += 1;
                    }
                }
            }
            Msg::Link(i, target) => {
                let entry = entries.get(i).ok_or_else(|| Error::Corrupt(DetailKind::EntryIndexOutOfRange.into()))?;
                throttle.begin_entry(&progress_name(entry))?;
                match target {
                    Some(target) => {
                        sink.symlink(entry, &target)?;
                        report.extracted += 1;
                    }
                    None => {
                        report.warnings.push(Warning::new(WarningKind::RarSymlinkRejected, progress_name(entry)));
                        report.skipped += 1;
                    }
                }
            }
            Msg::Begin(i) => {
                let entry = entries.get(i).ok_or_else(|| Error::Corrupt(DetailKind::EntryIndexOutOfRange.into()))?;
                throttle.begin_entry(&progress_name(entry))?;
                let mut chan = ChannelReader { rx, index: i, pending: None, done: None };
                let mut counting = CountingReader::new(&mut chan, &mut *throttle);
                let delivered = sink.file(entry, &mut counting).map(|_| ());
                // 先把通道里这个条目剩下的数据吃完，保证下一条消息对齐
                let _ = counting.drain();
                let on_inner = |m: String| if entry.encrypted { Error::WrongPassword } else { Error::corrupt(m) };
                counting.finish(delivered, on_inner)?;
                match chan.done.take() {
                    Some(Ok(())) => report.extracted += 1,
                    Some(Err(code)) => return Err(map_code(code, Stage::Process, entry.encrypted)),
                    None => return Err(Error::Other("unrar 数据流提前结束".into())),
                }
            }
            Msg::Data(_) => return Err(Error::Other("unrar 工作线程消息乱序".into())),
            Msg::End(..) => return Err(Error::Other("unrar 工作线程消息乱序".into())),
            Msg::Warning(i, code) => {
                let name = entries.get(i).map(progress_name).unwrap_or_default();
                report.warnings.push(Warning::with_detail(WarningKind::RarSkipDecodeError, name, format!("{code:?}")));
            }
            // 跳过的字节也算进度，同时充当取消检查点
            Msg::Skipped(n) => throttle.add_bytes(n)?,
            Msg::Finished(result) => {
                result?;
                return Ok(report);
            }
        }
    }
}

/// 把通道里一个条目的 Data 块串成 Read，读到 End 为止。
struct ChannelReader<'r> {
    rx: &'r Receiver<Msg>,
    /// 正在读的条目下标，End 消息必须对得上。
    index: usize,
    pending: Option<(Vec<u8>, usize)>,
    done: Option<std::result::Result<(), RarCode>>,
}

impl Read for ChannelReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.done.is_some() {
                return Ok(0);
            }
            if let Some((chunk, pos)) = &mut self.pending {
                let n = (chunk.len() - *pos).min(buf.len());
                buf[..n].copy_from_slice(&chunk[*pos..*pos + n]);
                *pos += n;
                if *pos >= chunk.len() {
                    self.pending = None;
                }
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
                continue;
            }
            match self.rx.recv() {
                Ok(Msg::Data(chunk)) => self.pending = Some((chunk, 0)),
                Ok(Msg::End(i, result)) if i == self.index => self.done = Some(result),
                Ok(_) => return Err(io::Error::other("unrar 工作线程消息乱序")),
                Err(_) => return Err(io::Error::other("unrar 工作线程意外退出")),
            }
        }
    }
}

/// 工作线程：拥有自己的 unrar 句柄，按计划顺序处理每个条目。
struct Worker {
    path: PathBuf,
    password: Option<String>,
    plan: Vec<Action>,
    /// 最后一个选中条目的下标，处理完它就不再往下读；None 表示什么都没选。
    stop_after: Option<usize>,
    solid: bool,
    tx: SyncSender<Msg>,
    stop: Arc<AtomicBool>,
    cancel: CancelToken,
}

impl Worker {
    /// 线程入口：最后总会发一条 Finished（除非消费端已经不在了）。
    fn run(self) {
        let mut state = CallbackState {
            tx: Some(self.tx.clone()),
            stop: Arc::clone(&self.stop),
            cancel: self.cancel.clone(),
            password: self.password.clone(),
            discard: false,
            aborted: false,
            skipped_pending: 0,
        };
        let result = self.process_all(&mut state);
        let _ = self.tx.send(Msg::Finished(result));
    }

    /// 顺序遍历所有头并按计划处理；任何一步失败都整体失败。
    fn process_all(&self, state: &mut CallbackState) -> Result<()> {
        let opened = Handle::open(&self.path, sys::RAR_OM_EXTRACT, state, self.password.is_some())?;
        let handle = opened.handle;
        let mut hd = HeaderBuf::new();
        let mut index = 0usize;
        let mut scratch = ScratchDir::default();
        loop {
            // 最后一个选中条目之后的都不用碰（solid 包里继续读只是白白解码）
            if self.stop_after.is_none_or(|last| index > last) {
                break;
            }
            if hd.read(&handle).map_err(|c| map_code(c, Stage::Read, self.password.is_some()))?.is_none() {
                break;
            }
            let action = self.plan.get(index).copied().unwrap_or(Action::Skip);
            match action {
                Action::Skip => self.skip_entry(&handle, state, index)?,
                Action::Meta => {
                    self.send(Msg::Meta(index))?;
                    self.skip_entry(&handle, state, index)?;
                }
                Action::Link => {
                    state.discard = true;
                    let target = scratch.extract_link(&handle);
                    state.discard = false;
                    if state.aborted {
                        return Err(abort_reason(&self.cancel));
                    }
                    self.send(Msg::Link(index, target))?;
                }
                Action::Stream => {
                    self.send(Msg::Begin(index))?;
                    state.discard = false;
                    let r = handle.process(sys::RAR_TEST);
                    if state.aborted {
                        return Err(abort_reason(&self.cancel));
                    }
                    self.send(Msg::End(index, r))?;
                }
            }
            index += 1;
        }
        Ok(())
    }

    /// 跳过一个条目。非 solid 包 RAR_SKIP 只是 seek；solid 包里 unrar 仍要解码来维持字典，
    /// 这时改用 RAR_TEST 并在回调里丢弃数据，这样取消能在块中间响应，而不是等整个文件解完。
    fn skip_entry(&self, handle: &Handle, state: &mut CallbackState, index: usize) -> Result<()> {
        if !self.solid {
            return handle.process(sys::RAR_SKIP).map_err(|c| map_code(c, Stage::Process, false));
        }
        state.discard = true;
        let r = handle.process(sys::RAR_TEST);
        state.discard = false;
        if state.aborted {
            return Err(abort_reason(&self.cancel));
        }
        // 回调里攒下的零头也要报出去，进度才能对上总量
        let pending = std::mem::take(&mut state.skipped_pending);
        if pending > 0 {
            self.send(Msg::Skipped(pending))?;
        }
        if let Err(code) = r {
            // 没选中的条目坏了不影响用户要的文件：只当警告，继续
            self.send(Msg::Warning(index, code))?;
        }
        Ok(())
    }

    /// 发消息；消费端已退出时返回 Cancelled 让线程尽快收工。
    fn send(&self, msg: Msg) -> Result<()> {
        self.tx.send(msg).map_err(|_| abort_reason(&self.cancel))
    }
}

/// 让 unrar 把 RAR4 符号链接解到临时目录再 readlink 的临时空间；Drop 时整个删掉。
#[derive(Default)]
struct ScratchDir {
    dir: Option<PathBuf>,
}

impl ScratchDir {
    /// 第一次用时才创建目录。
    fn path(&mut self) -> io::Result<&Path> {
        if self.dir.is_none() {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let dir = std::env::temp_dir().join(format!("piesi-rar-{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            self.dir = Some(dir);
        }
        Ok(self.dir.as_deref().unwrap_or(Path::new(".")))
    }

    /// 用 RAR_EXTRACT 把当前链接条目解到临时目录，读出目标后清空目录；任何一步失败都返回 None。
    /// unrar 自己会拒绝绝对路径或逃出目录的目标，这类情况也是 None，由消费端告警。
    /// 不用条目名去拼路径：名字是不可信数据（可能带 `..`），改为在空的临时目录里找唯一的链接。
    fn extract_link(&mut self, handle: &Handle) -> Option<Vec<u8>> {
        let dir = self.path().ok()?.to_path_buf();
        let dest = to_wide(&dir.to_string_lossy());
        // SAFETY：句柄有效，dest 是 NUL 结尾的宽字符串，在调用期间活着。
        let code = unsafe { sys::RARProcessFileW(handle.0.as_ptr(), sys::RAR_EXTRACT, dest.as_ptr(), std::ptr::null()) };
        let target = if code == sys::ERAR_SUCCESS { find_symlink(&dir, 0).and_then(|p| std::fs::read_link(p).ok()) } else { None };
        // 不管成败都清空，下一个链接再重建，保证目录里永远只有当前这一个
        let _ = std::fs::remove_dir_all(&dir);
        self.dir = None;
        target.map(|t| t.into_os_string().into_encoded_bytes()).filter(|t| t.len() <= MAX_SYMLINK_TARGET)
    }
}

/// 在目录里递归找第一个符号链接（限深 32 层，不跟随链接）。
fn find_symlink(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 32 {
        return None;
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if meta.file_type().is_symlink() {
            return Some(path);
        }
        if meta.is_dir() {
            if let Some(found) = find_symlink(&path, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// 中止原因：用户取消了就是 Cancelled，否则是消费端（sink）先失败了，错误由消费端报。
fn abort_reason(cancel: &CancelToken) -> Error {
    if cancel.is_cancelled() {
        Error::Cancelled
    } else {
        Error::Other("解压已中止".into())
    }
}

/// 回调共享状态：unrar 在 RAROpenArchiveEx / RARProcessFile 内同步调用回调，
/// 所以生命周期由调用方（列目录时是 list()，解压时是 Worker）保证。
struct CallbackState {
    /// 解压时数据块送这里；列目录时为 None。
    tx: Option<SyncSender<Msg>>,
    stop: Arc<AtomicBool>,
    cancel: CancelToken,
    /// 头加密的 RAR5 在打开时就会通过 UCM_NEEDPASSWORDW 要密码，这里能答上才打得开。
    password: Option<String>,
    /// 为真时数据直接丢弃（solid 包里跳过条目）。
    discard: bool,
    /// 回调已返回 -1 让 unrar 中止。
    aborted: bool,
    /// 丢弃模式下攒着还没报给消费端的字节数，攒够 SKIP_REPORT_CHUNK 才发一条消息。
    skipped_pending: u64,
}

impl CallbackState {
    /// 列目录用的状态：没有数据通道，不可取消。
    fn for_listing(password: Option<String>) -> Self {
        Self {
            tx: None,
            stop: Arc::new(AtomicBool::new(false)),
            cancel: CancelToken::new(),
            password,
            discard: true,
            aborted: false,
            skipped_pending: 0,
        }
    }
}

/// unrar 回调：数据块送通道；要密码时把已知密码填进缓冲；缺卷不交互直接失败。
extern "C" fn rar_callback(msg: sys::UINT, user_data: sys::LPARAM, p1: sys::LPARAM, p2: sys::LPARAM) -> c_int {
    if user_data == 0 {
        return -1;
    }
    // SAFETY：user_data 是调用方栈上 &mut CallbackState 的指针，
    // 回调只在 unrar 的同步调用期间被调，此时该引用一定还活着。
    let state = unsafe { &mut *(user_data as *mut CallbackState) };
    match msg {
        sys::UCM_PROCESSDATA => {
            if state.stop.load(Ordering::Relaxed) || state.cancel.is_cancelled() {
                state.aborted = true;
                return -1;
            }
            if p2 <= 0 {
                return 0;
            }
            let Some(tx) = &state.tx else { return 0 };
            if state.discard {
                // 解码后丢弃的字节也要计入进度，攒够一块再发，免得消息太密
                state.skipped_pending += p2 as u64;
                if state.skipped_pending >= SKIP_REPORT_CHUNK {
                    let n = std::mem::take(&mut state.skipped_pending);
                    if tx.send(Msg::Skipped(n)).is_err() {
                        state.aborted = true;
                        return -1;
                    }
                }
                return 0;
            }
            // SAFETY：unrar 保证 p1 指向 p2 个有效字节，仅在回调期间有效，这里立即拷走。
            let data = unsafe { std::slice::from_raw_parts(p1 as *const u8, p2 as usize) };
            for chunk in data.chunks(CHUNK) {
                if tx.send(Msg::Data(chunk.to_vec())).is_err() {
                    state.aborted = true;
                    return -1;
                }
            }
            0
        }
        // 找不到下一卷（RAR_VOL_ASK）不交互，直接让 unrar 报 EOPEN；找到了（NOTIFY）继续
        sys::UCM_CHANGEVOLUME | sys::UCM_CHANGEVOLUMEW => {
            if p2 == sys::RAR_VOL_ASK {
                -1
            } else {
                0
            }
        }
        // p1 = 宽字符缓冲，p2 = 容量（字符数）；没密码就返回 -1，unrar 会报 MISSING_PASSWORD
        sys::UCM_NEEDPASSWORDW => match &state.password {
            Some(pw) if p1 != 0 && p2 > 0 => {
                let cap = p2 as usize;
                let mut w = to_wide(pw);
                w.truncate(cap);
                if let Some(last) = w.last_mut() {
                    *last = 0;
                }
                // SAFETY：unrar 给的缓冲至少有 cap 个宽字符，w.len() <= cap。
                unsafe { std::ptr::copy_nonoverlapping(w.as_ptr(), p1 as *mut sys::WCHAR, w.len()) };
                0
            }
            _ => -1,
        },
        // 窄字符版本：只在宽字符版本没给出密码时才会被问，这里同样回答（UTF-8）
        sys::UCM_NEEDPASSWORD => match &state.password {
            Some(pw) if p1 != 0 && p2 > 0 => {
                let cap = p2 as usize;
                let mut a = pw.as_bytes().to_vec();
                a.push(0);
                a.truncate(cap);
                if let Some(last) = a.last_mut() {
                    *last = 0;
                }
                // SAFETY：unrar 给的缓冲至少有 cap 字节，a.len() <= cap。
                unsafe { std::ptr::copy_nonoverlapping(a.as_ptr(), p1 as *mut u8, a.len()) };
                0
            }
            _ => -1,
        },
        _ => 0,
    }
}

/// 与 dll.hpp 完全一致的 `RAROpenArchiveDataEx` 布局。dll.hpp 用 `#pragma pack(push, 1)`，
/// 而 unrar_sys 里的同名结构体没有 packed，指针字段之后的偏移全错（unrar crate 只读前面的字段
/// 所以没踩到）；直接用它会让 unrar 把注释写到野指针上。这里按 C 布局重新声明，调用时把指针转过去。
#[repr(C, packed)]
struct RarOpenData {
    archive_name: *const c_char,
    archive_name_w: *const sys::WCHAR,
    open_mode: c_uint,
    open_result: c_uint,
    comment_buffer: *mut c_char,
    comment_buffer_size: c_uint,
    comment_size: c_uint,
    comment_state: c_uint,
    flags: c_uint,
    callback: Option<sys::Callback>,
    user_data: sys::LPARAM,
    op_flags: c_uint,
    comment_buffer_w: *mut sys::WCHAR,
    reserved: [c_uint; 25],
}

/// 与 dll.hpp 完全一致的 `RARHeaderDataEx` 布局（同样是 pack(1)），理由同上；
/// 用错布局时 mtime、redir 等字段全读错，redir_name 还会被当野指针写。
#[repr(C, packed)]
struct RarHeaderData {
    arc_name: [c_char; 1024],
    arc_name_w: [sys::WCHAR; 1024],
    file_name: [c_char; 1024],
    file_name_w: [sys::WCHAR; 1024],
    flags: c_uint,
    pack_size: c_uint,
    pack_size_high: c_uint,
    unp_size: c_uint,
    unp_size_high: c_uint,
    host_os: c_uint,
    file_crc: c_uint,
    file_time: c_uint,
    unp_ver: c_uint,
    method: c_uint,
    file_attr: c_uint,
    cmt_buf: *mut c_char,
    cmt_buf_size: c_uint,
    cmt_size: c_uint,
    cmt_state: c_uint,
    dict_size: c_uint,
    hash_type: c_uint,
    hash: [u8; 32],
    redir_type: c_uint,
    redir_name: *mut sys::WCHAR,
    redir_name_size: c_uint,
    dir_target: c_uint,
    mtime_low: c_uint,
    mtime_high: c_uint,
    ctime_low: c_uint,
    ctime_high: c_uint,
    atime_low: c_uint,
    atime_high: c_uint,
    arc_name_ex: *mut sys::WCHAR,
    arc_name_ex_size: c_uint,
    file_name_ex: *mut sys::WCHAR,
    file_name_ex_size: c_uint,
    reserved: [c_uint; 982],
}

// 编译期核对布局：和 dll.hpp 按 pack(1) 逐字段算出来的大小一致
const PTR: usize = std::mem::size_of::<usize>();
const WCH: usize = std::mem::size_of::<sys::WCHAR>();
const _: () = assert!(std::mem::size_of::<RarOpenData>() == 6 * PTR + 7 * 4 + 25 * 4);
const _: () = assert!(
    std::mem::size_of::<RarHeaderData>() == 2 * 1024 + 2 * 1024 * WCH + 11 * 4 + PTR + 5 * 4 + 32 + 4 + PTR + 8 * 4 + PTR + 4 + PTR + 4 + 982 * 4
);

/// unrar 错误码（dll.hpp ERAR_*）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RarCode {
    BadData,
    BadArchive,
    UnknownFormat,
    EOpen,
    ECreate,
    EClose,
    ERead,
    EWrite,
    SmallBuf,
    NoMemory,
    Unknown,
    MissingPassword,
    EReference,
    BadPassword,
    Other(c_int),
}

impl RarCode {
    /// 从 C 返回值映射。
    fn from_raw(code: c_int) -> Self {
        match code {
            sys::ERAR_BAD_DATA => Self::BadData,
            sys::ERAR_BAD_ARCHIVE => Self::BadArchive,
            sys::ERAR_UNKNOWN_FORMAT => Self::UnknownFormat,
            sys::ERAR_EOPEN => Self::EOpen,
            sys::ERAR_ECREATE => Self::ECreate,
            sys::ERAR_ECLOSE => Self::EClose,
            sys::ERAR_EREAD => Self::ERead,
            sys::ERAR_EWRITE => Self::EWrite,
            sys::ERAR_SMALL_BUF => Self::SmallBuf,
            sys::ERAR_NO_MEMORY => Self::NoMemory,
            sys::ERAR_UNKNOWN => Self::Unknown,
            sys::ERAR_MISSING_PASSWORD => Self::MissingPassword,
            sys::ERAR_EREFERENCE => Self::EReference,
            sys::ERAR_BAD_PASSWORD => Self::BadPassword,
            other => Self::Other(other),
        }
    }
}

/// 出错发生在哪一步，决定怎么解释 BadData。
#[derive(Clone, Copy, Debug)]
enum Stage {
    Open,
    Read,
    Process,
}

/// unrar 错误码 → 引擎错误。`encrypted_with_password` 为真时 BadData 多半是密码错（RAR4 没有密码校验值）。
fn map_code(code: RarCode, stage: Stage, encrypted_with_password: bool) -> Error {
    match code {
        RarCode::MissingPassword => Error::PasswordRequired,
        RarCode::BadPassword => Error::WrongPassword,
        RarCode::BadData if encrypted_with_password => Error::WrongPassword,
        RarCode::BadData => Error::Corrupt(
            match stage {
                Stage::Open => DetailKind::RarArchiveHeaderBad,
                Stage::Read => DetailKind::RarFileHeaderBad,
                Stage::Process => DetailKind::RarCrcFailed,
            }
            .into(),
        ),
        RarCode::BadArchive => Error::Corrupt(DetailKind::NotRar.into()),
        RarCode::UnknownFormat => Error::Unsupported(DetailKind::RarUnknownFormat.into()),
        RarCode::EOpen => match stage {
            Stage::Open => Error::Io(io::Error::new(io::ErrorKind::NotFound, "无法打开 RAR 压缩包")),
            _ => Error::Corrupt(DetailKind::NextVolumeMissing.into()),
        },
        RarCode::ERead => Error::Corrupt(DetailKind::RarReadError.into()),
        RarCode::EReference => Error::Unsupported(DetailKind::RarReference.into()),
        RarCode::NoMemory => Error::Other("unrar：内存不足".into()),
        RarCode::ECreate | RarCode::EClose | RarCode::EWrite => Error::Other(format!("unrar 写入错误：{code:?}")),
        RarCode::SmallBuf => Error::Other("unrar：缓冲区过小".into()),
        RarCode::Unknown => Error::Other("unrar：未知错误".into()),
        RarCode::Other(c) => Error::Other(format!("unrar 错误码 {c}")),
    }
}

/// RAII 的 unrar 句柄：Drop 时 RARCloseArchive。只能在创建它的线程上用。
struct Handle(NonNull<sys::Handle>);

/// 打开结果：句柄 + 包级标志 + 注释。
struct Opened {
    handle: Handle,
    flags: u32,
    comment: Option<Vec<u8>>,
}

impl Handle {
    /// 打开压缩包。密码既通过回调在打开时回答（RAR5 头加密需要），也在打开后用 RARSetPassword 设一次。
    /// `encrypted_with_password` 为真时打开阶段的 BadData 按密码错报。
    fn open(path: &Path, mode: u32, state: &mut CallbackState, encrypted_with_password: bool) -> Result<Opened> {
        let narrow = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| Error::UnsafePath(ErrorDetail::with(DetailKind::Plain, path.display())))?;
        let wide = to_wide(&path.to_string_lossy());
        let mut comment = vec![0 as sys::WCHAR; COMMENT_BUF];
        let password = state.password.clone();
        let mut data = RarOpenData {
            archive_name: narrow.as_ptr(),
            // Linux 上 unrar 内部按 locale 转码，unrar crate 的做法是只给窄字符；其他平台给宽字符
            archive_name_w: if cfg!(any(target_os = "linux", target_os = "netbsd")) { std::ptr::null() } else { wide.as_ptr() },
            open_mode: mode,
            open_result: 0,
            comment_buffer: std::ptr::null_mut(),
            comment_buffer_size: COMMENT_BUF as c_uint,
            comment_size: 0,
            comment_state: 0,
            flags: 0,
            callback: Some(rar_callback),
            user_data: state as *mut CallbackState as sys::LPARAM,
            op_flags: 0,
            comment_buffer_w: comment.as_mut_ptr(),
            reserved: [0; 25],
        };
        // SAFETY：data 及其指向的缓冲在调用期间都活着；unrar 会写回 open_result/flags/comment，
        // 所以从 &mut 派生指针再转成 unrar_sys 声明里的指针类型（内存布局见 RarOpenData 的说明）。
        let raw = unsafe { sys::RAROpenArchiveEx(&mut data as *mut RarOpenData as *const sys::OpenArchiveDataEx) };
        // 即使 open_result 非零 unrar 也可能返回句柄，必须包进 Handle 保证关闭
        let handle = NonNull::new(raw as *mut sys::Handle).map(Handle);
        let open_result = data.open_result;
        if open_result != 0 {
            drop(handle);
            return Err(map_code(RarCode::from_raw(open_result as c_int), Stage::Open, encrypted_with_password));
        }
        let handle = handle.ok_or_else(|| Error::Other("unrar 打开失败".into()))?;
        if let Some(pw) = password {
            let cpw = CString::new(pw).map_err(|_| Error::Other("密码里不能有 NUL".into()))?;
            // SAFETY：句柄有效，密码字符串在调用期间活着，unrar 内部会拷贝。
            unsafe { sys::RARSetPassword(handle.0.as_ptr(), cpw.as_ptr() as *const c_char) };
        }
        let (comment_state, comment_size, flags) = (data.comment_state, data.comment_size, data.flags);
        let comment = (comment_state == 1 && comment_size > 0).then(|| wide_to_string(&comment).into_bytes());
        Ok(Opened { handle, flags, comment })
    }

    /// 处理当前条目（SKIP / TEST）；数据通过回调交付。
    fn process(&self, operation: c_int) -> std::result::Result<(), RarCode> {
        // SAFETY：句柄有效；不传目标路径，数据只走回调。
        let code = unsafe { sys::RARProcessFileW(self.0.as_ptr(), operation, std::ptr::null(), std::ptr::null()) };
        if code == sys::ERAR_SUCCESS {
            Ok(())
        } else {
            Err(RarCode::from_raw(code))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY：句柄由 RAROpenArchiveEx 返回且只关闭一次。
        unsafe { sys::RARCloseArchive(self.0.as_ptr()) };
    }
}

/// 读头用的缓冲：RarHeaderData 有十几 KB，放堆上复用；redir 目标与长文件名缓冲每次都要重新挂上。
struct HeaderBuf {
    hd: Box<RarHeaderData>,
    redir: Vec<sys::WCHAR>,
    /// FileNameEx：不受 file_name_w 的 1024 字符限制。
    name_ex: Vec<sys::WCHAR>,
}

/// 从 unrar 头里取出来的字段，已转成 Rust 类型。
struct RawHeader {
    name: String,
    flags: u32,
    pack_size: u64,
    unp_size: u64,
    host_os: u32,
    crc: u32,
    dos_time: u32,
    mtime_filetime: u64,
    unp_ver: u32,
    method: u32,
    attr: u32,
    hash_is_crc32: bool,
    redir_type: u32,
    redir_name: Option<String>,
}

impl HeaderBuf {
    /// 分配缓冲。
    fn new() -> Self {
        // SAFETY：RarHeaderData 全零是合法值（整数 0、空指针、空数组）。
        let hd = Box::new(unsafe { std::mem::zeroed::<RarHeaderData>() });
        Self { hd, redir: vec![0; REDIR_BUF], name_ex: vec![0; NAME_EX_BUF] }
    }

    /// 读下一个文件头；到末尾返回 None。
    fn read(&mut self, handle: &Handle) -> std::result::Result<Option<RawHeader>, RarCode> {
        // SAFETY：同 new()。
        *self.hd = unsafe { std::mem::zeroed::<RarHeaderData>() };
        self.redir[0] = 0;
        self.name_ex[0] = 0;
        self.hd.redir_name = self.redir.as_mut_ptr();
        self.hd.redir_name_size = REDIR_BUF as c_uint;
        self.hd.file_name_ex = self.name_ex.as_mut_ptr();
        self.hd.file_name_ex_size = NAME_EX_BUF as c_uint;
        // SAFETY：句柄有效，hd 与两个缓冲在调用期间活着，unrar 按给定容量截断写入；
        // 指针类型按 unrar_sys 的声明转换，内存布局见 RarHeaderData 的说明。
        let code = unsafe { sys::RARReadHeaderEx(handle.0.as_ptr(), &mut *self.hd as *mut RarHeaderData as *const sys::HeaderDataEx) };
        match code {
            sys::ERAR_SUCCESS => {}
            sys::ERAR_END_ARCHIVE => return Ok(None),
            other => return Err(RarCode::from_raw(other)),
        }
        // packed 结构体的字段不能取引用，逐个按值拷出来
        let hd = &*self.hd;
        let (flags, host_os, redir_type) = (hd.flags, hd.host_os, hd.redir_type);
        let (pack_size, pack_size_high, unp_size, unp_size_high) = (hd.pack_size, hd.pack_size_high, hd.unp_size, hd.unp_size_high);
        let (file_crc, file_time, mtime_low, mtime_high) = (hd.file_crc, hd.file_time, hd.mtime_low, hd.mtime_high);
        let (unp_ver, method, file_attr, hash_type) = (hd.unp_ver, hd.method, hd.file_attr, hd.hash_type);
        let mut name = wide_to_string(&self.name_ex);
        if name.is_empty() {
            let short: [sys::WCHAR; 1024] = hd.file_name_w;
            name = wide_to_string(&short);
        }
        let redir_name = (redir_type != 0).then(|| wide_to_string(&self.redir)).filter(|s| !s.is_empty());
        Ok(Some(RawHeader {
            name,
            flags,
            pack_size: (u64::from(pack_size_high) << 32) | u64::from(pack_size),
            unp_size: (u64::from(unp_size_high) << 32) | u64::from(unp_size),
            host_os,
            crc: file_crc,
            dos_time: file_time,
            mtime_filetime: (u64::from(mtime_high) << 32) | u64::from(mtime_low),
            unp_ver,
            method,
            attr: file_attr,
            hash_is_crc32: hash_type == sys::RAR_HASH_CRC32,
            redir_type,
            redir_name,
        }))
    }
}

/// 把 unrar 头翻译成统一条目。文件名 unrar 已解码成 Unicode 且分隔符统一为 '/'。
fn entry_from_header(id: EntryId, h: &RawHeader, solid: bool) -> (Entry, RarKind) {
    let is_dir = h.flags & sys::RHDF_DIRECTORY != 0;
    let unix_mode = (h.host_os == HOST_UNIX).then_some(h.attr);
    let kind = if is_dir {
        RarKind::Dir
    } else {
        match h.redir_type {
            REDIR_UNIX_SYMLINK | REDIR_WIN_SYMLINK | REDIR_JUNCTION if h.redir_name.is_some() => RarKind::SymlinkRedir,
            REDIR_HARDLINK => RarKind::HardLink,
            REDIR_FILECOPY => RarKind::FileCopy,
            // RAR4 的 Unix 符号链接：unrar 只标 redir 类型不给目标（目标存成文件内容），
            // 老版本连类型都不标，只能看 mode 是不是 S_IFLNK
            REDIR_UNIX_SYMLINK => RarKind::SymlinkData,
            _ if unix_mode.is_some_and(|m| m & 0o170000 == 0o120000) => RarKind::SymlinkData,
            _ => RarKind::Regular,
        }
    };
    let public_kind = match kind {
        RarKind::Dir => EntryKind::Dir,
        RarKind::SymlinkRedir | RarKind::SymlinkData => EntryKind::Symlink,
        _ => EntryKind::File,
    };
    let mut e = Entry::new(id, h.name.replace('\\', "/").into_bytes(), public_kind, if is_dir { 0 } else { h.unp_size });
    e.name_is_utf8 = true;
    e.packed_size = Some(h.pack_size);
    e.mtime = filetime_to_unix(h.mtime_filetime).or_else(|| dos_time_to_unix(h.dos_time));
    e.crc32 = h.hash_is_crc32.then_some(h.crc);
    e.encrypted = h.flags & sys::RHDF_ENCRYPTED != 0;
    // method 字节是 0x30 + m：m0 是存储；其余按格式家族报 RAR5 / RAR4（unp_ver 50 起是 RAR5）
    e.method = if h.method == 0x30 {
        Method::Store
    } else if h.unp_ver >= 50 {
        Method::Rar(5)
    } else if h.unp_ver >= 20 {
        Method::Rar(4)
    } else {
        Method::Rar(1)
    };
    e.unix_mode = unix_mode;
    // solid 包里所有文件条目同属一个顺序组
    e.solid_group = (solid && !is_dir).then_some(0);
    if kind == RarKind::SymlinkRedir || kind == RarKind::HardLink || kind == RarKind::FileCopy {
        e.link_target = h.redir_name.as_ref().map(|s| s.replace('\\', "/").into_bytes());
    }
    (e, kind)
}

/// Windows FILETIME 转 Unix 秒；0 表示没有。
fn filetime_to_unix(ft: u64) -> Option<i64> {
    (ft != 0).then(|| (ft / 10_000_000) as i64 - 11_644_473_600)
}

/// DOS 时间（RAR4 旧头）转 Unix 秒，按 UTC 解释（RAR 不记录时区）。
fn dos_time_to_unix(t: u32) -> Option<i64> {
    if t == 0 {
        return None;
    }
    let year = i64::from(t >> 25) + 1980;
    let month = i64::from((t >> 21) & 0xF);
    let day = i64::from((t >> 16) & 0x1F);
    let hour = i64::from((t >> 11) & 0x1F);
    let min = i64::from((t >> 5) & 0x3F);
    let sec = i64::from(t & 0x1F) * 2;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Howard Hinnant 的 days_from_civil
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + min * 60 + sec)
}

/// Rust 字符串转 NUL 结尾的 wchar_t 数组（macOS/Linux 4 字节，Windows 2 字节 UTF-16）。
fn to_wide(s: &str) -> Vec<sys::WCHAR> {
    let mut out: Vec<sys::WCHAR> = if std::mem::size_of::<sys::WCHAR>() == 2 {
        s.encode_utf16().map(|u| u as sys::WCHAR).collect()
    } else {
        s.chars().map(|c| c as u32 as sys::WCHAR).collect()
    };
    out.push(0);
    out
}

/// NUL 结尾的 wchar_t 数组转 String，非法码点替换成 U+FFFD。
fn wide_to_string(w: &[sys::WCHAR]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    let w = &w[..end];
    if std::mem::size_of::<sys::WCHAR>() == 2 {
        char::decode_utf16(w.iter().map(|&c| c as u16)).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
    } else {
        w.iter().map(|&c| char::from_u32(c as u32).unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DOS 时间转换：2024-01-02 03:04:06 UTC。
    #[test]
    fn dos_time_conversion() {
        let t = (44u32 << 25) | (1 << 21) | (2 << 16) | (3 << 11) | (4 << 5) | 3;
        assert_eq!(dos_time_to_unix(t), Some(1_704_164_646));
        assert_eq!(dos_time_to_unix(0), None);
    }

    /// 宽字符来回转换保持 Unicode 不变。
    #[test]
    fn wide_roundtrip() {
        let s = "中文/❤️ name";
        assert_eq!(wide_to_string(&to_wide(s)), s);
    }
}
