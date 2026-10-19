//! 语料回归：不经 zip 读取器，直接用 `tests/corpus/raw_names.json` 里的原始字节构造 `Entry`，
//! 跑 `detect` / `decode_all`，与 `manifest.json` 的预期比对。夹具由 `scripts/gen_corpus.py` 生成。

use std::collections::HashMap;
use std::path::PathBuf;

use piesi_core::archive::{Entry, EntryKind};
use piesi_core::encoding::{decode_all, detect, DetectionSource, Hints, NameEncoding};
use piesi_core::names::is_junk_path;

/// manifest.json 的一条记录。
#[derive(serde::Deserialize)]
struct Record {
    file: String,
    expected_encoding: String,
    expected_source: String,
    expected_names: Vec<String>,
    ambiguous: bool,
    hints: RecordHints,
    note: String,
    #[serde(default)]
    expected_junk: Vec<String>,
}

/// manifest 里的先验。
#[derive(serde::Deserialize)]
struct RecordHints {
    made_by_dos: bool,
    locale: Option<String>,
}

/// raw_names.json 里一个包的条目。
#[derive(serde::Deserialize)]
struct RawArchive {
    entries: Vec<RawEntry>,
}

/// raw_names.json 里一个条目的原始字节与标志。
#[derive(serde::Deserialize)]
struct RawEntry {
    raw_name: Vec<u8>,
    flags: u16,
    unicode_name: Option<String>,
    is_dir: bool,
}

/// 语料目录：crates/piesi-core/../../tests/corpus。
fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus")
}

/// 读 manifest 与 raw_names。
fn load() -> (Vec<Record>, HashMap<String, RawArchive>) {
    let dir = corpus_dir();
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("读 manifest.json（先跑 python3 -I scripts/gen_corpus.py tests/corpus）");
    let raw = std::fs::read_to_string(dir.join("raw_names.json")).expect("读 raw_names.json");
    (serde_json::from_str(&manifest).expect("解析 manifest.json"), serde_json::from_str(&raw).expect("解析 raw_names.json"))
}

/// 把 raw_names 里的条目变成引擎的 Entry（模拟 zip 读取器的输出）。
fn to_entries(raw: &RawArchive) -> Vec<Entry> {
    raw.entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let kind = if e.is_dir { EntryKind::Dir } else { EntryKind::File };
            let mut entry = Entry::new(i as u32, e.raw_name.clone(), kind, 0);
            entry.name_is_utf8 = e.flags & 0x0800 != 0;
            entry.unicode_name = e.unicode_name.clone();
            entry
        })
        .collect()
}

/// DetectionSource 与 manifest 字符串的对应。
fn source_id(source: DetectionSource) -> &'static str {
    match source {
        DetectionSource::Format => "format",
        DetectionSource::ValidUtf8 => "valid_utf8",
        DetectionSource::Scored => "scored",
        DetectionSource::Manual => "manual",
    }
}

#[test]
fn every_fixture_file_exists() {
    let (manifest, raw) = load();
    assert!(manifest.len() >= 40, "语料太少：{}", manifest.len());
    for r in &manifest {
        assert!(corpus_dir().join(&r.file).is_file(), "缺夹具文件 {}", r.file);
        assert!(raw.contains_key(&r.file), "raw_names.json 缺 {}", r.file);
    }
}

#[test]
fn corpus_detection_and_names_match_manifest() {
    let (manifest, raw) = load();
    let mut failures: Vec<String> = Vec::new();
    for r in &manifest {
        let entries = to_entries(&raw[&r.file]);
        let hints = Hints { made_by_dos: r.hints.made_by_dos, locale: r.hints.locale.clone() };
        let det = detect(&entries, &hints);
        let expected = NameEncoding::from_id(&r.expected_encoding).unwrap_or_else(|| panic!("manifest 里未知编码 {}", r.expected_encoding));
        let mut problems = Vec::new();
        if det.encoding != expected {
            problems.push(format!("编码 {} ≠ 预期 {}", det.encoding.id(), r.expected_encoding));
        }
        if source_id(det.source) != r.expected_source {
            problems.push(format!("来源 {} ≠ 预期 {}", source_id(det.source), r.expected_source));
        }
        if det.ambiguous != r.ambiguous {
            problems.push(format!("ambiguous {} ≠ 预期 {}（置信度 {:.3}）", det.ambiguous, r.ambiguous, det.confidence));
        }
        let names = decode_all(&entries, det.encoding);
        if names != r.expected_names {
            problems.push(format!("名字 {names:?} ≠ 预期 {:?}", r.expected_names));
        }
        for name in &names {
            let should_be_junk = r.expected_junk.contains(name);
            if is_junk_path(name) != should_be_junk {
                problems.push(format!("is_junk_path({name:?}) 应为 {should_be_junk}"));
            }
        }
        if !problems.is_empty() {
            failures.push(format!("{} [{}]\n    {}", r.file, r.note, problems.join("\n    ")));
        }
    }
    assert!(failures.is_empty(), "{} 个语料包不符合预期：\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn manual_override_redecodes_without_rereading() {
    // 用户在 UI 里切换编码时只重解码：同一组 Entry 用不同编码解出不同名字，带 0x7075/bit 11 的条目不受影响
    let (_, raw) = load();
    let entries = to_entries(&raw["encoding/mixed_gbk_and_utf8.zip"]);
    let as_gbk = decode_all(&entries, NameEncoding::Gb18030);
    let as_big5 = decode_all(&entries, NameEncoding::Big5);
    assert_eq!(as_gbk[1], "新建文件夹/旧文件.txt");
    assert_ne!(as_big5[1], as_gbk[1], "切到 Big5 后无标志条目应重解码");
    assert_eq!(as_big5[2], "新建文件夹/新增.txt", "bit 11 条目不随手动编码变化");
}
