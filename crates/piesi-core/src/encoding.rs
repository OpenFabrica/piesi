//! 文件名编码识别与解码。
//!
//! 读取顺序（设计文档 5.2）：
//! 1. 条目带 `unicode_name`（zip 0x7075 字段，CRC 已校验）→ 直接用；
//! 2. `name_is_utf8`（zip bit 11、7z、rar、tar pax）→ UTF-8。标志按**整包**判断可信度：
//!    只要有一个带标志条目的字节不是合法 UTF-8，整包标志都不信（见 `utf8_flags_reliable`）；
//! 3. 剩余条目的原始字节全部是合法 UTF-8 → UTF-8（Finder/ditto 产物）。例外：全是 2 字节序列
//!    且文字系统乱混的「撞车」字节（GBK「图片」= CD BC C6 AC）交给打分并标 ambiguous（见 `utf8_looks_incoherent`）；
//! 4. 否则把所有含非 ASCII 字节的名字拼起来，对候选编码逐个解码打分（见 `rank_candidates`）。
//!
//! 打分 = 「落在该编码常用字集里的字符比例」减去控制字符/私用区/符号的惩罚，
//! 再加上特异性、locale 先验、DOS 先验和 chardetng 的一票；第一名领先不到 0.15 或不足 0.6 → ambiguous。
//! 常用字集不手写表，由 encoding_rs 解码字节区间现算并缓存（`OnceLock`）。
//! 已知极限：谚文区 ⊂ GB2312 一级字区，韩文字节当 GBK 解永远是「完美」的一级汉字，2 字名只能靠先验与 ambiguous 兜底。

use std::collections::HashSet;
use std::sync::OnceLock;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

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

impl NameEncoding {
    /// 全部编码，按 UI 选择器的显示顺序。
    pub const ALL: [NameEncoding; 8] = [
        NameEncoding::Utf8,
        NameEncoding::Gb18030,
        NameEncoding::Big5,
        NameEncoding::ShiftJis,
        NameEncoding::EucKr,
        NameEncoding::Cp1251,
        NameEncoding::Cp866,
        NameEncoding::Cp1252,
    ];

    /// 全部编码的切片形式。
    pub fn all() -> &'static [NameEncoding] {
        &Self::ALL
    }

    /// 给 UI 显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            NameEncoding::Utf8 => "UTF-8",
            NameEncoding::Gb18030 => "GBK / GB18030",
            NameEncoding::Big5 => "Big5",
            NameEncoding::ShiftJis => "Shift_JIS",
            NameEncoding::EucKr => "EUC-KR",
            NameEncoding::Cp1251 => "Windows-1251",
            NameEncoding::Cp866 => "CP866",
            NameEncoding::Cp1252 => "Windows-1252",
        }
    }

    /// 稳定的机器可读 id，用于 CLI 参数、语料 manifest 和按包记忆。
    pub fn id(self) -> &'static str {
        match self {
            NameEncoding::Utf8 => "utf8",
            NameEncoding::Gb18030 => "gb18030",
            NameEncoding::Big5 => "big5",
            NameEncoding::ShiftJis => "shift_jis",
            NameEncoding::EucKr => "euc_kr",
            NameEncoding::Cp1251 => "cp1251",
            NameEncoding::Cp866 => "cp866",
            NameEncoding::Cp1252 => "cp1252",
        }
    }

    /// 从 id 或常见别名解析（大小写、`-`/`_` 不敏感），认不出返回 None。
    pub fn from_id(id: &str) -> Option<NameEncoding> {
        let key: String = id.trim().to_ascii_lowercase().chars().filter(|c| *c != '-' && *c != '_').collect();
        Some(match key.as_str() {
            "utf8" => NameEncoding::Utf8,
            "gb18030" | "gbk" | "gb2312" | "cp936" | "windows936" => NameEncoding::Gb18030,
            "big5" | "cp950" | "windows950" | "big5hkscs" => NameEncoding::Big5,
            "shiftjis" | "sjis" | "cp932" | "windows31j" | "ms932" => NameEncoding::ShiftJis,
            "euckr" | "cp949" | "uhc" | "ksc5601" => NameEncoding::EucKr,
            "cp1251" | "windows1251" | "win1251" => NameEncoding::Cp1251,
            "cp866" | "ibm866" | "dos866" => NameEncoding::Cp866,
            "cp1252" | "windows1252" | "win1252" | "latin1" | "iso88591" => NameEncoding::Cp1252,
            _ => return None,
        })
    }

    /// 对应的 encoding_rs 编码对象。
    pub fn encoding(self) -> &'static Encoding {
        match self {
            NameEncoding::Utf8 => encoding_rs::UTF_8,
            NameEncoding::Gb18030 => encoding_rs::GB18030,
            NameEncoding::Big5 => encoding_rs::BIG5,
            NameEncoding::ShiftJis => encoding_rs::SHIFT_JIS,
            NameEncoding::EucKr => encoding_rs::EUC_KR,
            NameEncoding::Cp1251 => encoding_rs::WINDOWS_1251,
            NameEncoding::Cp866 => encoding_rs::IBM866,
            NameEncoding::Cp1252 => encoding_rs::WINDOWS_1252,
        }
    }

    /// 按本编码解码任意原始字节（symlink 目标、注释也能用）；解不出的字节用 U+FFFD。
    pub fn decode_lossy(self, raw: &[u8]) -> String {
        match self {
            NameEncoding::Utf8 => String::from_utf8_lossy(raw).into_owned(),
            _ => self.encoding().decode_without_bom_handling(raw).0.into_owned(),
        }
    }
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

