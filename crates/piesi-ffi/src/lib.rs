//! UniFFI 导出层：只放导出函数和跨语言的 record/enum，不放逻辑。
//! 门面对象内部用 Mutex 包住 `Archive`（驱动是 Send 不是 Sync），长操作由调用方放到后台线程。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use piesi_core::create::CreateOptions;
use piesi_core::encoding::NameEncoding;
use piesi_core::facade::Archive;
use piesi_core::plan::{ConflictPolicy, ExtractOptions};
use piesi_core::progress::{CancelToken, OpContext, Progress, ProgressEvent};
use piesi_core::{DetailKind as CoreDetailKind, EntryKind, ErrorDetail as CoreErrorDetail, PasswordNeed, Warning as CoreWarning, WarningKind as CoreWarningKind};

uniffi::setup_scaffolding!();

/// 引擎版本号，给 About 页和 P0 的 FFI 连通性验证用。
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// 识别文件格式名；认不出返回 None。
#[uniffi::export]
pub fn detect_format_name(path: String) -> Option<String> {
    piesi_core::detect_format(Path::new(&path)).ok().flatten().map(|f| f.display_name().to_owned())
}

/// 跨语言错误。壳层按变体翻译标题；`detail` 是结构化细节，按 `DetailKind` 翻译；`message` 是系统错误等原文，原样附在后面。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PiesiError {
    #[error("无法识别的压缩格式")]
    UnknownFormat,
    /// 格式认得出但功能不支持（某种压缩方法、RAR 引用记录等）
    #[error("不支持：{detail}")]
    Unsupported { detail: ErrorDetail },
    /// 压缩包数据损坏或结构不一致
    #[error("压缩包损坏：{detail}")]
    Corrupt { detail: ErrorDetail },
    #[error("需要密码")]
    PasswordRequired,
    #[error("密码错误")]
    WrongPassword,
    #[error("已取消")]
    Cancelled,
    /// 条目路径不安全（绝对路径、`..`、盘符等）
    #[error("不安全的路径：{detail}")]
    UnsafePath { detail: ErrorDetail },
    /// 目标磁盘放不下
    #[error("磁盘空间不足：需要 {needed} 字节，可用 {available} 字节")]
    NoSpace { needed: u64, available: u64 },
    /// 读写文件时的系统错误（message 是系统给的英文文案）
    #[error("{message}")]
    Io { message: String },
    #[error("{message}")]
    Other { message: String },
}

impl From<piesi_core::Error> for PiesiError {
    fn from(e: piesi_core::Error) -> Self {
        use piesi_core::Error as E;
        match e {
            E::UnknownFormat => PiesiError::UnknownFormat,
            E::Unsupported(detail) => PiesiError::Unsupported { detail: detail.into() },
            E::Corrupt(detail) => PiesiError::Corrupt { detail: detail.into() },
            E::PasswordRequired => PiesiError::PasswordRequired,
            E::WrongPassword => PiesiError::WrongPassword,
            E::Cancelled => PiesiError::Cancelled,
            E::UnsafePath(detail) => PiesiError::UnsafePath { detail: detail.into() },
            E::NoSpace { needed, available } => PiesiError::NoSpace { needed, available },
            E::Io(e) => PiesiError::Io { message: e.to_string() },
            other => PiesiError::Other { message: other.to_string() },
        }
    }
}

