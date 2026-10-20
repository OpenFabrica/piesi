//! piesi-core：Piesi 的压缩/解压引擎。
//! 分层：`fmt`（格式驱动）→ `archive`（统一模型）→ `encoding`/`names`（文件名）→ `plan`/`fs`（安全落盘）。

pub mod archive;
pub mod codec;
pub mod create;
pub mod encoding;
pub mod error;
pub mod facade;
pub mod fmt;
pub mod fs;
pub mod names;
pub mod plan;
pub mod progress;
pub mod sys;
pub mod volumes;
pub mod warning;

pub use archive::{ArchiveReader, Entry, EntryId, EntryKind, Format, IndexState, Method, PasswordNeed};
pub use error::{Error, Result};
pub use progress::{CancelToken, OpContext, Progress, ProgressEvent};
pub use warning::{Warning, WarningKind};

use std::path::Path;

/// 按魔数识别格式（扩展名只做兜底），返回 None 表示不认识。
pub fn detect_format(path: &Path) -> Result<Option<Format>> {
    fmt::detect::detect_format(path)
}

/// 打开压缩包，返回对应格式的驱动。不读内容，只读目录区（tar 系除外）。
pub fn open(path: &Path) -> Result<Box<dyn ArchiveReader>> {
    let format = detect_format(path)?.ok_or(Error::UnknownFormat)?;
    fmt::open_with_format(path, format)
}
