//! 格式无关的核心数据模型：条目、格式、驱动 trait、解压接收端。
//! 所有格式驱动（zip/7z/rar/tar）都实现 `ArchiveReader`，上层只认这些类型。

use std::io::Read;

use crate::progress::OpContext;
use crate::warning::Warning;
use crate::Result;

/// 条目 id = 在 `entries` 数组里的下标，跨 FFI 只传这个 u32。
pub type EntryId = u32;

/// 识别出的容器格式（压缩流套在 tar 外面的也算独立格式，便于 UI 显示和分发驱动）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Format {
    Zip,
    SevenZ,
    Rar,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
    Gz,
    Bz2,
    Xz,
    Zst,
}

impl Format {
    /// 是否有「只读目录区就能列目录」的即时索引（zip/7z/rar 有，tar 系没有）。
    pub fn has_instant_index(self) -> bool {
        matches!(self, Format::Zip | Format::SevenZ | Format::Rar)
    }

    /// 是否是 tar 容器（含各种压缩外壳）。
    pub fn is_tar(self) -> bool {
        matches!(self, Format::Tar | Format::TarGz | Format::TarBz2 | Format::TarXz | Format::TarZst)
    }

    /// 是否是单文件压缩流（gz/bz2/xz/zst 直接包一个文件）。
    pub fn is_single_stream(self) -> bool {
        matches!(self, Format::Gz | Format::Bz2 | Format::Xz | Format::Zst)
    }

    /// 给 UI 显示的名字。
    pub fn display_name(self) -> &'static str {
        match self {
            Format::Zip => "ZIP",
            Format::SevenZ => "7z",
            Format::Rar => "RAR",
            Format::Tar => "TAR",
            Format::TarGz => "TAR.GZ",
            Format::TarBz2 => "TAR.BZ2",
            Format::TarXz => "TAR.XZ",
            Format::TarZst => "TAR.ZST",
            Format::Gz => "GZIP",
            Format::Bz2 => "BZIP2",
            Format::Xz => "XZ",
            Format::Zst => "Zstandard",
        }
    }
}

/// 条目类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// 条目的压缩方法，给 UI 显示和 plan 估算用。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Method {
    Store,
    Deflate,
    Deflate64,
    Bzip2,
    Lzma,
    Lzma2,
    Xz,
    Zstd,
    Ppmd,
    /// RAR 各版本的方法号
    Rar(u8),
    Other(String),
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Method::Store => write!(f, "Store"),
            Method::Deflate => write!(f, "Deflate"),
            Method::Deflate64 => write!(f, "Deflate64"),
            Method::Bzip2 => write!(f, "BZip2"),
            Method::Lzma => write!(f, "LZMA"),
            Method::Lzma2 => write!(f, "LZMA2"),
            Method::Xz => write!(f, "XZ"),
            Method::Zstd => write!(f, "Zstandard"),
            Method::Ppmd => write!(f, "PPMd"),
            Method::Rar(v) => write!(f, "RAR{v}"),
            Method::Other(s) => write!(f, "{s}"),
        }
    }
}

/// 一个压缩包条目。名字保留原始字节，解码放在 `encoding` 模块，
/// 这样用户切换编码时只重解码、不重读包。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub id: EntryId,
    /// 格式里存的原始路径字节（zip/tar 可能是任意代码页；7z/rar 已是 UTF-8）。分隔符已统一为 `/`。
    pub raw_name: Vec<u8>,
    /// 格式层已确认是 UTF-8：zip 的 bit 11、7z、rar、tar 的 pax 头。
    pub name_is_utf8: bool,
    /// zip 0x7075（Info-ZIP Unicode Path）扩展字段里的 UTF-8 名，CRC 校验通过才填。
    pub unicode_name: Option<String>,
    pub kind: EntryKind,
    /// 展开后大小。
    pub size: u64,
    /// 包内占用大小，格式不提供时为 None。
    pub packed_size: Option<u64>,
    /// 修改时间，Unix 秒。
    pub mtime: Option<i64>,
    pub crc32: Option<u32>,
    pub encrypted: bool,
    /// 加密方式已知可破解（zip 的老式 ZipCrypto）：UI 要密码时据此提示「这不安全」。AES zip、7z、rar 都是 false。
    #[serde(default)]
    pub weak_encryption: bool,
    pub method: Method,
    /// Unix 权限位（含类型位），zip 的 Unix 产物和 tar/7z 常有。
    pub unix_mode: Option<u32>,
    /// 同一组必须按顺序解压（7z 的 folder、rar 的 solid 块）；None 表示独立可随机访问。
    pub solid_group: Option<u32>,
    /// 符号链接目标（原始字节）。
    pub link_target: Option<Vec<u8>>,
}

