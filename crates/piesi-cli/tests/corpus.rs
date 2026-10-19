//! 语料回归：tests/corpus/manifest.json 里每个压缩包，打开后识别出的编码和解码后的名字必须与预期一致。
//! 这是合并门禁；改格式驱动或编码算法必须让它全绿。

use std::path::{Path, PathBuf};

use piesi_core::facade::Archive;

/// manifest 里的一条。
#[derive(serde::Deserialize)]
struct Case {
    file: String,
    expected_encoding: String,
    expected_names: Vec<String>,
    #[serde(default)]
    ambiguous: bool,
    #[serde(default)]
    note: String,
    /// 需要密码的包
    #[serde(default)]
    password: Option<String>,
    /// 识别用的先验（locale 为 None 表示不用地区先验）
    #[serde(default)]
    hints: CaseHints,
    /// 预期的判定来源（snake_case，如 valid_utf8）
    #[serde(default)]
    expected_source: Option<String>,
}

/// manifest 里的先验。
#[derive(serde::Deserialize, Default)]
struct CaseHints {
    #[serde(default)]
    locale: Option<String>,
}

/// CamelCase → snake_case，用来和 manifest 的来源标签比对。
fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// 仓库根目录下的 tests/corpus。
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus")
}

/// 把 NameEncoding 的 Debug 名和 manifest 里的小写标签对上。
fn encoding_label(e: piesi_core::encoding::NameEncoding) -> String {
    format!("{e:?}").to_lowercase()
}

#[test]
fn corpus_roundtrip() {
    let dir = corpus_dir();
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).expect("读取 manifest.json");
    let cases: Vec<Case> = serde_json::from_str(&manifest).expect("解析 manifest.json");
    assert!(!cases.is_empty(), "manifest 为空");
    let mut failures = Vec::new();
    for case in &cases {
        let path = dir.join(&case.file);
        let mut archive = match Archive::open(&path, case.hints.locale.as_deref()) {
            Ok(a) => a,
            Err(e) => {
                failures.push(format!("{}：打开失败：{e}", case.file));
                continue;
            }
        };
        if let Some(pw) = &case.password {
            if let Err(e) = archive.set_password(Some(pw)) {
                failures.push(format!("{}：设置密码失败：{e}", case.file));
                continue;
            }
        }
        let d = archive.detection();
        let got = encoding_label(d.encoding);
        if got != case.expected_encoding.to_lowercase().replace(['-', '_'], "") {
            failures.push(format!("{}：编码 {} ≠ 预期 {}（置信度 {:.2}，{:?}）{}", case.file, got, case.expected_encoding, d.confidence, d.source, case.note));
        }
        if let Some(src) = &case.expected_source {
            let got_src = snake(&format!("{:?}", d.source));
            if &got_src != src {
                failures.push(format!("{}：来源 {} ≠ 预期 {}", case.file, got_src, src));
            }
        }
        if d.ambiguous != case.ambiguous {
            failures.push(format!("{}：ambiguous {} ≠ 预期 {}", case.file, d.ambiguous, case.ambiguous));
        }
        let mut names: Vec<String> = archive.names().iter().map(|n| n.trim_end_matches('/').to_owned()).collect();
        names.sort();
        let mut expected: Vec<String> = case.expected_names.iter().map(|n| n.trim_end_matches('/').to_owned()).collect();
        expected.sort();
        if names != expected {
            failures.push(format!("{}：名字不一致\n  得到 {:?}\n  预期 {:?}", case.file, names, expected));
        }
    }
    assert!(failures.is_empty(), "语料回归失败 {} 项：\n{}", failures.len(), failures.join("\n"));
}
