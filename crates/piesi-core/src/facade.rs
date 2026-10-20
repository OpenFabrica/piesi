//! 对外门面：打开压缩包 → 识别文件名编码 → 解码 → 规划 → 安全解压。
//! UI、CLI、FFI 都只经过这一层，不直接碰驱动。

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::archive::{ArchiveReader, Entry, EntryId, EntrySink, ExtractReport, Format, IndexState, PasswordNeed};
use crate::encoding::{decode_all, detect, Detection, NameEncoding};
use crate::fs::{read_quarantine, DirSink, FileSink};
use crate::plan::{plan_extract, ExtractOptions, ExtractPlan, PlanSink};
use crate::progress::OpContext;
use crate::Result;

/// 一次解压的结果：驱动报告 + 实际根目录 + 计划阶段的信息。
#[derive(Debug)]
pub struct ExtractOutcome {
    pub report: ExtractReport,
    pub root: PathBuf,
    pub skipped_junk: u64,
    pub conflicts: Vec<String>,
    /// 解出来的顶层项目（见 ExtractPlan::top_level）
    pub top_level: Vec<PathBuf>,
}

/// 已打开的压缩包。
pub struct Archive {
    path: PathBuf,
    reader: Box<dyn ArchiveReader>,
    encoding: NameEncoding,
    detection: Detection,
    names: Vec<String>,
    locale: Option<String>,
}

impl Archive {
    /// 打开并自动识别编码，地区先验取系统环境变量（CLI 默认用这个）。
    pub fn open_system(path: &Path) -> Result<Self> {
        let locale = system_locale();
        Self::open(path, locale.as_deref())
    }

    /// 打开并自动识别编码。`locale` 形如 "zh-CN"，None 表示不用地区先验（测试与 UI 显式传入时用）。
    pub fn open(path: &Path, locale: Option<&str>) -> Result<Self> {
        let reader = crate::open(path)?;
        let mut archive = Self {
            path: path.to_path_buf(),
            reader,
            encoding: NameEncoding::Utf8,
            detection: Detection::manual(NameEncoding::Utf8),
            names: Vec::new(),
            locale: locale.map(str::to_owned),
        };
        archive.redetect();
        Ok(archive)
    }

    /// 重新识别编码并解码全部名字（打开时、设密码后调用）。
    fn redetect(&mut self) {
        let mut hints = self.reader.hints();
        if hints.locale.is_none() {
            hints.locale = self.locale.clone();
        }
        self.detection = detect(self.reader.entries(), &hints);
        self.encoding = self.detection.encoding;
        self.redecode();
    }

    /// 按当前编码重解码所有名字，不重读包。整包一起解：bit 11 可信度要看全部条目。
    fn redecode(&mut self) {
        self.names = decode_all(self.reader.entries(), self.encoding);
    }

    /// 压缩包路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 格式。
    pub fn format(&self) -> Format {
        self.reader.format()
    }

    /// 全部条目。
    pub fn entries(&self) -> &[Entry] {
        self.reader.entries()
    }

    /// 与 `entries()` 对齐的已解码名字。
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// 当前编码识别结果。
    pub fn detection(&self) -> &Detection {
        &self.detection
    }

    /// 当前采用的编码。
    pub fn encoding(&self) -> NameEncoding {
        self.encoding
    }

    /// 用户手动切换编码：只重解码，不重读包。
    pub fn set_encoding(&mut self, enc: NameEncoding) {
        self.encoding = enc;
        self.detection = Detection::manual(enc);
        self.redecode();
    }

    /// 索引状态。
    pub fn index_state(&self) -> IndexState {
        self.reader.index_state()
    }

    /// 是否需要密码。
    pub fn needs_password(&self) -> PasswordNeed {
        self.reader.needs_password()
    }

    /// 是否有条目用的是弱加密（zip 的老式 ZipCrypto，已知可破解）。要密码时 UI 据此提示「这不安全」。
    pub fn has_weak_encryption(&self) -> bool {
        self.entries().iter().any(|e| e.weak_encryption)
    }

    /// 设置密码；头加密的包设完才有条目，所以随后重新识别编码。
    pub fn set_password(&mut self, password: Option<&str>) -> Result<()> {
        self.reader.set_password(password)?;
        self.redetect();
        Ok(())
    }

    /// 只做计划不解压，UI 用来预览冲突和总大小。
    pub fn plan(&self, selected: Option<&[EntryId]>, opts: &ExtractOptions) -> Result<ExtractPlan> {
        plan_extract(self.reader.entries(), &self.names, selected, opts)
    }

    /// 按计划解压到真实目录。
    pub fn extract(&mut self, selected: Option<&[EntryId]>, opts: &ExtractOptions, ctx: &OpContext<'_>) -> Result<ExtractOutcome> {
        let plan = self.plan(selected, opts)?;
        let quarantine = read_quarantine(&self.path);
        let mut dir_sink = DirSink::create(&plan.root, quarantine)?;
        let ids = plan.ids();
        let mut report = {
            let mut plan_sink = PlanSink::new(&plan, &mut dir_sink);
            self.reader.extract(Some(&ids), &mut plan_sink, ctx)?
        };
        let fs_report = dir_sink.finish()?;
        // 驱动报告里的 extracted 以落盘为准，警告两边合并
        report.extracted = fs_report.extracted;
        report.skipped += fs_report.skipped;
        report.warnings.extend(fs_report.warnings);
        report.warnings.extend(plan.warnings.iter().cloned());
        Ok(ExtractOutcome { report, root: plan.root.clone(), skipped_junk: plan.skipped_junk, conflicts: plan.conflicts.clone(), top_level: plan.top_level.clone() })
    }

    /// 校验压缩包：把全部（或选中）条目解到丢弃式接收端，驱动照常走完解码和 CRC 校验，但不写盘。
    /// 返回的 extracted 是校验过的条目数；数据损坏、密码错误都以 Err 返回。
    pub fn verify(&mut self, selected: Option<&[EntryId]>, ctx: &OpContext<'_>) -> Result<ExtractReport> {
        let mut sink = DiscardSink;
        self.reader.extract(selected, &mut sink, ctx)
    }

    /// 读取单个条目内容（预览用）。
    pub fn read_entry(&mut self, id: EntryId, out: &mut dyn std::io::Write, ctx: &OpContext<'_>) -> Result<()> {
        self.reader.read_entry(id, out, ctx)
    }
}

/// 只读不落盘的接收端：文件内容读完就丢，目的是让驱动完整跑一遍解码与 CRC。
struct DiscardSink;

impl EntrySink for DiscardSink {
    fn dir(&mut self, _entry: &Entry) -> Result<()> {
        Ok(())
    }

    fn file(&mut self, _entry: &Entry, data: &mut dyn Read) -> Result<u64> {
        Ok(std::io::copy(data, &mut std::io::sink())?)
    }

    fn symlink(&mut self, _entry: &Entry, _target: &[u8]) -> Result<()> {
        Ok(())
    }
}

/// 从环境变量取系统地区，"zh_CN.UTF-8" → "zh-CN"。
pub fn system_locale() -> Option<String> {
    let raw = std::env::var("LC_ALL").ok().filter(|s| !s.is_empty()).or_else(|| std::env::var("LANG").ok())?;
    let base = raw.split('.').next()?.trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return None;
    }
    Some(base.replace('_', "-"))
}