impl Detection {
    /// 用户手动指定编码时的结果（置信度视为 1，不再弹选择条）。
    pub fn manual(encoding: NameEncoding) -> Self {
        Detection { encoding, confidence: 1.0, source: DetectionSource::Manual, ambiguous: false }
    }
}

/// 识别用的先验信息。
#[derive(Clone, Debug, Default)]
pub struct Hints {
    /// zip 的 version made by 是 MS-DOS（0）：多半是 Windows 工具产物
    pub made_by_dos: bool,
    /// 系统地区，如 "zh-CN"、"ja-JP"、"zh_TW.UTF-8"
    pub locale: Option<String>,
}

/// 一个候选编码的最终得分，`rank_candidates` 按分数从高到低返回。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CandidateScore {
    pub encoding: NameEncoding,
    pub score: f32,
}

// ---- 打分参数：全部集中在这里，便于按语料调整 ----

/// 第一名与第二名差距小于它 → ambiguous。
const AMBIGUOUS_GAP: f32 = 0.15;
/// 第一名分数低于它 → ambiguous。
const AMBIGUOUS_FLOOR: f32 = 0.6;
/// locale 先验加分（ru/uk 时 CP866 只拿一半，CP1251 优先）。
/// 为什么是 0.1：谚文区 ⊂ GB2312 一级字区，4 字韩文名与 15% 的 4 字中文名在字节层面互相都是「完美」的，
/// 只能靠先验定夺；0.05 压不过 chardetng 的一票，本地用户的 4 字名会一直被要求确认。
const LOCALE_PRIOR: f32 = 0.1;
/// version made by = MS-DOS 时各本地代码页的加分。
const DOS_PRIOR: f32 = 0.02;
/// chardetng 一票的满额加分。
/// 为什么要按长度打折：它是面向网页的，实测对 2 字的 GBK 名只有四成准确率（多半误判成 EUC-KR），
/// 4 字以上才有九成；所以权重按「非 ASCII 字节数 / 满额字节数」的平方衰减，4 字节时几乎不计票。
const CHARDETNG_VOTE: f32 = 0.10;
/// chardetng 加分到满额所需的非 ASCII 字节数（5 个双字节字）。
const CHARDETNG_FULL_BYTES: f32 = 10.0;
/// 特异性加分：每个落在常用字集里的字符 × 特异性因子。
const SPECIFICITY_PER_CHAR: f32 = 0.01;
/// 特异性加分上限。
const SPECIFICITY_CAP: f32 = 0.2;
/// 控制字符惩罚（C1 控制区、非字符）。
const PENALTY_CONTROL: f32 = 2.0;
/// 私用区惩罚（GBK/Big5 互解、CP949 用户区的典型产物）。
const PENALTY_PUA: f32 = 1.0;
/// 符号区惩罚（制表符、半角片假名、Latin-1 符号等乱码典型产物）。
const PENALTY_SYMBOL: f32 = 0.5;
/// 全部字符都是中性字符（全角标点等）时的保底分：说明内容无信息量，交给先验与 ambiguous 处理。
const UNINFORMATIVE_SCORE: f32 = 0.5;
/// 全部名字是合法 UTF-8 但没有格式标志时的置信度。
const VALID_UTF8_CONFIDENCE: f32 = 0.95;

/// 参与打分的候选（不含 UTF-8：能走到打分这一步，说明已有名字不是合法 UTF-8，UTF-8 必然被淘汰）。
const CANDIDATES: [NameEncoding; 7] = [
    NameEncoding::Gb18030,
    NameEncoding::Big5,
    NameEncoding::ShiftJis,
    NameEncoding::EucKr,
    NameEncoding::Cp1251,
    NameEncoding::Cp866,
    NameEncoding::Cp1252,
];

/// 候选编码的特征：常用字集、特异性因子、非 ASCII 密度上限。
struct Profile {
    encoding: NameEncoding,
    /// 常用字集（由 encoding_rs 解码字节区间现算）。
    common: HashSet<char>,
    /// 0..=1：常用字集越小，「所有字都落在集合里」越能说明问题。
    specificity: f32,
    /// 非 ASCII 字符占字母数字的比例上限；超过就按比例压分。
    /// 为什么：西欧文本的带变音字母只是少数，一串全是 Ä ã º Ã 的名字几乎必然是 CJK 字节被误读。
    max_density: f32,
    /// 西里尔字母紧贴 ASCII 字母是否要罚分。
    /// 为什么：真实文本一个词只用一种文字，「rйsumй」这种拉丁字母里夹西里尔字母只会来自误读。
    penalize_latin_adjacent: bool,
    /// 中性字符（全角标点、带圈数字等）是否算本编码的常用字。
    /// 为什么：这些字符本来就是 CJK 代码页的东西，解出它们是支持该候选的证据；
    /// 单字节编码只会解出 – — “ ” 这类通用标点，不给分也不扣分。
    neutral_is_common: bool,
}

/// 所有候选的特征表，首次使用时构建并缓存。
fn profiles() -> &'static [Profile] {
    static PROFILES: OnceLock<Vec<Profile>> = OnceLock::new();
    PROFILES.get_or_init(build_profiles)
}

