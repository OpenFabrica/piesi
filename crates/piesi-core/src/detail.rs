//! 结构化错误细节：`Error::Corrupt` / `Unsupported` / `UnsafePath` 带的不是成句文案，而是「类型 + 参数」。
//! 为什么：App 要做四种语言，细节句子必须在壳层按 `DetailKind` 翻译；CLI 用这里的 `Display`（中文）直接打印。
//! 照搬 `warning.rs` 的做法；第三方库给的英文文案翻不了，归到 `Library`，原样显示。

use std::fmt;

/// 错误细节类型。每个变体对应一句细节；会变的部分（偏移、方法号、条目名、路径）放在 `ErrorDetail::args`。
/// 加变体时要同步：`Display`、FFI 的 `DetailKind`、Swift 的文案表（`CoreErrorText.swift`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DetailKind {
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

/// 一条错误细节：类型 + 参数（按 `DetailKind` 各变体说明的顺序，可空）。
/// 为什么用 `Vec` 而不是固定的一两个字段：CRC 不匹配要三个参数（条目名、期望值、实际值），其余多数零到一个。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorDetail {
    pub kind: DetailKind,
    pub args: Vec<String>,
}

impl ErrorDetail {
    /// 带一个参数的细节。
    pub fn with(kind: DetailKind, arg: impl fmt::Display) -> Self {
        ErrorDetail { kind, args: vec![arg.to_string()] }
    }

    /// 带多个参数的细节。
    pub fn with_args<const N: usize>(kind: DetailKind, args: [String; N]) -> Self {
        ErrorDetail { kind, args: args.into() }
    }

    /// 第三方库文案的兜底细节。
    pub fn library(e: impl fmt::Display) -> Self {
        Self::with(DetailKind::Library, e)
    }

    /// 第 `i` 个参数，缺了给空串（防御：调用方漏传时 Display 不 panic）。
    fn arg(&self, i: usize) -> &str {
        self.args.get(i).map_or("", String::as_str)
    }
}

impl From<DetailKind> for ErrorDetail {
    /// 没有参数的细节。
    fn from(kind: DetailKind) -> Self {
        ErrorDetail { kind, args: Vec::new() }
    }
}

