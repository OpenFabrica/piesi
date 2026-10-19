//! 短文件名识别：设计文档点名的「你好世界」「測試檔案」「日本語ファイル」「한국어파일」「Привет」，
//! 以及 GBK/Big5 互相合法、谚文区 ⊂ GB2312 一级字区这两个陷阱。

use piesi_core::archive::{Entry, EntryKind};
use piesi_core::encoding::{decode_name, detect, rank_candidates, DetectionSource, Hints, NameEncoding};

/// 用指定编码把字符串转成原始字节。
fn enc(s: &str, e: NameEncoding) -> Vec<u8> {
    let (bytes, _, had_errors) = e.encoding().encode(s);
    assert!(!had_errors, "{s} 无法用 {} 编码", e.label());
    bytes.into_owned()
}

/// 构造无标志条目列表。
fn entries(names: &[&str], e: NameEncoding) -> Vec<Entry> {
    names.iter().enumerate().map(|(i, n)| Entry::new(i as u32, enc(n, e), EntryKind::File, 0)).collect()
}

/// 断言一组名字在给定先验下识别成 `expected`，并且解码回原文。
fn assert_detects(names: &[&str], expected: NameEncoding, hints: &Hints, want_ambiguous: Option<bool>) {
    let es = entries(names, expected);
    let det = detect(&es, hints);
    assert_eq!(det.encoding, expected, "{names:?} hints={hints:?} → {det:?}");
    assert_eq!(det.source, DetectionSource::Scored);
    if let Some(a) = want_ambiguous {
        assert_eq!(det.ambiguous, a, "{names:?} hints={hints:?} ambiguous → {det:?}");
    }
    for (e, n) in es.iter().zip(names) {
        assert_eq!(decode_name(e, det.encoding), *n);
    }
}

#[test]
fn five_canonical_short_names_without_locale() {
    let no_hints = Hints::default();
    assert_detects(&["你好世界.txt"], NameEncoding::Gb18030, &no_hints, Some(false));
    assert_detects(&["測試檔案.txt"], NameEncoding::Big5, &no_hints, Some(false));
    assert_detects(&["日本語ファイル.txt"], NameEncoding::ShiftJis, &no_hints, Some(false));
    assert_detects(&["Привет.txt"], NameEncoding::Cp1251, &no_hints, Some(false));
    // 谚文字节当 GBK 解全是一级汉字，单个 5 字名只能靠特异性 + chardetng 分开，结果正确但要用户确认
    assert_detects(&["한국어파일.txt"], NameEncoding::EucKr, &no_hints, Some(true));
}

#[test]
fn korean_becomes_confident_with_more_names_or_locale() {
    let set = ["한국어파일.txt", "문서/보고서.docx", "사진/서울.jpg"];
    assert_detects(&set, NameEncoding::EucKr, &Hints::default(), Some(false));
    assert_detects(&["한국어파일.txt"], NameEncoding::EucKr, &Hints { made_by_dos: true, locale: Some("ko-KR".into()) }, Some(false));
    // 即使 locale 是 zh-CN，三个名字的谚文也压得过 GBK 的先验（但先验把差距拉近，允许要求确认）
    assert_detects(&set, NameEncoding::EucKr, &Hints { made_by_dos: true, locale: Some("zh-CN".into()) }, None);
}

#[test]
fn gbk_vs_big5_trap_is_split_by_common_char_ratio() {
    // GBK 字节当 Big5 解通常也「合法」，Big5 字节当 GBK 解会落进私用区；靠常用字比例与惩罚分开
    for (name, e) in [("你好世界.txt", NameEncoding::Gb18030), ("文档/报告.docx", NameEncoding::Gb18030), ("測試檔案.txt", NameEncoding::Big5), ("圖片/相片.jpg", NameEncoding::Big5)] {
        let raw = enc(name, e);
        let ranked = rank_candidates(&[raw.as_slice()], &Hints::default());
        assert_eq!(ranked[0].encoding, e, "{name}: {ranked:?}");
        let other = if e == NameEncoding::Gb18030 { NameEncoding::Big5 } else { NameEncoding::Gb18030 };
        let other_score = ranked.iter().find(|c| c.encoding == other).map(|c| c.score).unwrap_or(0.0);
        assert!(ranked[0].score - other_score >= 0.25, "{name}: 与对方差距不够 {ranked:?}");
    }
    // 繁体「你好」的 Big5 字节当 GBK 解全是私用区，GBK 分数应接近 0
    let raw = enc("你好.txt", NameEncoding::Big5);
    let ranked = rank_candidates(&[raw.as_slice()], &Hints::default());
    assert_eq!(ranked[0].encoding, NameEncoding::Big5);
    assert!(ranked.iter().find(|c| c.encoding == NameEncoding::Gb18030).is_none_or(|c| c.score < 0.1), "{ranked:?}");
}

