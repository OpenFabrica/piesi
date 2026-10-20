//! 引擎统一错误类型；UI 层按变体决定提示文案（要密码、不安全路径、空间不足等）。
//! 损坏 / 不支持 / 不安全路径的细节是结构化的 `ErrorDetail`（见 `detail.rs`），壳层按类型翻译。

use crate::detail::ErrorDetail;

/// 引擎所有公开 API 的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("无法识别的压缩格式")]
    UnknownFormat,
    #[error("不支持：{0}")]
    Unsupported(ErrorDetail),
    #[error("压缩包损坏：{0}")]
    Corrupt(ErrorDetail),
    #[error("需要密码")]
    PasswordRequired,
    #[error("密码错误")]
    WrongPassword,
    #[error("已取消")]
    Cancelled,
    #[error("不安全的路径：{0}")]
    UnsafePath(ErrorDetail),
    #[error("磁盘空间不足：需要 {needed} 字节，可用 {available} 字节")]
    NoSpace { needed: u64, available: u64 },
    #[error("条目不存在：{0}")]
    NoSuchEntry(u32),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// 引擎统一 Result 别名。
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// 把任意可显示的错误包成 Other，用于第三方库错误转换。
    pub fn other(e: impl std::fmt::Display) -> Self {
        Error::Other(e.to_string())
    }

    /// 把第三方库错误归为「压缩包损坏」，文案原样放进兜底细节 `Library`。
    pub fn corrupt(e: impl std::fmt::Display) -> Self {
        Error::Corrupt(ErrorDetail::library(e))
    }
}
