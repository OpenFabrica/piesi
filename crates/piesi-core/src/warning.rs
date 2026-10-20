//! 结构化警告：核心只给「类型 + 主体 + 细节」，不给成句文案。
//! 为什么：App 要做四种语言，文案必须在壳层按 `WarningKind` 翻译；CLI 用这里的 `Display`（中文）直接打印。
//! 错误（`Error`）走另一条路：常见的几种有独立变体，壳层按变体翻译标题；损坏 / 不支持 / 不安全路径的细节见 `detail.rs`。

use std::fmt;

use crate::create::apple_double::MAX_FORK_BYTES;

/// 警告类型。每个变体就是一条用户可见的提示；会变的部分放在 `Warning::subject` / `Warning::detail`。
/// 加变体时要同步：`Display`、FFI 的 `WarningKind`、Swift 的文案表。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum WarningKind {
    /// 条目路径不安全（绝对路径、`..`、盘符等），解压计划跳过了它。subject = 条目名，detail = 原因
    UnsafePathSkipped,
    /// 同目录下两个名字只差大小写，后一个改了名。subject = 原名，detail = 新名
    CaseConflictRenamed,
    /// 硬链接暂不支持，已跳过。subject = 条目名
    HardLinkSkipped,
    /// 设备文件、管道等特殊条目，已跳过。subject = 条目名
    SpecialEntrySkipped,
    /// RAR 引用记录（`rar -oi`）暂不支持，已跳过。subject = 条目名
    RarReferenceSkipped,
    /// 条目没有数据，已跳过。subject = 条目名
    NoDataSkipped,
    /// 符号链接指向目标目录之外，已跳过。subject = 条目名，detail = 链接目标
    SymlinkEscapeSkipped,
    /// unrar 拒绝还原符号链接（目标不安全或读取失败），已跳过。subject = 条目名
    RarSymlinkRejected,
    /// 创建符号链接失败。subject = 条目名，detail = 系统错误
    SymlinkCreateFailed,
    /// RAR 跳过未选中条目时解码出错，已忽略。subject = 条目名，detail = unrar 错误码
    RarSkipDecodeError,
    /// 压缩时跳过了特殊文件（socket、设备等）。subject = 源路径
    SpecialFileSkipped,
    /// 「发给 Windows」改了名字。subject = 原名，detail = 新名
    RenamedForWindows,
    /// zip 不能加密文件名，只加密了内容。没有 subject / detail
    ZipCannotEncryptNames,
    /// 读取 Mac 元数据失败，这个文件没带元数据。subject = 源路径，detail = 系统错误
    MacMetadataReadFailed,
    /// 资源分支超过单 fork 上限，已丢弃。subject = 源路径
    MacResourceForkTooLarge,
    /// 扩展属性放不进 AppleDouble（单值超上限、条目表超 64 KB 或值区偏移超 u32），已丢弃。subject = 源路径，detail = 属性名
    MacXattrTooLarge,
    /// 扩展属性名超过 254 字节，已丢弃。subject = 源路径，detail = 属性名
    MacXattrNameTooLong,
    /// Finder 信息长度不是 32 字节，已丢弃。subject = 源路径
    MacFinderInfoInvalid,
}

/// 一条警告：类型 + 主体（哪个条目或文件）+ 细节（链接目标、新名、系统错误等，可空）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Warning {
    pub kind: WarningKind,
    pub subject: String,
    pub detail: String,
}

impl Warning {
    /// 只有主体、没有细节的警告。
    pub fn new(kind: WarningKind, subject: impl Into<String>) -> Self {
        Warning { kind, subject: subject.into(), detail: String::new() }
    }

    /// 带细节的警告。
    pub fn with_detail(kind: WarningKind, subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Warning { kind, subject: subject.into(), detail: detail.into() }
    }

    /// 既没有主体也没有细节的警告（整包级别的提示）。
    pub fn bare(kind: WarningKind) -> Self {
        Warning { kind, subject: String::new(), detail: String::new() }
    }
}

impl fmt::Display for Warning {
    /// 中文成句，CLI 直接打印；App 不走这里，按 kind 本地化。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Warning { subject: s, detail: d, .. } = self;
        match self.kind {
            WarningKind::UnsafePathSkipped => write!(f, "跳过条目 {s}：{d}"),
            WarningKind::CaseConflictRenamed => write!(f, "大小写冲突：{s} 改名为 {d}"),
            WarningKind::HardLinkSkipped => write!(f, "{s}：硬链接暂不支持，已跳过"),
            WarningKind::SpecialEntrySkipped => write!(f, "{s}：设备文件或管道，已跳过"),
            WarningKind::RarReferenceSkipped => write!(f, "{s}：引用记录（rar -oi）暂不支持，已跳过"),
            WarningKind::NoDataSkipped => write!(f, "{s}：条目没有数据，已跳过"),
            WarningKind::SymlinkEscapeSkipped => write!(f, "跳过越出目标目录的符号链接：{s} -> {d}"),
            WarningKind::RarSymlinkRejected => write!(f, "{s}：unrar 无法还原该符号链接（平台不支持、目标不安全或读取失败），已跳过"),
            WarningKind::SymlinkCreateFailed => write!(f, "创建符号链接失败：{s}：{d}"),
            WarningKind::RarSkipDecodeError => write!(f, "{s}：跳过时解码出错（{d}），未选中，忽略"),
            WarningKind::SpecialFileSkipped => write!(f, "跳过特殊文件：{s}"),
            WarningKind::RenamedForWindows => write!(f, "已改名以兼容 Windows：{s} → {d}"),
            WarningKind::ZipCannotEncryptNames => write!(f, "zip 不能加密文件名（只有 7z 可以），文件名未加密"),
            WarningKind::MacMetadataReadFailed => write!(f, "读取 Mac 元数据失败，已跳过：{s}：{d}"),
            WarningKind::MacResourceForkTooLarge => write!(f, "跳过 {s}：资源分支超过 {} MB 上限", MAX_FORK_BYTES >> 20),
            WarningKind::MacXattrTooLarge => write!(f, "跳过 {s}：扩展属性 {d} 放不进 AppleDouble（单值 {} MB、条目表 64 KB 上限）", MAX_FORK_BYTES >> 20),
            WarningKind::MacXattrNameTooLong => write!(f, "跳过 {s}：扩展属性名 {d} 太长"),
            WarningKind::MacFinderInfoInvalid => write!(f, "跳过 {s}：Finder 信息不是 32 字节"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display 要把主体和细节都带上；bare 的变体不依赖它们。
    #[test]
    fn display_includes_subject_and_detail() {
        let w = Warning::with_detail(WarningKind::SymlinkEscapeSkipped, "a/link", "../../etc");
        assert_eq!(w.to_string(), "跳过越出目标目录的符号链接：a/link -> ../../etc");
        assert_eq!(Warning::bare(WarningKind::ZipCannotEncryptNames).to_string(), "zip 不能加密文件名（只有 7z 可以），文件名未加密");
        let big = Warning::new(WarningKind::MacResourceForkTooLarge, "/x/huge.bin");
        assert!(big.to_string().contains("64 MB"), "{big}");
    }

    /// serde 往返（CLI `--json` 以后可能直接输出它）。
    #[test]
    fn serde_roundtrip() {
        let w = Warning::with_detail(WarningKind::RenamedForWindows, "a:b", "a：b");
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<Warning>(&json).unwrap(), w);
    }
}