/// 定义一个和核心枚举逐个对应的 FFI 枚举，并生成双向 `From`。
/// 为什么用宏：`DetailKind` 有几十个变体，手写定义 + 两个方向的 match 要抄三遍，容易漏；变体名对不上会直接编译失败。
macro_rules! mirror_enum {
    ($(#[$meta:meta])* pub enum $name:ident mirrors $core:ident { $($(#[$vmeta:meta])* $v:ident,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
        pub enum $name {
            $($(#[$vmeta])* $v,)*
        }

        impl From<$core> for $name {
            fn from(k: $core) -> Self {
                match k {
                    $($core::$v => $name::$v,)*
                }
            }
        }

        impl From<$name> for $core {
            fn from(k: $name) -> Self {
                match k {
                    $($name::$v => $core::$v,)*
                }
            }
        }
    };
}

mirror_enum! {
    /// 错误细节类型，和核心的 `DetailKind` 一一对应；文案由壳层按类型本地化（见核心 `detail.rs` 每个变体的说明）。
    pub enum DetailKind mirrors CoreDetailKind {
        /// 第三方库或系统给的文案，翻不了，原样显示。args = [原文]
        Library,
        /// 只有一个名字（格式名、路径），原样显示。args = [名字]
        Plain,

        // ---- 路径（UnsafePath）----
        /// 路径含控制字符。args = [转义后的路径]
        PathControlChars,
        /// 绝对路径。args = [路径]
        PathAbsolute,
        /// 带 Windows 盘符。args = [路径]
        PathDrivePrefix,
        /// 含 `.` / `..` 分量。args = [分量, 路径]
        PathDotComponent,
        /// 空路径。args = [带引号的原始名]
        PathEmpty,

        // ---- 通用 ----
        /// 条目序号越界
        EntryIndexOutOfRange,
        /// 条目数超过上限
        TooManyEntries,
        /// 符号链接目标过长。args = [条目名]
        SymlinkTargetTooLong,
        /// 压缩包内容与打开时建立的索引不一致
        IndexMismatch,
        /// 不支持的压缩方法。args = [方法]
        CompressionMethod,
        /// 解压失败（解码层的 io 错误）。args = [错误原文]
        DecompressFailed,

        // ---- RAR ----
        /// RAR 压缩包头损坏
        RarArchiveHeaderBad,
        /// RAR 文件头损坏
        RarFileHeaderBad,
        /// RAR 数据 CRC 校验失败
        RarCrcFailed,
        /// 不是 RAR 压缩包
        NotRar,
        /// 找不到下一个分卷
        NextVolumeMissing,
        /// RAR 读取错误
        RarReadError,
        /// RAR 未知的格式或加密方式
        RarUnknownFormat,
        /// RAR 引用记录需要完整解压
        RarReference,

        // ---- 7z ----
        /// 7z 条目引用了不存在的块
        SevenZMissingBlock,
        /// 7z 块与文件表对不上
        SevenZBlockMismatch,
        /// 7z 块内条目与目录不一致。args = [条目名]
        SevenZEntryMismatch,
        /// 7z 不支持的压缩方法。args = [方法]
        SevenZMethod,
        /// sevenz-rust2 给的文案，前面带「7z：」。args = [原文]
        SevenZLibrary,

        // ---- tar ----
        /// tar 驱动收到了非 tar 格式（内部错误）。args = [格式名]
        TarDriverMismatch,
        /// tar 某个条目读取失败。args = [序号（从 1 起）, 错误原文]
        TarEntryReadFailed,
        /// 写 tar 时给了非 tar 格式（内部错误）。args = [格式]
        NotTarFormat,

        // ---- zip 结构 ----
        /// 文件太短，不是 zip
        ZipTooShort,
        /// 读取文件尾部失败
        ZipTailReadFailed,
        /// 找不到中央目录结束记录（EOCD）
        EocdNotFound,
        /// 字段溢出但找不到 zip64 定位记录
        Zip64LocatorMissing,
        /// zip64 中央目录结束记录缺失或损坏
        Zip64EocdBad,
        /// 中央目录头不完整
        CdHeaderIncomplete,
        /// 中央目录条目名不完整
        CdNameIncomplete,
        /// 中央目录扩展字段不完整
        CdExtraIncomplete,
        /// 中央目录条目注释不完整
        CdCommentIncomplete,
        /// zip64 扩展字段缺少原始大小
        Zip64MissingSize,
        /// zip64 扩展字段缺少压缩后大小
        Zip64MissingPacked,
        /// zip64 扩展字段缺少本地头偏移
        Zip64MissingOffset,
        /// zip64 扩展字段缺少盘号
        Zip64MissingDisk,
        /// 中央目录越界
        CdOutOfRange,
        /// 中央目录位置不对，读不到任何条目
        CdMisplaced,
        /// 偏移溢出
        OffsetOverflow,
        /// 引用了不存在的分卷。args = [盘号]
        DiskMissing,
        /// 记录不完整（字段越界）
        RecordTruncated,
        /// 本地文件头签名不对
        LocalHeaderBadSignature,
        /// 本地文件头偏移越界。args = [偏移]
        LocalHeaderOffsetOutOfRange,
        /// 本地文件头不完整
        LocalHeaderIncomplete,
        /// 本地文件头越界
        LocalHeaderOutOfRange,
        /// 条目数据越界。args = [条目名]
        EntryDataOutOfRange,
        /// 数据提前结束。args = [条目名]
        DataTruncated,
        /// CRC 不匹配。args = [条目名, 期望值, 实际值]（8 位十六进制）
        CrcMismatch,
        /// 修复扫描不到任何完整条目
        RepairNoEntries,

        // ---- zip 加密 ----
        /// PKWARE 强加密不支持
        PkwareStrongEncryption,
        /// AES 条目缺少 0x9901 扩展字段
        AesExtraMissing,
        /// AES 密钥长度错误
        AesKeyLength,
        /// 不支持的 AES 强度。args = [强度值]
        AesStrength,
        /// HMAC 密钥长度错误
        HmacKeyLength,
        /// AES 条目长度不足
        AesEntryTooShort,
        /// AES 盐不完整
        AesSaltIncomplete,
        /// AES 口令校验值不完整
        AesVerifierIncomplete,
        /// AES 密文提前结束
        AesCiphertextTruncated,
        /// AES 认证码缺失
        AesAuthCodeMissing,
        /// AES 认证失败：数据被改动或密码错误
        AesAuthFailed,
        /// ZipCrypto 加密头不完整
        ZipCryptoHeaderIncomplete,

        // ---- zip 分卷 ----
        /// 分卷总长溢出
        VolumeTotalOverflow,
        /// 分卷数非法。args = [分卷数]
        VolumeCountInvalid,
        /// 缺少分卷。args = [分卷文件名]
        VolumeMissing,

        // ---- 编解码参数 ----
        /// 流头部不完整。args = [编解码器名，如 LZMA]
        StreamHeaderIncomplete,
        /// LZMA 属性字节非法
        LzmaPropsInvalid,
        /// LZMA 字典过大。args = [MB 数]
        LzmaDictTooLarge,
        /// LZMA 属性长度非法。args = [长度]
        LzmaPropsLength,
        /// LZMA 属性不足 5 字节
        LzmaPropsShort,
        /// LZMA2 缺少属性字节
        Lzma2PropsMissing,
        /// LZMA2 字典过大
        Lzma2DictTooLarge,
        /// 不支持的 PPMd 恢复模式。args = [模式号]
        PpmdRestoreMode,
        /// PPMd 参数非法。args = [错误原文]
        PpmdParamsInvalid,
        /// PPMd 内存需求过大。args = [MB 数]
        PpmdMemTooLarge,
        /// PPMd 属性不足 5 字节
        PpmdPropsShort,
    }
}

/// 错误细节：类型 + 参数（顺序见核心 `DetailKind` 各变体说明）。
#[derive(Clone, Debug, uniffi::Record)]
pub struct ErrorDetail {
    pub kind: DetailKind,
    pub args: Vec<String>,
}

impl From<CoreErrorDetail> for ErrorDetail {
    fn from(d: CoreErrorDetail) -> Self {
        ErrorDetail { kind: d.kind.into(), args: d.args }
    }
}

impl std::fmt::Display for ErrorDetail {
    /// 中文文案借核心的 Display（`PiesiError` 的 Display 要用它）；壳层不走这里。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        CoreErrorDetail { kind: self.kind.into(), args: self.args.clone() }.fmt(f)
    }
}

/// 条目名为什么不安全（解压计划跳过它时给的 `UnsafePathSkipped` 警告用）；名字安全返回 None。
/// 为什么单独导出：警告的 detail 是中文成句，壳层拿条目名（警告的 subject）重算一遍结构化原因再本地化，
/// 不用改 `Warning` 的形状；`sanitize_rel_path` 是纯函数，结果和计划时一致。
#[uniffi::export]
pub fn unsafe_path_reason(name: String) -> Option<ErrorDetail> {
    match piesi_core::names::sanitize_rel_path(&name) {
        Err(piesi_core::Error::UnsafePath(d)) => Some(d.into()),
        _ => None,
    }
}

/// 警告类型，和核心的 `WarningKind` 一一对应；文案由壳层按类型本地化（见核心 `warning.rs` 每个变体的说明）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum WarningKind {
    /// 条目路径不安全，解压计划跳过了它。subject = 条目名，detail = 原因
    UnsafePathSkipped,
    /// 同目录下两个名字只差大小写，后一个改了名。subject = 原名，detail = 新名
    CaseConflictRenamed,
    /// 硬链接暂不支持，已跳过。subject = 条目名
    HardLinkSkipped,
    /// 设备文件、管道等特殊条目，已跳过。subject = 条目名
    SpecialEntrySkipped,
    /// RAR 引用记录暂不支持，已跳过。subject = 条目名
    RarReferenceSkipped,
    /// 条目没有数据，已跳过。subject = 条目名
    NoDataSkipped,
    /// 符号链接指向目标目录之外，已跳过。subject = 条目名，detail = 链接目标
    SymlinkEscapeSkipped,
    /// unrar 拒绝还原符号链接，已跳过。subject = 条目名
    RarSymlinkRejected,
    /// 创建符号链接失败。subject = 条目名，detail = 系统错误
    SymlinkCreateFailed,
    /// RAR 跳过未选中条目时解码出错，已忽略。subject = 条目名，detail = unrar 错误码
    RarSkipDecodeError,
    /// 压缩时跳过了特殊文件。subject = 源路径
    SpecialFileSkipped,
    /// 「发给 Windows」改了名字。subject = 原名，detail = 新名
    RenamedForWindows,
    /// zip 不能加密文件名，只加密了内容
    ZipCannotEncryptNames,
    /// 读取 Mac 元数据失败。subject = 源路径，detail = 系统错误
    MacMetadataReadFailed,
    /// 资源分支超过 64 MB，已丢弃。subject = 源路径
    MacResourceForkTooLarge,
    /// 扩展属性放不进 AppleDouble，已丢弃。subject = 源路径，detail = 属性名
    MacXattrTooLarge,
    /// 扩展属性名太长，已丢弃。subject = 源路径，detail = 属性名
    MacXattrNameTooLong,
    /// Finder 信息不是 32 字节，已丢弃。subject = 源路径
    MacFinderInfoInvalid,
}

impl From<CoreWarningKind> for WarningKind {
    fn from(k: CoreWarningKind) -> Self {
        match k {
            CoreWarningKind::UnsafePathSkipped => WarningKind::UnsafePathSkipped,
            CoreWarningKind::CaseConflictRenamed => WarningKind::CaseConflictRenamed,
            CoreWarningKind::HardLinkSkipped => WarningKind::HardLinkSkipped,
            CoreWarningKind::SpecialEntrySkipped => WarningKind::SpecialEntrySkipped,
            CoreWarningKind::RarReferenceSkipped => WarningKind::RarReferenceSkipped,
            CoreWarningKind::NoDataSkipped => WarningKind::NoDataSkipped,
            CoreWarningKind::SymlinkEscapeSkipped => WarningKind::SymlinkEscapeSkipped,
            CoreWarningKind::RarSymlinkRejected => WarningKind::RarSymlinkRejected,
            CoreWarningKind::SymlinkCreateFailed => WarningKind::SymlinkCreateFailed,
            CoreWarningKind::RarSkipDecodeError => WarningKind::RarSkipDecodeError,
            CoreWarningKind::SpecialFileSkipped => WarningKind::SpecialFileSkipped,
            CoreWarningKind::RenamedForWindows => WarningKind::RenamedForWindows,
            CoreWarningKind::ZipCannotEncryptNames => WarningKind::ZipCannotEncryptNames,
            CoreWarningKind::MacMetadataReadFailed => WarningKind::MacMetadataReadFailed,
            CoreWarningKind::MacResourceForkTooLarge => WarningKind::MacResourceForkTooLarge,
            CoreWarningKind::MacXattrTooLarge => WarningKind::MacXattrTooLarge,
            CoreWarningKind::MacXattrNameTooLong => WarningKind::MacXattrNameTooLong,
            CoreWarningKind::MacFinderInfoInvalid => WarningKind::MacFinderInfoInvalid,
        }
    }
}

/// 一条警告：类型 + 主体（哪个条目或文件）+ 细节（链接目标、新名、系统错误等，可空）。
#[derive(Clone, Debug, uniffi::Record)]
pub struct Warning {
    pub kind: WarningKind,
    pub subject: String,
    pub detail: String,
}

impl From<CoreWarning> for Warning {
    fn from(w: CoreWarning) -> Self {
        Warning { kind: w.kind.into(), subject: w.subject, detail: w.detail }
    }
}

/// 核心的警告列表转成跨语言记录。
fn convert_warnings(warnings: Vec<CoreWarning>) -> Vec<Warning> {
    warnings.into_iter().map(Into::into).collect()
}

/// 文件名编码（和核心的 NameEncoding 一一对应）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Encoding {
    Utf8,
    Gb18030,
    Big5,
    ShiftJis,
    EucKr,
    Cp1251,
    Cp866,
    Cp1252,
}