impl Entry {
    /// 新建一个只填必填字段的条目，其余由驱动按格式补齐。
    pub fn new(id: EntryId, raw_name: Vec<u8>, kind: EntryKind, size: u64) -> Self {
        Self {
            id,
            raw_name,
            name_is_utf8: false,
            unicode_name: None,
            kind,
            size,
            packed_size: None,
            mtime: None,
            crc32: None,
            encrypted: false,
            weak_encryption: false,
            method: Method::Store,
            unix_mode: None,
            solid_group: None,
            link_target: None,
        }
    }

    /// 原始名字是否以 `/` 结尾（zip 用它标目录）。
    pub fn raw_ends_with_slash(&self) -> bool {
        self.raw_name.last() == Some(&b'/')
    }
}

/// 索引状态：zip/7z/rar 打开即完整；tar 系要边读边建，UI 据此显示进度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexState {
    /// 读完目录区即得到全部条目。
    Complete,
    /// 还在扫描，`done_bytes/total_bytes` 给 UI 画进度。
    Building { done_bytes: u64, total_bytes: Option<u64> },
}

/// 密码需求：无 / 解数据要 / 连文件名都加密了（7z 头加密、rar -hp）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordNeed {
    None,
    ForData,
    ForNames,
}

/// 解压报告：成功数、跳过数与逐条警告（比如 symlink 逃逸被跳过、文件名被截断）。
#[derive(Clone, Debug, Default)]
pub struct ExtractReport {
    pub extracted: u64,
    pub skipped: u64,
    pub warnings: Vec<Warning>,
}

/// 驱动把条目内容交给它。实现方负责决定落盘路径与安全检查，驱动只管按顺序吐数据。
/// 用「拉」模型（驱动给 Read，接收端自己 copy），solid 7z/rar 顺序解压时最自然。
pub trait EntrySink {
    /// 创建目录。
    fn dir(&mut self, entry: &Entry) -> Result<()>;
    /// 写入文件内容，返回实际写入字节数。
    fn file(&mut self, entry: &Entry, data: &mut dyn Read) -> Result<u64>;
    /// 记录符号链接，接收端通常延后到最后再真正创建。
    fn symlink(&mut self, entry: &Entry, target: &[u8]) -> Result<()>;
}

/// 格式驱动统一接口。`entries()` 的顺序就是包内原始顺序，id 等于下标。
pub trait ArchiveReader: Send {
    /// 本驱动识别出的格式。
    fn format(&self) -> Format;

    /// 全部条目（tar 系在索引未完成前是部分列表）。
    fn entries(&self) -> &[Entry];

    /// 当前索引状态。
    fn index_state(&self) -> IndexState {
        IndexState::Complete
    }

    /// 是否需要密码，以及需要到什么程度。
    fn needs_password(&self) -> PasswordNeed {
        PasswordNeed::None
    }

    /// 设置密码。头加密的格式会在这里重新读头并填充 `entries`。
    fn set_password(&mut self, _password: Option<&str>) -> Result<()> {
        Ok(())
    }

    /// 解压。`ids` 为 None 表示全部；驱动负责 solid 组内的顺序处理与跳过。
    fn extract(&mut self, ids: Option<&[EntryId]>, sink: &mut dyn EntrySink, ctx: &OpContext<'_>) -> Result<ExtractReport>;

    /// 把单个条目内容写到 `out`（预览、校验用）。
    fn read_entry(&mut self, id: EntryId, out: &mut dyn std::io::Write, ctx: &OpContext<'_>) -> Result<()>;

    /// 压缩包注释（原始字节），没有则 None。
    fn comment(&self) -> Option<&[u8]> {
        None
    }

    /// 给编码识别的先验：zip 驱动在中央目录 version made by 多为 MS-DOS 时置 made_by_dos。
    fn hints(&self) -> crate::encoding::Hints {
        crate::encoding::Hints::default()
    }
}