impl fmt::Display for ErrorDetail {
    /// 中文成句，CLI 直接打印；App 不走这里，按 kind 本地化。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use DetailKind as K;
        let (a, b, c) = (self.arg(0), self.arg(1), self.arg(2));
        match self.kind {
            K::Library | K::Plain => f.write_str(a),
            K::PathControlChars => write!(f, "含控制字符：{a}"),
            K::PathAbsolute => write!(f, "绝对路径：{a}"),
            K::PathDrivePrefix => write!(f, "Windows 盘符：{a}"),
            K::PathDotComponent => write!(f, "路径含 `{a}` 分量：{b}"),
            K::PathEmpty => write!(f, "空路径：{a}"),
            K::EntryIndexOutOfRange => f.write_str("条目序号越界"),
            K::TooManyEntries => f.write_str("条目数超过上限"),
            K::SymlinkTargetTooLong => write!(f, "符号链接目标过长：{a}"),
            K::IndexMismatch => f.write_str("压缩包内容与打开时建立的索引不一致"),
            K::CompressionMethod => write!(f, "压缩方法 {a}"),
            K::DecompressFailed => write!(f, "解压失败：{a}"),
            K::RarArchiveHeaderBad => f.write_str("RAR 压缩包头损坏"),
            K::RarFileHeaderBad => f.write_str("RAR 文件头损坏"),
            K::RarCrcFailed => f.write_str("RAR 数据 CRC 校验失败"),
            K::NotRar => f.write_str("不是 RAR 压缩包"),
            K::NextVolumeMissing => f.write_str("找不到下一个分卷"),
            K::RarReadError => f.write_str("RAR 读取错误"),
            K::RarUnknownFormat => f.write_str("RAR：未知的格式或加密方式"),
            K::RarReference => f.write_str("RAR 引用记录（-oi）需要完整解压"),
            K::SevenZMissingBlock => f.write_str("7z：条目引用了不存在的块"),
            K::SevenZBlockMismatch => f.write_str("7z：块与文件表对不上"),
            K::SevenZEntryMismatch => write!(f, "块内条目与目录不一致：{a}"),
            K::SevenZMethod => write!(f, "7z 压缩方法 {a}"),
            K::SevenZLibrary => write!(f, "7z：{a}"),
            K::TarDriverMismatch => write!(f, "tar 驱动不处理 {a}"),
            K::TarEntryReadFailed => write!(f, "tar 第 {a} 个条目读取失败：{b}"),
            K::NotTarFormat => write!(f, "{a} 不是 tar 格式"),
            K::ZipTooShort => f.write_str("文件太短，不是 zip"),
            K::ZipTailReadFailed => f.write_str("读取文件尾部失败"),
            K::EocdNotFound => f.write_str("找不到中央目录结束记录（EOCD）"),
            K::Zip64LocatorMissing => f.write_str("字段溢出但找不到 zip64 定位记录"),
            K::Zip64EocdBad => f.write_str("zip64 中央目录结束记录缺失或损坏"),
            K::CdHeaderIncomplete => f.write_str("中央目录头不完整"),
            K::CdNameIncomplete => f.write_str("中央目录条目名不完整"),
            K::CdExtraIncomplete => f.write_str("中央目录扩展字段不完整"),
            K::CdCommentIncomplete => f.write_str("中央目录条目注释不完整"),
            K::Zip64MissingSize => f.write_str("zip64 扩展字段缺少原始大小"),
            K::Zip64MissingPacked => f.write_str("zip64 扩展字段缺少压缩后大小"),
            K::Zip64MissingOffset => f.write_str("zip64 扩展字段缺少本地头偏移"),
            K::Zip64MissingDisk => f.write_str("zip64 扩展字段缺少盘号"),
            K::CdOutOfRange => f.write_str("中央目录越界"),
            K::CdMisplaced => f.write_str("中央目录位置不对，读不到任何条目"),
            K::OffsetOverflow => f.write_str("偏移溢出"),
            K::DiskMissing => write!(f, "引用了不存在的分卷 #{a}"),
            K::RecordTruncated => f.write_str("记录不完整"),
            K::LocalHeaderBadSignature => f.write_str("本地文件头签名不对"),
            K::LocalHeaderOffsetOutOfRange => write!(f, "本地文件头偏移 {a} 越界"),
            K::LocalHeaderIncomplete => f.write_str("本地文件头不完整"),
            K::LocalHeaderOutOfRange => f.write_str("本地文件头越界"),
            K::EntryDataOutOfRange => write!(f, "条目数据越界：{a}"),
            K::DataTruncated => write!(f, "数据提前结束：{a}"),
            K::CrcMismatch => write!(f, "CRC 不匹配：{a}（期望 {b}，实际 {c}）"),
            K::RepairNoEntries => f.write_str("扫描不到任何完整条目"),
            K::PkwareStrongEncryption => f.write_str("PKWARE 强加密"),
            K::AesExtraMissing => f.write_str("AES 条目缺少 0x9901 扩展字段"),
            K::AesKeyLength => f.write_str("AES 密钥长度错误"),
            K::AesStrength => write!(f, "AES 强度 {a}"),
            K::HmacKeyLength => f.write_str("HMAC 密钥长度错误"),
            K::AesEntryTooShort => f.write_str("AES 条目长度不足"),
            K::AesSaltIncomplete => f.write_str("AES 盐不完整"),
            K::AesVerifierIncomplete => f.write_str("AES 口令校验值不完整"),
            K::AesCiphertextTruncated => f.write_str("AES 密文提前结束"),
            K::AesAuthCodeMissing => f.write_str("AES 认证码缺失"),
            K::AesAuthFailed => f.write_str("AES 认证失败：数据被改动或密码错误"),
            K::ZipCryptoHeaderIncomplete => f.write_str("ZipCrypto 加密头不完整"),
            K::VolumeTotalOverflow => f.write_str("分卷总长溢出"),
            K::VolumeCountInvalid => write!(f, "分卷数非法：{a}"),
            K::VolumeMissing => write!(f, "缺少分卷：{a}"),
            K::StreamHeaderIncomplete => write!(f, "{a}：流头部不完整"),
            K::LzmaPropsInvalid => f.write_str("LZMA 属性字节非法"),
            K::LzmaDictTooLarge => write!(f, "LZMA 字典过大：{a} MB"),
            K::LzmaPropsLength => write!(f, "LZMA 属性长度非法：{a}"),
            K::LzmaPropsShort => f.write_str("LZMA 属性不足 5 字节"),
            K::Lzma2PropsMissing => f.write_str("LZMA2 缺少属性字节"),
            K::Lzma2DictTooLarge => f.write_str("LZMA2 字典过大"),
            K::PpmdRestoreMode => write!(f, "PPMd 恢复模式 {a}"),
            K::PpmdParamsInvalid => write!(f, "PPMd 参数非法：{a}"),
            K::PpmdMemTooLarge => write!(f, "PPMd 内存需求过大：{a} MB"),
            K::PpmdPropsShort => f.write_str("PPMd 属性不足 5 字节"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    /// 错误的 Display 要和改成结构化之前逐字一致（CLI 输出靠它）。
    #[test]
    fn display_matches_legacy_text() {
        let crc = ErrorDetail::with_args(DetailKind::CrcMismatch, ["a.txt".into(), format!("{:08x}", 0xdeadbeef_u32), format!("{:08x}", 1)]);
        assert_eq!(Error::Corrupt(crc).to_string(), "压缩包损坏：CRC 不匹配：a.txt（期望 deadbeef，实际 00000001）");
        assert_eq!(Error::Corrupt(DetailKind::EocdNotFound.into()).to_string(), "压缩包损坏：找不到中央目录结束记录（EOCD）");
        let dot = ErrorDetail::with_args(DetailKind::PathDotComponent, ["..".into(), "../x".into()]);
        assert_eq!(Error::UnsafePath(dot).to_string(), "不安全的路径：路径含 `..` 分量：../x");
        assert_eq!(Error::Unsupported(ErrorDetail::with(DetailKind::Plain, "TAR")).to_string(), "不支持：TAR");
        assert_eq!(Error::corrupt("boom").to_string(), "压缩包损坏：boom");
    }

    /// 少传参数时不 panic，缺的位置是空串。
    #[test]
    fn missing_args_are_empty() {
        let d: ErrorDetail = DetailKind::CrcMismatch.into();
        assert_eq!(d.to_string(), "CRC 不匹配：（期望 ，实际 ）");
    }
}