impl From<Encoding> for NameEncoding {
    fn from(e: Encoding) -> Self {
        match e {
            Encoding::Utf8 => NameEncoding::Utf8,
            Encoding::Gb18030 => NameEncoding::Gb18030,
            Encoding::Big5 => NameEncoding::Big5,
            Encoding::ShiftJis => NameEncoding::ShiftJis,
            Encoding::EucKr => NameEncoding::EucKr,
            Encoding::Cp1251 => NameEncoding::Cp1251,
            Encoding::Cp866 => NameEncoding::Cp866,
            Encoding::Cp1252 => NameEncoding::Cp1252,
        }
    }
}

impl From<NameEncoding> for Encoding {
    fn from(e: NameEncoding) -> Self {
        match e {
            NameEncoding::Utf8 => Encoding::Utf8,
            NameEncoding::Gb18030 => Encoding::Gb18030,
            NameEncoding::Big5 => Encoding::Big5,
            NameEncoding::ShiftJis => Encoding::ShiftJis,
            NameEncoding::EucKr => Encoding::EucKr,
            NameEncoding::Cp1251 => Encoding::Cp1251,
            NameEncoding::Cp866 => Encoding::Cp866,
            NameEncoding::Cp1252 => Encoding::Cp1252,
        }
    }
}