/// 构建特征表：先算各常用字集，再按集合大小推特异性因子。
fn build_profiles() -> Vec<Profile> {
    let sets: Vec<(NameEncoding, HashSet<char>)> = CANDIDATES.iter().map(|&e| (e, common_set(e))).collect();
    // 特异性因子以 CJK 集合的最大/最小规模为两端（Big5 常用字 ≈ 5400 → 0，KS X 1001 谚文 2350 → 1）。
    // 为什么单字节编码不拿这个分：它们永远解码成功，高位字节有一半都是字母，「全落在字母区」不说明什么。
    let cjk_sizes: Vec<f32> = sets
        .iter()
        .filter(|(e, _)| matches!(e, NameEncoding::Gb18030 | NameEncoding::Big5 | NameEncoding::ShiftJis | NameEncoding::EucKr))
        .map(|(_, s)| s.len() as f32)
        .collect();
    let ln_max = cjk_sizes.iter().cloned().fold(f32::MIN, f32::max).ln();
    let ln_min = cjk_sizes.iter().cloned().fold(f32::MAX, f32::min).ln();
    sets.into_iter()
        .map(|(encoding, common)| {
            let single_byte = matches!(encoding, NameEncoding::Cp1251 | NameEncoding::Cp866 | NameEncoding::Cp1252);
            let specificity = if single_byte || ln_max <= ln_min {
                0.0
            } else {
                ((ln_max - (common.len() as f32).ln()) / (ln_max - ln_min)).clamp(0.0, 1.0)
            };
            let (max_density, penalize_latin_adjacent) = match encoding {
                NameEncoding::Cp1252 => (0.4, false),
                NameEncoding::Cp1251 | NameEncoding::Cp866 => (1.0, true),
                _ => (1.0, false),
            };
            Profile { encoding, common, specificity, max_density, penalize_latin_adjacent, neutral_is_common: !single_byte }
        })
        .collect()
}

/// 某候选的常用字集：
/// - GB18030：GB2312 一级字 = 0xB0A1..=0xD7F9（按拼音序的 3755 字）
/// - Big5：常用字区 0xA440..=0xC67E
/// - Shift_JIS：JIS 第一水準 0x889F..=0x9872 + 平假名 0x829F..=0x82F1 + 片假名 0x8340..=0x8396 + ー 々
/// - EUC-KR：KS X 1001 谚文 0xB0A1..=0xC8FE（2350 字）
/// - CP1251/CP866：西里尔字母 U+0410..=U+044F 及 Ё/ё
/// - CP1252：Latin-1 带变音字母 U+00C0..=U+00FF（去掉 × ÷）及 Œœ Šš Žž Ÿ
fn common_set(enc: NameEncoding) -> HashSet<char> {
    let mut set = HashSet::new();
    match enc {
        NameEncoding::Gb18030 => {
            decode_pairs(encoding_rs::GB18030, 0xB0A1, 0xD7F9, |t| (0xA1..=0xFE).contains(&t), &mut set);
        }
        NameEncoding::Big5 => {
            decode_pairs(encoding_rs::BIG5, 0xA440, 0xC67E, |t| (0x40..=0x7E).contains(&t) || (0xA1..=0xFE).contains(&t), &mut set);
        }
        NameEncoding::ShiftJis => {
            let trail_ok = |t: u8| (0x40..=0x7E).contains(&t) || (0x80..=0xFC).contains(&t);
            decode_pairs(encoding_rs::SHIFT_JIS, 0x889F, 0x9872, trail_ok, &mut set);
            decode_pairs(encoding_rs::SHIFT_JIS, 0x829F, 0x82F1, trail_ok, &mut set);
            decode_pairs(encoding_rs::SHIFT_JIS, 0x8340, 0x8396, trail_ok, &mut set);
            // 长音符与叠字符在片假名词里极常见，单独补上
            decode_pairs(encoding_rs::SHIFT_JIS, 0x815B, 0x815B, trail_ok, &mut set);
            decode_pairs(encoding_rs::SHIFT_JIS, 0x8158, 0x8158, trail_ok, &mut set);
        }
        NameEncoding::EucKr => {
            decode_pairs(encoding_rs::EUC_KR, 0xB0A1, 0xC8FE, |t| (0xA1..=0xFE).contains(&t), &mut set);
        }
        NameEncoding::Cp1251 | NameEncoding::Cp866 => {
            set.extend(('\u{0410}'..='\u{044F}').chain(['\u{0401}', '\u{0451}']));
        }
        NameEncoding::Cp1252 => {
            set.extend(('\u{00C0}'..='\u{00FF}').filter(|c| *c != '\u{00D7}' && *c != '\u{00F7}'));
            set.extend(['\u{0152}', '\u{0153}', '\u{0160}', '\u{0161}', '\u{017D}', '\u{017E}', '\u{0178}']);
        }
        NameEncoding::Utf8 => {}
    }
    set
}

/// 把 `lo..=hi` 的双字节码逐个用 encoding_rs 解码，只收「恰好解出一个非替换字符」的结果。
fn decode_pairs(enc: &'static Encoding, lo: u16, hi: u16, trail_ok: impl Fn(u8) -> bool, out: &mut HashSet<char>) {
    for code in lo..=hi {
        let pair = code.to_be_bytes();
        if !trail_ok(pair[1]) {
            continue;
        }
        if let Some(s) = enc.decode_without_bom_handling_and_without_replacement(&pair) {
            let mut chars = s.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                out.insert(c);
            }
        }
    }
}