#[test]
fn two_char_names_are_ambiguous_and_follow_locale() {
    // 2 字名证据不足：GBK/Big5/EUC-KR 都能解出常用字，必须标 ambiguous，并由 locale 决定默认值
    let gbk = entries(&["你好.txt"], NameEncoding::Gb18030);
    let det = detect(&gbk, &Hints { made_by_dos: true, locale: Some("zh-CN".into()) });
    assert_eq!((det.encoding, det.ambiguous), (NameEncoding::Gb18030, true));
    let det = detect(&gbk, &Hints::default());
    assert!(det.ambiguous, "无 locale 的 2 字名必须要求确认：{det:?}");

    let kr = entries(&["서울.jpg"], NameEncoding::EucKr);
    let det = detect(&kr, &Hints { made_by_dos: true, locale: Some("ko-KR".into()) });
    assert_eq!((det.encoding, det.ambiguous), (NameEncoding::EucKr, true));
}

#[test]
fn locale_prior_breaks_ties_but_cannot_override_clear_evidence() {
    // 四字 GBK 名在 zh-TW / ko-KR / ru-RU 先验下仍是 GBK
    for loc in ["zh-TW", "ko-KR", "ru-RU", "ja-JP", "de-DE"] {
        assert_detects(&["你好世界.txt"], NameEncoding::Gb18030, &Hints { made_by_dos: true, locale: Some(loc.into()) }, Some(false));
        assert_detects(&["日本語ファイル.txt"], NameEncoding::ShiftJis, &Hints { made_by_dos: false, locale: Some(loc.into()) }, Some(false));
    }
}

#[test]
fn cyrillic_code_pages_are_told_apart() {
    let names = ["Привет.txt", "Документы/Отчёт.docx"];
    assert_detects(&names, NameEncoding::Cp1251, &Hints::default(), Some(false));
    assert_detects(&names, NameEncoding::Cp866, &Hints::default(), Some(false));
    // ru 先验：CP1251 优先于 CP866，但 CP866 字节仍识别为 CP866
    let ru = Hints { made_by_dos: true, locale: Some("ru-RU".into()) };
    assert_detects(&names, NameEncoding::Cp1251, &ru, Some(false));
    assert_detects(&names, NameEncoding::Cp866, &ru, Some(false));
}

#[test]
fn western_names_are_cp1252_not_cyrillic() {
    // 「résumé」的 E9 在 CP1251 里是合法的 й，靠「西里尔字母紧贴拉丁字母」罚分压下去
    assert_detects(&["résumé.pdf"], NameEncoding::Cp1252, &Hints::default(), Some(false));
    assert_detects(&["Übersicht/Käse Straße.txt", "Ärger.txt"], NameEncoding::Cp1252, &Hints::default(), Some(false));
}

#[test]
fn cjk_bytes_do_not_pass_as_cp1252() {
    // 全部非 ASCII 的 CJK 字节用 CP1252 解出来是一串带变音字母，密度惩罚应把它压到很低
    let raw = enc("你好世界.txt", NameEncoding::Gb18030);
    let ranked = rank_candidates(&[raw.as_slice()], &Hints::default());
    let cp1252 = ranked.iter().find(|c| c.encoding == NameEncoding::Cp1252).unwrap();
    assert!(cp1252.score < 0.5, "{ranked:?}");
}

#[test]
fn names_with_only_fullwidth_punctuation_are_uninformative() {
    // 「（1）.txt」：GBK 解出全角括号，没有汉字证据；带 zh-CN 先验选 GBK 但必须 ambiguous
    let raw = enc("（1）.txt", NameEncoding::Gb18030);
    let det = piesi_core::encoding::detect_raw(&[raw.as_slice()], &Hints { made_by_dos: true, locale: Some("zh-CN".into()) });
    assert_eq!(det.encoding, NameEncoding::Gb18030);
    assert!(det.ambiguous);
}

#[test]
fn print_canonical_score_table() {
    // 不做断言，只打印五个标准短名的候选打分表（cargo test -- --nocapture），给调参留个参照
    let cases = [
        ("你好世界.txt", NameEncoding::Gb18030),
        ("測試檔案.txt", NameEncoding::Big5),
        ("日本語ファイル.txt", NameEncoding::ShiftJis),
        ("한국어파일.txt", NameEncoding::EucKr),
        ("Привет.txt", NameEncoding::Cp1251),
    ];
    for (name, e) in cases {
        let raw = enc(name, e);
        for locale in [None, Some("zh-CN")] {
            let hints = Hints { made_by_dos: true, locale: locale.map(String::from) };
            let ranked = rank_candidates(&[raw.as_slice()], &hints);
            let det = detect(&entries(&[name], e), &hints);
            let row: Vec<String> = ranked.iter().map(|c| format!("{}={:.3}", c.encoding.id(), c.score)).collect();
            println!("{name:14} locale={:6} → {} {} | {}", locale.unwrap_or("-"), det.encoding.id(), if det.ambiguous { "ambiguous" } else { "confident" }, row.join("  "));
        }
    }
}