/// 编码识别结果。
#[derive(Clone, Debug, uniffi::Record)]
pub struct EncodingInfo {
    pub encoding: Encoding,
    pub confidence: f32,
    /// "format" / "utf8" / "scored" / "manual"
    pub source: String,
    pub ambiguous: bool,
}

/// 条目类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

/// 列表里的一行。目录层级由 `list_dir` 现算，UI 不持有整棵树。
#[derive(Clone, Debug, uniffi::Record)]
pub struct Row {
    pub id: u32,
    /// 最后一段名字
    pub name: String,
    /// 完整包内路径
    pub path: String,
    pub kind: Kind,
    pub size: u64,
    pub packed_size: Option<u64>,
    pub mtime: Option<i64>,
    pub encrypted: bool,
    pub method: String,
    /// 目录行：下面有多少个直接子项
    pub child_count: u32,
}

/// 密码需求。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PasswordState {
    NotNeeded,
    ForData,
    ForNames,
}

/// 冲突策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Conflict {
    Overwrite,
    Skip,
    Rename,
}

/// 解压选项。
#[derive(Clone, Debug, uniffi::Record)]
pub struct ExtractRequest {
    /// 目标目录；空 = 压缩包所在目录
    pub dest: String,
    pub smart_folder: bool,
    pub conflict: Conflict,
    pub skip_junk: bool,
    /// 空 = 全部
    pub ids: Vec<u32>,
    /// 顶层文件夹已存在时：true = 解进已有文件夹，false = 另建 `名字-2`（见 ExtractPlanInfo.existing_folder）
    pub merge_into_existing: bool,
}