/// 非 ASCII 字符的分类，决定它在打分里算分母、算惩罚还是忽略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharClass {
    /// CJK 标点、全角字母数字、带圈数字、常见破折号引号等：CJK 候选算常用字，单字节候选不计入分母
    Neutral,
    /// C1 控制区、非字符：正常文件名不可能有
    Control,
    /// 私用区
    Pua,
    /// 符号区：制表符、方块、半角片假名、Latin-1 符号等
    Symbol,
    /// 其他可打印字符：是否「常用」由各编码的常用字集决定
    Other,
}

/// 给非 ASCII 字符分类。分支顺序有意义：具体码位在前，大区间在后。
fn classify(c: char) -> CharClass {
    match c as u32 {
        0x80..=0x9F | 0xFFF9..=0xFFFF => CharClass::Control,
        0xA0 | 0xB7 => CharClass::Neutral,
        0xA1..=0xBF | 0xD7 | 0xF7 => CharClass::Symbol,
        0x2013 | 0x2014 | 0x2018 | 0x2019 | 0x201C | 0x201D | 0x2026 | 0x2116 => CharClass::Neutral,
        0x2010..=0x2027 | 0x2030..=0x205E | 0x20A0..=0x20CF => CharClass::Symbol,
        0x2460..=0x24FF => CharClass::Neutral,
        0x2100..=0x2BFF => CharClass::Symbol,
        0x3000..=0x303F => CharClass::Neutral,
        0xE000..=0xF8FF | 0xF0000..=0x10FFFD => CharClass::Pua,
        0xFF01..=0xFF60 | 0xFFE0..=0xFFE6 => CharClass::Neutral,
        0xFF61..=0xFF9F => CharClass::Symbol,
        _ => CharClass::Other,
    }
}

/// 单个候选对拼接文本的基础分与常用字数：`(base, common_count)`；解码失败返回 None（淘汰）。
fn score_profile(profile: &Profile, joined: &[u8], ascii_alnum: usize) -> Option<(f32, usize)> {
    let text = profile.encoding.encoding().decode_without_bom_handling_and_without_replacement(joined)?;
    let chars: Vec<char> = text.chars().collect();
    let mut total = 0usize;
    let mut common = 0usize;
    let mut penalty = 0f32;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii() {
            continue;
        }
        match classify(c) {
            CharClass::Neutral if profile.neutral_is_common => common += 1,
            CharClass::Neutral => continue,
            CharClass::Control => penalty += PENALTY_CONTROL,
            CharClass::Pua => penalty += PENALTY_PUA,
            CharClass::Symbol => penalty += PENALTY_SYMBOL,
            CharClass::Other => {
                if profile.common.contains(&c) {
                    let prev_latin = i > 0 && chars[i - 1].is_ascii_alphabetic();
                    let next_latin = chars.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic());
                    if profile.penalize_latin_adjacent && (prev_latin || next_latin) {
                        penalty += 1.0;
                    } else {
                        common += 1;
                    }
                }
            }
        }
        total += 1;
    }
    if total == 0 {
        return Some((UNINFORMATIVE_SCORE, 0));
    }
    let mut base = ((common as f32 - penalty) / total as f32).clamp(0.0, 1.0);
    let density = total as f32 / (total + ascii_alnum) as f32;
    if density > profile.max_density {
        base *= profile.max_density / density;
    }
    Some((base, common))
}

/// locale 先验：返回 (编码, 加分) 列表。只认语言子标签，zh 再看地区/文字。
fn locale_priors(locale: &str) -> Vec<(NameEncoding, f32)> {
    let lower = locale.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split(['-', '_', '.', '@']).filter(|p| !p.is_empty()).collect();
    let Some(&lang) = parts.first() else { return Vec::new() };
    match lang {
        "zh" => {
            let traditional = parts.iter().skip(1).any(|p| matches!(*p, "tw" | "hk" | "mo" | "hant"));
            vec![(if traditional { NameEncoding::Big5 } else { NameEncoding::Gb18030 }, LOCALE_PRIOR)]
        }
        "ja" => vec![(NameEncoding::ShiftJis, LOCALE_PRIOR)],
        "ko" => vec![(NameEncoding::EucKr, LOCALE_PRIOR)],
        "ru" | "uk" | "be" | "bg" | "mk" | "sr" | "kk" | "ky" | "tg" | "mn" => {
            vec![(NameEncoding::Cp1251, LOCALE_PRIOR), (NameEncoding::Cp866, LOCALE_PRIOR / 2.0)]
        }
        "de" | "fr" | "es" | "pt" | "it" | "nl" | "sv" | "da" | "nb" | "nn" | "no" | "fi" | "is" | "ca" | "gl" | "eu" | "af" => {
            vec![(NameEncoding::Cp1252, LOCALE_PRIOR)]
        }
        _ => Vec::new(),
    }
}

