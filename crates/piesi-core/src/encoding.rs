//! 文件名编码识别与解码。待实现：候选解码 + 字频打分 + locale 先验。

use crate::archive::Entry;

/// 支持的文件名编码。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum NameEncoding {
    Utf8,
    Gb18030,
    Big5,
    ShiftJis,
    EucKr,
    Cp1251,
    Cp866,
    Cp1252,
}

/// 判断结论从哪来，UI 据此显示「自动」「手动」等标签。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DetectionSource {
    /// 格式本身是 Unicode（7z/rar）或全部条目带 UTF-8 标志
    Format,
    /// 全部名字是合法 UTF-8
    ValidUtf8,
    /// 候选解码打分得出
    Scored,
    /// 用户手动指定
    Manual,
}

/// 识别结果。`ambiguous` 为 true 时 UI 在解压前先弹编码选择条。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Detection {
    pub encoding: NameEncoding,
    pub confidence: f32,
    pub source: DetectionSource,
    pub ambiguous: bool,
}

/// 识别用的先验信息。
#[derive(Clone, Debug, Default)]
pub struct Hints {
    /// zip 的 version made by 是 MS-DOS（0）：多半是 Windows 工具产物
    pub made_by_dos: bool,
    /// 系统地区，如 "zh-CN"、"ja-JP"
    pub locale: Option<String>,
}

/// 对一组条目做编码识别。
pub fn detect(_entries: &[Entry], _hints: &Hints) -> Detection {
    Detection { encoding: NameEncoding::Utf8, confidence: 0.0, source: DetectionSource::ValidUtf8, ambiguous: false }
}

/// 按给定编码解码单个条目名；优先级：unicode_name > name_is_utf8 > 指定编码。
pub fn decode_name(entry: &Entry, _enc: NameEncoding) -> String {
    if let Some(u) = &entry.unicode_name {
        return u.clone();
    }
    String::from_utf8_lossy(&entry.raw_name).into_owned()
}

/// 写入前归一化：NFC。
pub fn normalize_for_write(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().collect()
}