/// 解压结果。
#[derive(Clone, Debug, uniffi::Record)]
pub struct ExtractResult {
    pub root: String,
    /// 解出来的顶层项目的完整路径，完成后在 Finder 里选中
    pub top_level: Vec<String>,
    pub extracted: u64,
    pub skipped: u64,
    pub skipped_junk: u64,
    pub warnings: Vec<Warning>,
}

/// 解压前的计划摘要：给确认对话框、磁盘空间预检、压缩炸弹预警用，不落盘。
#[derive(Clone, Debug, uniffi::Record)]
pub struct ExtractPlanInfo {
    /// 实际落盘的根目录（智能建文件夹时是 dest/<压缩包名>）
    pub root: String,
    /// 计划解出的条目数
    pub entries: u64,
    /// 展开后的总字节数
    pub total_bytes: u64,
    /// 压缩包自身大小（分卷只算打开的那一卷）
    pub archive_bytes: u64,
    /// 目标里已存在、会按冲突策略处理的相对路径
    pub conflicts: Vec<String>,
    pub warnings: Vec<Warning>,
    pub skipped_junk: u64,
    /// 顶层文件夹在目标里已存在时，那个文件夹的名字；UI 据此问「另建还是解进去」
    pub existing_folder: Option<String>,
    /// 不合并时会用的新文件夹名（`名字-2`…）
    pub new_folder: Option<String>,
}

/// 校验结果。
#[derive(Clone, Debug, uniffi::Record)]
pub struct VerifyResult {
    /// 校验过的条目数
    pub entries: u64,
    pub skipped: u64,
    pub warnings: Vec<Warning>,
}

/// 进度快照。
#[derive(Clone, Debug, uniffi::Record)]
pub struct ProgressInfo {
    pub done_bytes: u64,
    pub total_bytes: Option<u64>,
    pub done_entries: u64,
    pub total_entries: Option<u64>,
    pub current: Option<String>,
}

/// 由 Swift 实现的进度回调。Rust 侧已节流（≥50 ms 或 ≥4 MB 才调一次）。
#[uniffi::export(with_foreign)]
pub trait ProgressListener: Send + Sync {
    /// 收到一次进度。
    fn on_progress(&self, info: ProgressInfo);
}

/// 把回调适配成核心的 Progress。
struct ListenerAdapter(Arc<dyn ProgressListener>);

impl Progress for ListenerAdapter {
    fn report(&self, e: &ProgressEvent) {
        self.0.on_progress(ProgressInfo {
            done_bytes: e.done_bytes,
            total_bytes: e.total_bytes,
            done_entries: e.done_entries,
            total_entries: e.total_entries,
            current: e.current.clone(),
        });
    }
}

/// 取消句柄：UI 持有它，点取消时调用 `cancel()`。
#[derive(uniffi::Object)]
pub struct CancelHandle {
    token: CancelToken,
}

#[uniffi::export]
impl CancelHandle {
    /// 新建。
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self { token: CancelToken::new() })
    }

    /// 请求取消。
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// 是否已取消。
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// 已打开的压缩包。
#[derive(uniffi::Object)]
pub struct ArchiveHandle {
    inner: Mutex<Archive>,
}

/// 打开压缩包并自动识别编码。
#[uniffi::export]
pub fn open_archive(path: String, locale: Option<String>) -> Result<Arc<ArchiveHandle>, PiesiError> {
    let archive = match locale {
        Some(l) => Archive::open(Path::new(&path), Some(&l))?,
        None => Archive::open_system(Path::new(&path))?,
    };
    Ok(Arc::new(ArchiveHandle { inner: Mutex::new(archive) }))
}