/// chardetng 的结论映射到我们的候选；它认出的其他编码（KOI8、EUC-JP…）不投票。
fn chardetng_vote(joined: &[u8]) -> Option<NameEncoding> {
    let mut det = EncodingDetector::new(Iso2022JpDetection::Deny);
    det.feed(joined, true);
    let guess = det.guess(None, Utf8Detection::Deny);
    if guess == encoding_rs::GBK || guess == encoding_rs::GB18030 {
        Some(NameEncoding::Gb18030)
    } else if guess == encoding_rs::BIG5 {
        Some(NameEncoding::Big5)
    } else if guess == encoding_rs::SHIFT_JIS {
        Some(NameEncoding::ShiftJis)
    } else if guess == encoding_rs::EUC_KR {
        Some(NameEncoding::EucKr)
    } else if guess == encoding_rs::WINDOWS_1251 {
        Some(NameEncoding::Cp1251)
    } else if guess == encoding_rs::IBM866 {
        Some(NameEncoding::Cp866)
    } else if guess == encoding_rs::WINDOWS_1252 {
        Some(NameEncoding::Cp1252)
    } else {
        None
    }
}

/// 对一组原始名字字节打分，按分数从高到低返回全部存活候选（解码失败的候选不在列表里）。
/// 只有含非 ASCII 字节的名字参与；全部是 ASCII 时返回空列表。
pub fn rank_candidates(raw_names: &[&[u8]], hints: &Hints) -> Vec<CandidateScore> {
    // 用 '\n' 分隔拼接：所有候选都兼容 ASCII，分隔符能防止前一个名字的尾字节与下一个名字的首字节粘成一个多字节码。
    let mut joined: Vec<u8> = Vec::new();
    for name in raw_names.iter().filter(|n| !n.is_ascii()) {
        if !joined.is_empty() {
            joined.push(b'\n');
        }
        joined.extend_from_slice(name);
    }
    if joined.is_empty() {
        return Vec::new();
    }
    let ascii_alnum = joined.iter().filter(|b| b.is_ascii_alphanumeric()).count();
    let non_ascii_bytes = joined.iter().filter(|b| !b.is_ascii()).count();

    let priors = hints.locale.as_deref().map(locale_priors).unwrap_or_default();
    let vote = chardetng_vote(&joined);
    let vote_weight = CHARDETNG_VOTE * (non_ascii_bytes as f32 / CHARDETNG_FULL_BYTES).min(1.0).powi(2);

    let mut scores: Vec<CandidateScore> = profiles()
        .iter()
        .filter_map(|p| {
            let (base, common) = score_profile(p, &joined, ascii_alnum)?;
            let mut score = base;
            // 特异性：常用字集越小，落进去的字越多，越可信（谚文 ⊂ GB2312 一级字区这种嵌套靠它分开）
            score += (SPECIFICITY_PER_CHAR * common as f32 * p.specificity).min(SPECIFICITY_CAP);
            score += priors.iter().filter(|(e, _)| *e == p.encoding).map(|(_, w)| *w).sum::<f32>();
            if hints.made_by_dos {
                score += DOS_PRIOR;
            }
            if vote == Some(p.encoding) {
                score += vote_weight;
            }
            Some(CandidateScore { encoding: p.encoding, score })
        })
        .collect();
    // 稳定排序：同分按候选表顺序（GB18030 在前）
    scores.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scores
}

/// 合法 UTF-8 名字按文字系统归类的结果，用来判断「读出来像不像人话」。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Script {
    Latin,
    Greek,
    Cyrillic,
    Armenian,
    Hebrew,
    Arabic,
    Other,
    /// C1 控制符、IPA/修饰符、挂在非字母后面的组合符：真实文件名里不会有
    Weird,
}

/// 一组合法 UTF-8 名字是否「读出来不像人话」：所有非 ASCII 字符都是 2 字节序列（U+0080..=U+07FF），
/// 且文字系统不一致或含怪字符。
/// 为什么：GBK 的「图片」(CD BC C6 AC)、「通知」(CD A8 D6 AA) 恰好是合法 UTF-8，直接信第 3 步会显示成 ͼƬ、֪ͨ；
/// 真正的 2 字节 UTF-8 名字（Käse、Привет、Αθήνα）文字是一致的，撞车产物则是希腊+拉丁扩展+组合符乱混。
/// 只要出现任何 3 字节以上的字符（CJK、emoji…）就不算撞车，直接返回 false。
fn utf8_looks_incoherent(raw_names: &[&[u8]]) -> bool {
    let mut scripts: HashSet<Script> = HashSet::new();
    for name in raw_names {
        let Ok(text) = std::str::from_utf8(name) else { return false };
        let mut prev_alpha = false;
        for c in text.chars() {
            let u = c as u32;
            if u >= 0x800 {
                return false;
            }
            let script = match u {
                0x00..=0x7F => {
                    prev_alpha = c.is_ascii_alphabetic();
                    continue;
                }
                0x80..=0x9F => Script::Weird,
                0xA0..=0xBF => continue,
                0xC0..=0x24F => Script::Latin,
                0x250..=0x2FF => Script::Weird,
                // NFD 名字里的组合符跟在字母后面是正常的（Finder/ditto 产物），孤零零出现才是撞车
                0x300..=0x36F if prev_alpha => continue,
                0x300..=0x36F => Script::Weird,
                0x370..=0x3FF => Script::Greek,
                0x400..=0x52F => Script::Cyrillic,
                0x530..=0x58F => Script::Armenian,
                0x590..=0x5FF => Script::Hebrew,
                0x600..=0x6FF => Script::Arabic,
                _ => Script::Other,
            };
            prev_alpha = c.is_alphabetic();
            scripts.insert(script);
        }
    }
    scripts.len() >= 2 || scripts.contains(&Script::Weird)
}

/// 对打分结果取第一名并判断是否含糊；列表为空时兜底按 UTF-8 并要求用户确认。
fn pick_top(ranked: &[CandidateScore]) -> Detection {
    let Some(top) = ranked.first() else {
        // 理论上到不了：单字节候选永远解码成功
        return Detection { encoding: NameEncoding::Utf8, confidence: 0.0, source: DetectionSource::Scored, ambiguous: true };
    };
    let runner_up = ranked.get(1).map(|c| c.score).unwrap_or(0.0);
    let ambiguous = top.score - runner_up < AMBIGUOUS_GAP || top.score < AMBIGUOUS_FLOOR;
    Detection { encoding: top.encoding, confidence: top.score.clamp(0.0, 1.0), source: DetectionSource::Scored, ambiguous }
}

/// 对一组原始名字字节做识别（不看条目标志）。tar 等只有字节的场景直接用它。
pub fn detect_raw(raw_names: &[&[u8]], hints: &Hints) -> Detection {
    if raw_names.iter().all(|n| n.is_ascii()) {
        return Detection { encoding: NameEncoding::Utf8, confidence: 1.0, source: DetectionSource::ValidUtf8, ambiguous: false };
    }
    if raw_names.iter().all(|n| std::str::from_utf8(n).is_ok()) {
        if utf8_looks_incoherent(raw_names) {
            // 合法但不像人话：看本地代码页能不能解出整齐的常用字。能 → 用它但一律标 ambiguous 让用户确认；
            // 不能 → 仍按 UTF-8，但压低置信度并标 ambiguous。
            let ranked = rank_candidates(raw_names, hints);
            if let Some(top) = ranked.first().filter(|c| c.score >= AMBIGUOUS_FLOOR) {
                return Detection { encoding: top.encoding, confidence: top.score.clamp(0.0, 1.0), source: DetectionSource::Scored, ambiguous: true };
            }
            return Detection { encoding: NameEncoding::Utf8, confidence: AMBIGUOUS_FLOOR, source: DetectionSource::ValidUtf8, ambiguous: true };
        }
        return Detection {
            encoding: NameEncoding::Utf8,
            confidence: VALID_UTF8_CONFIDENCE,
            source: DetectionSource::ValidUtf8,
            ambiguous: false,
        };
    }
    pick_top(&rank_candidates(raw_names, hints))
}

/// 这一包的 UTF-8 标志（zip bit 11 等）是否可信：只要有一个带标志的条目字节不是合法 UTF-8，整包的标志都不可信。
/// 为什么按包而不是按条目：标志是写包工具的行为，一个条目撒谎说明该工具无条件设标志；
/// 这时另一个条目的 GBK 字节恰好是合法 UTF-8（「图片/」= CD BC 2F）按条目判断就会漏网，显示成 ͼƬ。
pub fn utf8_flags_reliable(entries: &[Entry]) -> bool {
    entries.iter().filter(|e| e.name_is_utf8).all(|e| std::str::from_utf8(&e.raw_name).is_ok())
}

/// 对一组条目做编码识别。带 0x7075 名或（可信的）UTF-8 标志的条目不参与打分；
/// 其余条目全是 ASCII 时，只要有条目靠格式给出了 Unicode 名，来源就算 Format。
pub fn detect(entries: &[Entry], hints: &Hints) -> Detection {
    let trust_flags = utf8_flags_reliable(entries);
    let pool: Vec<&[u8]> = entries
        .iter()
        .filter(|e| e.unicode_name.is_none() && !(trust_flags && e.name_is_utf8))
        .map(|e| e.raw_name.as_slice())
        .collect();
    let has_format_signal = pool.len() < entries.len();
    if pool.iter().all(|n| n.is_ascii()) {
        let source = if has_format_signal { DetectionSource::Format } else { DetectionSource::ValidUtf8 };
        return Detection { encoding: NameEncoding::Utf8, confidence: 1.0, source, ambiguous: false };
    }
    detect_raw(&pool, hints)
}

/// 按给定编码解码单个条目名，`trust_flags` 是 `utf8_flags_reliable` 对整包的判断。
/// 优先级：unicode_name > （标志可信且字节合法的）name_is_utf8 > 指定编码；解不出的字节用 U+FFFD。
pub fn decode_name_with(entry: &Entry, enc: NameEncoding, trust_flags: bool) -> String {
    if let Some(u) = &entry.unicode_name {
        return u.clone();
    }
    if trust_flags && entry.name_is_utf8 {
        if let Ok(s) = std::str::from_utf8(&entry.raw_name) {
            return s.to_owned();
        }
    }
    enc.decode_lossy(&entry.raw_name)
}

/// 按给定编码解码单个条目名；优先级：unicode_name > name_is_utf8（字节合法）> 指定编码。
/// 没有整包上下文，标志按可信处理；要正确处理「bit 11 撒谎」的包请用 `decode_all` 或 `decode_name_with`。
pub fn decode_name(entry: &Entry, enc: NameEncoding) -> String {
    decode_name_with(entry, enc, true)
}