#[uniffi::export]
impl ArchiveHandle {
    /// 格式显示名。
    pub fn format_name(&self) -> String {
        self.inner.lock().unwrap().format().display_name().to_owned()
    }

    /// 条目总数。
    pub fn entry_count(&self) -> u32 {
        self.inner.lock().unwrap().entries().len() as u32
    }

    /// `dir` 下（含所有子目录）全部条目的 id，一次遍历；dir 传 "" 是整包。
    /// 给「解压选中的目录」收集条目用：在 Swift 里递归 list_dir 每层都要全量扫一遍，10 万条目会卡主线程。
    pub fn ids_under(&self, dir: String) -> Vec<u32> {
        let a = self.inner.lock().unwrap();
        let prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_matches('/')) };
        a.names()
            .iter()
            .enumerate()
            .filter(|(_, n)| prefix.is_empty() || n.starts_with(&prefix))
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// 全部文件条目的原始大小之和（目录、链接不算），给摘要行用。
    pub fn total_size(&self) -> u64 {
        let a = self.inner.lock().unwrap();
        a.entries().iter().filter(|e| e.kind == EntryKind::File).map(|e| e.size).sum()
    }

    /// 当前编码识别结果。
    pub fn encoding_info(&self) -> EncodingInfo {
        let a = self.inner.lock().unwrap();
        let d = a.detection();
        EncodingInfo {
            encoding: d.encoding.into(),
            confidence: d.confidence,
            source: format!("{:?}", d.source).to_lowercase(),
            ambiguous: d.ambiguous,
        }
    }

    /// 手动切换编码，只重解码不重读包。
    pub fn set_encoding(&self, encoding: Encoding) {
        self.inner.lock().unwrap().set_encoding(encoding.into());
    }

    /// 前 `limit` 个非 ASCII 名字的样本，编码选择器用来实时预览。
    pub fn sample_names(&self, limit: u32) -> Vec<String> {
        let a = self.inner.lock().unwrap();
        a.names().iter().filter(|n| !n.is_ascii()).take(limit as usize).cloned().collect()
    }

    /// 密码状态。
    pub fn password_state(&self) -> PasswordState {
        match self.inner.lock().unwrap().needs_password() {
            PasswordNeed::None => PasswordState::NotNeeded,
            PasswordNeed::ForData => PasswordState::ForData,
            PasswordNeed::ForNames => PasswordState::ForNames,
        }
    }

    /// 是否有条目用的是弱加密（zip 的老式 ZipCrypto，已知可破解）。要密码时 UI 据此提示「这不安全，只能读」。
    pub fn has_weak_encryption(&self) -> bool {
        self.inner.lock().unwrap().has_weak_encryption()
    }

    /// 设置密码。
    pub fn set_password(&self, password: Option<String>) -> Result<(), PiesiError> {
        self.inner.lock().unwrap().set_password(password.as_deref())?;
        Ok(())
    }

    /// 列出某个目录层级（`dir` 为空 = 顶层）。目录行带子项计数；没有显式目录条目的中间目录也会合成出来。
    pub fn list_dir(&self, dir: String) -> Vec<Row> {
        let a = self.inner.lock().unwrap();
        let prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_matches('/')) };
        let mut rows: Vec<Row> = Vec::new();
        let mut index_of: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (e, full) in a.entries().iter().zip(a.names()) {
            let full = full.trim_end_matches('/');
            let Some(rest) = full.strip_prefix(prefix.as_str()) else { continue };
            if rest.is_empty() {
                continue;
            }
            match rest.split_once('/') {
                None => {
                    // 直接子项
                    let row = Row {
                        id: e.id,
                        name: rest.to_owned(),
                        path: full.to_owned(),
                        kind: match e.kind {
                            EntryKind::File => Kind::File,
                            EntryKind::Dir => Kind::Dir,
                            EntryKind::Symlink => Kind::Symlink,
                        },
                        size: e.size,
                        packed_size: e.packed_size,
                        mtime: e.mtime,
                        encrypted: e.encrypted,
                        method: e.method.to_string(),
                        child_count: 0,
                    };
                    match index_of.get(rest) {
                        Some(&i) => {
                            // 已由子项合成过目录行：补上真实 id 和元数据，保留计数
                            let count = rows[i].child_count;
                            rows[i] = Row { child_count: count, ..row };
                        }
                        None => {
                            index_of.insert(rest.to_owned(), rows.len());
                            rows.push(row);
                        }
                    }
                }
                Some((top, remainder)) => {
                    // 更深的条目：确保顶层目录行存在，并把直接子项计数加一（只数一层）
                    let i = match index_of.get(top) {
                        Some(&i) => i,
                        None => {
                            index_of.insert(top.to_owned(), rows.len());
                            rows.push(Row {
                                id: u32::MAX,
                                name: top.to_owned(),
                                path: format!("{prefix}{top}"),
                                kind: Kind::Dir,
                                size: 0,
                                packed_size: None,
                                mtime: None,
                                encrypted: false,
                                method: String::new(),
                                child_count: 0,
                            });
                            rows.len() - 1
                        }
                    };
                    if !remainder.contains('/') {
                        rows[i].child_count += 1;
                    }
                }
            }
        }
        rows
    }

    /// 平铺搜索：名字包含关键字（不区分大小写），最多 `limit` 条。
    pub fn search(&self, query: String, limit: u32) -> Vec<Row> {
        let a = self.inner.lock().unwrap();
        let q = query.to_lowercase();
        a.entries()
            .iter()
            .zip(a.names())
            .filter(|(_, n)| n.to_lowercase().contains(&q))
            .take(limit as usize)
            .map(|(e, n)| Row {
                id: e.id,
                name: n.rsplit('/').next().unwrap_or(n).to_owned(),
                path: n.clone(),
                kind: match e.kind {
                    EntryKind::File => Kind::File,
                    EntryKind::Dir => Kind::Dir,
                    EntryKind::Symlink => Kind::Symlink,
                },
                size: e.size,
                packed_size: e.packed_size,
                mtime: e.mtime,
                encrypted: e.encrypted,
                method: e.method.to_string(),
                child_count: 0,
            })
            .collect()
    }

    /// 解压。阻塞调用，Swift 侧放到后台线程；进度经 listener 回调，取消经 cancel。
    pub fn extract(&self, request: ExtractRequest, listener: Arc<dyn ProgressListener>, cancel: Arc<CancelHandle>) -> Result<ExtractResult, PiesiError> {
        let mut a = self.inner.lock().unwrap();
        let opts = extract_options(a.path(), &request);
        let progress = ListenerAdapter(listener);
        let ctx = OpContext::new(&progress, &cancel.token);
        let selected = if request.ids.is_empty() { None } else { Some(request.ids.as_slice()) };
        let outcome = a.extract(selected, &opts, &ctx)?;
        Ok(ExtractResult {
            root: outcome.root.to_string_lossy().into_owned(),
            top_level: outcome.top_level.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            extracted: outcome.report.extracted,
            skipped: outcome.report.skipped,
            skipped_junk: outcome.skipped_junk,
            warnings: convert_warnings(outcome.report.warnings),
        })
    }

    /// 只算计划不落盘：根目录、总大小、冲突清单。UI 据此弹冲突 / 磁盘空间 / 压缩炸弹提示。
    pub fn plan(&self, request: ExtractRequest) -> Result<ExtractPlanInfo, PiesiError> {
        let a = self.inner.lock().unwrap();
        let opts = extract_options(a.path(), &request);
        let selected = if request.ids.is_empty() { None } else { Some(request.ids.as_slice()) };
        let plan = a.plan(selected, &opts)?;
        let archive_bytes = std::fs::metadata(a.path()).map(|m| m.len()).unwrap_or(0);
        Ok(ExtractPlanInfo {
            root: plan.root.to_string_lossy().into_owned(),
            entries: plan.items.len() as u64,
            total_bytes: plan.total_bytes,
            archive_bytes,
            conflicts: plan.conflicts,
            warnings: convert_warnings(plan.warnings),
            skipped_junk: plan.skipped_junk,
            existing_folder: plan.existing_folder,
            new_folder: plan.new_folder,
        })
    }

    /// 校验压缩包（解码 + CRC，不写盘）。ids 空 = 全部。长操作，放后台线程；数据损坏以错误返回。
    pub fn verify(&self, ids: Vec<u32>, listener: Arc<dyn ProgressListener>, cancel: Arc<CancelHandle>) -> Result<VerifyResult, PiesiError> {
        let mut a = self.inner.lock().unwrap();
        let progress = ListenerAdapter(listener);
        let ctx = OpContext::new(&progress, &cancel.token);
        let selected = if ids.is_empty() { None } else { Some(ids.as_slice()) };
        let report = a.verify(selected, &ctx)?;
        Ok(VerifyResult { entries: report.extracted, skipped: report.skipped, warnings: convert_warnings(report.warnings) })
    }

    /// 把单个条目解到临时文件（Quick Look 预览用），返回临时文件路径。
    pub fn extract_entry_to_temp(&self, id: u32, cancel: Arc<CancelHandle>) -> Result<String, PiesiError> {
        let mut a = self.inner.lock().unwrap();
        let name = a.names().get(id as usize).cloned().ok_or(PiesiError::Other { message: format!("条目不存在：{id}") })?;
        let file_name = name.rsplit('/').next().unwrap_or("entry").to_owned();
        let dir = std::env::temp_dir().join(format!("piesi-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| PiesiError::Other { message: e.to_string() })?;
        let path = dir.join(format!("{id}-{file_name}"));
        let mut out = std::fs::File::create(&path).map_err(|e| PiesiError::Other { message: e.to_string() })?;
        let progress = piesi_core::progress::NoProgress;
        let ctx = OpContext::new(&progress, &cancel.token);
        a.read_entry(id, &mut out, &ctx)?;
        Ok(path.to_string_lossy().into_owned())
    }
}

/// 把跨语言的解压请求转成核心选项；dest 为空时解到压缩包所在目录。
fn extract_options(archive: &Path, request: &ExtractRequest) -> ExtractOptions {
    let mut opts = ExtractOptions::beside_archive(archive);
    if !request.dest.is_empty() {
        opts.dest = PathBuf::from(&request.dest);
    }
    opts.smart_folder = request.smart_folder;
    opts.skip_junk = request.skip_junk;
    opts.merge_into_existing = request.merge_into_existing;
    opts.conflict = match request.conflict {
        Conflict::Overwrite => ConflictPolicy::Overwrite,
        Conflict::Skip => ConflictPolicy::Skip,
        Conflict::Rename => ConflictPolicy::Rename,
    };
    opts
}

/// 可创建的格式（和核心 CreateFormat 一一对应）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CreateFormat {
    Zip,
    SevenZ,
    TarGz,
    TarXz,
    TarZst,
}

impl From<CreateFormat> for piesi_core::create::CreateFormat {
    fn from(f: CreateFormat) -> Self {
        match f {
            CreateFormat::Zip => Self::Zip,
            CreateFormat::SevenZ => Self::SevenZ,
            CreateFormat::TarGz => Self::TarGz,
            CreateFormat::TarXz => Self::TarXz,
            CreateFormat::TarZst => Self::TarZst,
        }
    }
}

/// 格式的默认扩展名（zip / 7z / tar.gz / tar.xz / tar.zst），UI 拼默认文件名用。
#[uniffi::export]
pub fn format_extension(format: CreateFormat) -> String {
    piesi_core::create::CreateFormat::from(format).extension().to_owned()
}

/// 压缩请求。
#[derive(Clone, Debug, uniffi::Record)]
pub struct CreateRequest {
    /// 输出文件完整路径；已存在会被覆盖，UI 先确认
    pub dest: String,
    /// 源文件 / 目录的绝对路径
    pub sources: Vec<String>,
    pub format: CreateFormat,
    /// 0 = 仅存储，1 最快 … 9 最高
    pub level: u8,
    /// 密码：zip 用 WinZip AES-256（AE-1 / AE-2），7z 用 AES-256；None = 不加密
    pub password: Option<String>,
    /// 7z：连文件名一起加密
    pub encrypt_names: bool,
    /// 跳过 .DS_Store、__MACOSX 等垃圾
    pub skip_junk: bool,
    /// 「发给 Windows」：改成 Windows 安全名
    pub windows_safe: bool,
    /// 「Mac 专用」：保留 xattr / 资源分支 / Finder 信息。zip 写 `__MACOSX/._名字`，tar.* 写同目录 `._名字` + PAX xattr，7z 忽略。
    /// 带默认值，生成的 Swift 构造器里是可选参数，老调用点不用改
    #[uniffi(default = false)]
    pub mac_metadata: bool,
}