/// 按给定编码解码全部条目名，顺序与 `entries` 一致；标志可信度按整包判断。
pub fn decode_all(entries: &[Entry], enc: NameEncoding) -> Vec<String> {
    let trust_flags = utf8_flags_reliable(entries);
    entries.iter().map(|e| decode_name_with(e, enc, trust_flags)).collect()
}

/// 写入前归一化：NFC。
pub fn normalize_for_write(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::EntryKind;

    /// 用指定编码把字符串转成原始字节（测试夹具）。
    fn enc(s: &str, e: NameEncoding) -> Vec<u8> {
        let (bytes, _, had_errors) = e.encoding().encode(s);
        assert!(!had_errors, "{s} 无法用 {} 编码", e.label());
        bytes.into_owned()
    }

    /// 构造只填名字字节的条目。
    fn entry(id: u32, raw: Vec<u8>) -> Entry {
        Entry::new(id, raw, EntryKind::File, 0)
    }

    #[test]
    fn common_sets_look_right() {
        let find = |e: NameEncoding| profiles().iter().find(|p| p.encoding == e).unwrap();
        let gb = find(NameEncoding::Gb18030);
        assert_eq!(gb.common.len(), 3755, "GB2312 一级字应为 3755 个");
        assert!(gb.common.contains(&'你') && gb.common.contains(&'啊') && gb.common.contains(&'座'));
        assert!(!gb.common.contains(&'喆'), "喆 是二级字");
        let big5 = find(NameEncoding::Big5);
        assert!(big5.common.len() > 5000 && big5.common.contains(&'測') && big5.common.contains(&'一'));
        let jis = find(NameEncoding::ShiftJis);
        assert!(jis.common.contains(&'亜') && jis.common.contains(&'あ') && jis.common.contains(&'ア') && jis.common.contains(&'ー'));
        let ks = find(NameEncoding::EucKr);
        assert_eq!(ks.common.len(), 2350, "KS X 1001 谚文应为 2350 个");
        assert!(ks.common.contains(&'한'));
        let cyr = find(NameEncoding::Cp1251);
        assert_eq!(cyr.common.len(), 66);
        assert!(cyr.common.contains(&'ё'));
        let latin = find(NameEncoding::Cp1252);
        assert!(latin.common.contains(&'é') && !latin.common.contains(&'×'));
        // 特异性：谚文集最小 → 1，Big5 常用字最大 → 0，单字节编码一律 0
        assert!((ks.specificity - 1.0).abs() < 1e-6 && big5.specificity.abs() < 1e-6);
        assert!(gb.specificity > 0.3 && gb.specificity < 0.6);
        assert!(cyr.specificity == 0.0 && latin.specificity == 0.0);
    }

    #[test]
    fn classify_covers_typical_garbage() {
        assert_eq!(classify('\u{0081}'), CharClass::Control);
        assert_eq!(classify('\u{E000}'), CharClass::Pua);
        assert_eq!(classify('─'), CharClass::Symbol);
        assert_eq!(classify('ﾄ'), CharClass::Symbol);
        assert_eq!(classify('º'), CharClass::Symbol);
        assert_eq!(classify('（'), CharClass::Neutral);
        assert_eq!(classify('①'), CharClass::Neutral);
        assert_eq!(classify('№'), CharClass::Neutral);
        assert_eq!(classify('—'), CharClass::Neutral);
        assert_eq!(classify('你'), CharClass::Other);
        assert_eq!(classify('ä'), CharClass::Other);
    }

    #[test]
    fn locale_parsing() {
        assert_eq!(locale_priors("zh-CN"), vec![(NameEncoding::Gb18030, LOCALE_PRIOR)]);
        assert_eq!(locale_priors("zh_TW.UTF-8"), vec![(NameEncoding::Big5, LOCALE_PRIOR)]);
        assert_eq!(locale_priors("zh-Hant-HK"), vec![(NameEncoding::Big5, LOCALE_PRIOR)]);
        assert_eq!(locale_priors("zh-Hans-SG"), vec![(NameEncoding::Gb18030, LOCALE_PRIOR)]);
        assert_eq!(locale_priors("ja-JP"), vec![(NameEncoding::ShiftJis, LOCALE_PRIOR)]);
        assert_eq!(locale_priors("ko"), vec![(NameEncoding::EucKr, LOCALE_PRIOR)]);
        assert_eq!(locale_priors("uk-UA")[0], (NameEncoding::Cp1251, LOCALE_PRIOR));
        assert_eq!(locale_priors("de-DE"), vec![(NameEncoding::Cp1252, LOCALE_PRIOR)]);
        assert!(locale_priors("en-US").is_empty());
        assert!(locale_priors("").is_empty());
    }

    #[test]
    fn ids_round_trip() {
        for e in NameEncoding::all() {
            assert_eq!(NameEncoding::from_id(e.id()), Some(*e));
        }
        assert_eq!(NameEncoding::from_id("GBK"), Some(NameEncoding::Gb18030));
        assert_eq!(NameEncoding::from_id("Shift-JIS"), Some(NameEncoding::ShiftJis));
        assert_eq!(NameEncoding::from_id("windows-1251"), Some(NameEncoding::Cp1251));
        assert_eq!(NameEncoding::from_id("klingon"), None);
    }

    #[test]
    fn utf8_flag_and_unicode_name_short_circuit() {
        let mut flagged = entry(0, enc("你好世界.txt", NameEncoding::Utf8));
        flagged.name_is_utf8 = true;
        let mut with_7075 = entry(1, enc("文档/报告.docx", NameEncoding::Gb18030));
        with_7075.unicode_name = Some("文档/报告.docx".into());
        let det = detect(&[flagged.clone(), with_7075.clone()], &Hints::default());
        assert_eq!(det.encoding, NameEncoding::Utf8);
        assert_eq!(det.source, DetectionSource::Format);
        assert!(!det.ambiguous);
        assert_eq!(decode_name(&flagged, NameEncoding::Gb18030), "你好世界.txt");
        assert_eq!(decode_name(&with_7075, NameEncoding::Big5), "文档/报告.docx");
    }

    #[test]
    fn lying_utf8_flag_falls_back_to_bytes() {
        let mut liar = entry(0, enc("你好世界.txt", NameEncoding::Gb18030));
        liar.name_is_utf8 = true;
        // 「图片/」的 GBK 字节恰好是合法 UTF-8，按条目判断会漏网，按包判断不会
        let mut accidental = entry(1, enc("图片/", NameEncoding::Gb18030));
        accidental.name_is_utf8 = true;
        let entries = vec![liar.clone(), accidental.clone()];
        assert!(!utf8_flags_reliable(&entries));
        let det = detect(&entries, &Hints { made_by_dos: true, locale: Some("zh-CN".into()) });
        assert_eq!(det.encoding, NameEncoding::Gb18030);
        assert_eq!(det.source, DetectionSource::Scored);
        assert_eq!(decode_all(&entries, det.encoding), vec!["你好世界.txt", "图片/"]);
        // 单条解码没有整包上下文：字节非法时回退到指定编码，恰好合法时信标志
        assert_eq!(decode_name(&liar, det.encoding), "你好世界.txt");
        assert_eq!(decode_name(&accidental, det.encoding), "ͼƬ/");
        assert_eq!(decode_name_with(&accidental, det.encoding, false), "图片/");
    }

    #[test]
    fn format_source_when_only_ascii_left_unflagged() {
        let mut with_7075 = entry(0, enc("合同/", NameEncoding::Gb18030));
        with_7075.unicode_name = Some("合同/".into());
        let entries = vec![with_7075, entry(1, b"readme.txt".to_vec())];
        let det = detect(&entries, &Hints::default());
        assert_eq!((det.encoding, det.source), (NameEncoding::Utf8, DetectionSource::Format));
    }

    #[test]
    fn all_valid_utf8_without_flags() {
        let entries = vec![entry(0, enc("你好世界.txt", NameEncoding::Utf8)), entry(1, b"__MACOSX/._x".to_vec())];
        let det = detect(&entries, &Hints::default());
        assert_eq!(det.encoding, NameEncoding::Utf8);
        assert_eq!(det.source, DetectionSource::ValidUtf8);
        assert!(det.confidence >= 0.9 && !det.ambiguous);
    }

    #[test]
    fn pure_ascii_and_empty() {
        let det = detect(&[entry(0, b"readme.txt".to_vec())], &Hints::default());
        assert_eq!((det.encoding, det.source, det.confidence), (NameEncoding::Utf8, DetectionSource::ValidUtf8, 1.0));
        let det = detect(&[], &Hints::default());
        assert_eq!(det.encoding, NameEncoding::Utf8);
        assert!(!det.ambiguous);
    }

    #[test]
    fn decode_lossy_marks_bad_bytes() {
        let e = entry(0, vec![0xC4, 0xE3, 0xFF, 0x2E, 0x74]);
        let s = decode_name(&e, NameEncoding::Gb18030);
        assert!(s.starts_with('你') && s.contains('\u{FFFD}') && s.ends_with(".t"));
        assert!(decode_name(&e, NameEncoding::Utf8).contains('\u{FFFD}'));
    }

    #[test]
    fn accidental_utf8_from_gbk_is_caught() {
        // 「图片」「通知」的 GBK 字节恰好是合法 UTF-8
        for name in ["图片.docx", "通知.txt"] {
            let raw = enc(name, NameEncoding::Gb18030);
            assert!(std::str::from_utf8(&raw).is_ok(), "{name} 的 GBK 字节应是合法 UTF-8，否则本测试失去意义");
            let det = detect_raw(&[raw.as_slice()], &Hints { made_by_dos: true, locale: Some("zh-CN".into()) });
            assert_eq!(det.encoding, NameEncoding::Gb18030, "{name}");
            assert!(det.ambiguous, "撞车场景必须要求用户确认");
        }
    }

    #[test]
    fn coherent_two_byte_utf8_stays_utf8() {
        for name in ["Käse Straße.txt", "Привет.txt", "Αθήνα.txt", "שלום.txt", "مرحبا.txt", "e\u{301}cole.txt", "©2024 90°.jpg"] {
            let raw = name.as_bytes();
            let det = detect_raw(&[raw], &Hints::default());
            assert_eq!(det.encoding, NameEncoding::Utf8, "{name}");
            assert_eq!(det.source, DetectionSource::ValidUtf8);
            assert!(!det.ambiguous, "{name}");
        }
    }

    #[test]
    fn normalize_is_nfc() {
        assert_eq!(normalize_for_write("e\u{0301}"), "\u{00E9}");
        assert_eq!(normalize_for_write("한국어"), "한국어");
    }
}