/// 压缩结果。
#[derive(Clone, Debug, uniffi::Record)]
pub struct CreateResult {
    pub entries: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub skipped_junk: u64,
    pub warnings: Vec<Warning>,
}

/// 创建压缩包。长操作，调用方放后台线程；进度经 listener 节流回调，cancel 可中断。
/// 失败或取消时删掉写了一半的输出文件，不留残件。
#[uniffi::export]
pub fn create_archive(request: CreateRequest, listener: Arc<dyn ProgressListener>, cancel: Arc<CancelHandle>) -> Result<CreateResult, PiesiError> {
    let opts = CreateOptions {
        format: request.format.into(),
        level: request.level,
        password: request.password,
        encrypt_names: request.encrypt_names,
        skip_junk: request.skip_junk,
        windows_safe: request.windows_safe,
        mac_metadata: request.mac_metadata,
        ..CreateOptions::default()
    };
    let roots: Vec<PathBuf> = request.sources.iter().map(PathBuf::from).collect();
    let dest = Path::new(&request.dest);
    let progress = ListenerAdapter(listener);
    let ctx = OpContext::new(&progress, &cancel.token);
    match piesi_core::create::create_archive(dest, &roots, &opts, &ctx) {
        Ok(report) => Ok(CreateResult {
            entries: report.entries,
            input_bytes: report.input_bytes,
            output_bytes: report.output_bytes,
            skipped_junk: report.skipped_junk,
            warnings: convert_warnings(report.warnings),
        }),
        Err(e) => {
            let _ = std::fs::remove_file(dest);
            Err(e.into())
        }
    }
}
