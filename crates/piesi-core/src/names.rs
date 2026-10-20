//! 文件名规则：垃圾文件过滤、相对路径安全化、Windows 安全名、APFS 255 字节截断、大小写冲突。
//! 这里只处理「已解码的字符串」，编码识别在 `encoding` 模块；落盘时的系统调用级防护在 `fs` 模块（cap-std）。

use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::error::{Error, Result};

/// APFS 单个路径分量的上限：255 字节（不是字符）。
pub const APFS_COMPONENT_MAX_BYTES: usize = 255;

/// `windows_safe_name` 做过的改动。给用户看的提示走 `Warning::RenamedForWindows`（只带新旧名字），这里的原因明细供测试和日志核对。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NameWarning {
    /// 替换了 Windows 不允许的字符（`\ / : * ? " < > |` 与控制字符），附替换个数
    IllegalCharsReplaced(usize),
    /// 去掉了尾部的空格和点（Windows 会静默吞掉它们，导致名字对不上）
    TrailingDotsOrSpacesTrimmed,
    /// 命中 Windows 保留设备名（CON、PRN、AUX、NUL、COM1-9、LPT1-9），加了后缀
    ReservedName,
    /// 处理后名字为空，用占位名代替
    EmptyName,
}

/// 是否是不该解出来的垃圾条目：`__MACOSX/`、`.DS_Store`、`._*` AppleDouble、`Thumbs.db`、`desktop.ini`、
/// `.Spotlight-V100`、`.fseventsd`、`.Trashes`。路径里任一分量命中即算垃圾（目录名命中则整棵子树都是）。
pub fn is_junk_path(path: &str) -> bool {
    let components = path.split(['/', '\\']).filter(|c| !c.is_empty());
    let mut last: Option<&str> = None;
    for c in components {
        if matches!(c, "__MACOSX" | ".Spotlight-V100" | ".fseventsd" | ".Trashes") {
            return true;
        }
        last = Some(c);
    }
    let Some(name) = last else { return false };
    // AppleDouble 资源分支：`._` 开头且后面还有内容（单独的 `._` 不算，虽然也没人会这么起名）
    if name.len() > 2 && name.starts_with("._") {
        return true;
    }
    // Thumbs.db / desktop.ini 在 Windows 上大小写不敏感；.DS_Store 只认精确拼写
    name == ".DS_Store" || name.eq_ignore_ascii_case("Thumbs.db") || name.eq_ignore_ascii_case("desktop.ini")
}

/// 把压缩包里的条目名变成安全的相对路径：`\` 转 `/`、折叠 `//`、去前导 `./`、去尾部 `/`。
/// 拒绝：空路径、`.`/`..` 分量、绝对路径（含 `\\?\` 与 UNC）、Windows 盘符（`C:`）、控制字符。
/// 注意：目录条目的尾部 `/` 会被去掉，是否是目录以 `Entry::kind` 为准；`./` 或 `.` 这种根目录条目会返回
/// `UnsafePath`，调用方应把它当作「跳过」而不是错误中止。
pub fn sanitize_rel_path(raw: &str) -> Result<String> {
    if raw.chars().any(|c| c.is_control()) {
        return Err(Error::UnsafePath(format!("含控制字符：{}", raw.escape_debug())));
    }
    let unified = raw.replace('\\', "/");
    if unified.starts_with('/') {
        // 覆盖 `/abs`、`//server/share`、`//?/C:/x`
        return Err(Error::UnsafePath(format!("绝对路径：{raw}")));
    }
    if has_drive_prefix(&unified) {
        return Err(Error::UnsafePath(format!("Windows 盘符：{raw}")));
    }
    // 去前导 `./`（可能叠多层，tar -cf x.tar . 的产物就是 `./a/b`）
    let mut rest = unified.as_str();
    while let Some(stripped) = rest.strip_prefix("./") {
        rest = stripped;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" => continue,
            "." | ".." => return Err(Error::UnsafePath(format!("路径含 `{part}` 分量：{raw}"))),
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(Error::UnsafePath(format!("空路径：{raw:?}")));
    }
    Ok(parts.join("/"))
}

/// 路径是否以 Windows 盘符开头（`C:`、`c:/x`）。只看第一个分量，中间的 `a/C:` 在 Unix 上无害。
fn has_drive_prefix(path: &str) -> bool {
    let mut chars = path.chars();
    matches!((chars.next(), chars.next()), (Some(l), Some(':')) if l.is_ascii_alphabetic())
}

/// Windows 不允许出现在文件名里的字符 → 对应全角字符（保留可读性，比 `_` 更不容易撞名）。
fn fullwidth_replacement(c: char) -> Option<char> {
    Some(match c {
        '\\' => '＼',
        '/' => '／',
        ':' => '：',
        '*' => '＊',
        '?' => '？',
        '"' => '＂',
        '<' => '＜',
        '>' => '＞',
        '|' => '｜',
        _ => return None,
    })
}

/// Windows 保留设备名，大小写不敏感，且只看第一个点之前的主干（`con.txt` 也是保留名）。
fn is_windows_reserved(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let (base, digit) = (upper.get(..3), upper.get(3..));
            matches!(base, Some("COM") | Some("LPT")) && matches!(digit, Some(d) if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && d != "0")
        }
    }
}

/// 把单个路径分量改成 Windows 能接受的名字：非法字符换全角、控制字符换 `_`、去尾部空格和点、
/// 保留名加 `_` 后缀。返回新名字和改动清单（空清单表示原样可用）。
pub fn windows_safe_name(component: &str) -> (String, Vec<NameWarning>) {
    let mut warnings = Vec::new();
    let mut replaced = 0usize;
    let mut out = String::with_capacity(component.len());
    for c in component.chars() {
        if let Some(r) = fullwidth_replacement(c) {
            out.push(r);
            replaced += 1;
        } else if c.is_control() {
            out.push('_');
            replaced += 1;
        } else {
            out.push(c);
        }
    }
    if replaced > 0 {
        warnings.push(NameWarning::IllegalCharsReplaced(replaced));
    }
    let trimmed = out.trim_end_matches([' ', '.']);
    if trimmed.len() != out.len() {
        warnings.push(NameWarning::TrailingDotsOrSpacesTrimmed);
        out = trimmed.to_owned();
    }
    if out.is_empty() {
        warnings.push(NameWarning::EmptyName);
        return ("_".to_owned(), warnings);
    }
    // 保留名：主干加 `_`，扩展名保持（CON → CON_，con.txt → con_.txt）
    let (stem, ext) = match out.find('.') {
        Some(i) => (&out[..i], &out[i..]),
        None => (out.as_str(), ""),
    };
    if is_windows_reserved(stem) {
        warnings.push(NameWarning::ReservedName);
        out = format!("{stem}_{ext}");
    }
    (out, warnings)
}

/// 把单个路径分量压到 APFS 的 255 字节以内：超长时按 grapheme 截断主干、保留扩展名，
/// 加 `~` + 6 位十六进制哈希后缀，保证同一原名总得到同一结果、不同原名极少撞名。
pub fn fit_apfs_component(component: &str) -> String {
    if component.len() <= APFS_COMPONENT_MAX_BYTES {
        return component.to_owned();
    }
    // 哈希取自完整原名，这样「前 200 字节相同、后面不同」的两个名字不会截成同一个
    let suffix = format!("~{:06x}", crc32fast::hash(component.as_bytes()) & 0x00FF_FFFF);
    let (stem, ext) = split_extension(component);
    // 扩展名本身太长（或留给主干的空间不够一个字符）就不保留它，整体当主干截
    let budget = APFS_COMPONENT_MAX_BYTES.saturating_sub(suffix.len() + ext.len());
    let (stem, ext, budget) = if ext.len() > 16 || budget < 4 {
        (component, "", APFS_COMPONENT_MAX_BYTES - suffix.len())
    } else {
        (stem, ext, budget)
    };
    let mut kept = String::new();
    for g in stem.graphemes(true) {
        if kept.len() + g.len() > budget {
            break;
        }
        kept.push_str(g);
    }
    let result = format!("{kept}{suffix}{ext}");
    debug_assert!(result.len() <= APFS_COMPONENT_MAX_BYTES);
    result
}

/// 拆出扩展名（含点）。点开头的隐藏文件（`.bashrc`）和没有点的名字扩展名为空。
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// 找出在大小写不敏感、归一化不敏感的文件系统（macOS 默认 APFS）上会落到同一个文件的路径。
/// 返回下标分组（每组按下标升序，组按首个下标排序），只报「至少两个成员拼写不同」的组；
/// 完全相同的重复路径是另一个问题（重复条目），不在这里报。
pub fn find_case_collisions(paths: &[String]) -> Vec<Vec<usize>> {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in paths.iter().enumerate() {
        groups.entry(fold_key(p)).or_default().push(i);
    }
    let mut result: Vec<Vec<usize>> = groups
        .into_values()
        .filter(|g| g.len() >= 2 && g.iter().any(|&i| paths[i] != paths[g[0]]))
        .collect();
    result.sort_by_key(|g| g[0]);
    result
}

/// 碰撞键：去尾部 `/`，NFC 归一化，再小写。APFS 既不区分大小写也不区分 NFC/NFD，所以两者都要折叠。
fn fold_key(path: &str) -> String {
    path.trim_end_matches('/').nfc().collect::<String>().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_paths() {
        for p in [
            "__MACOSX/",
            "__MACOSX/._a.txt",
            "dir/__MACOSX/x",
            ".DS_Store",
            "photos/.DS_Store",
            "._report.docx",
            "docs/._report.docx",
            "Thumbs.db",
            "thumbs.DB",
            "desktop.ini",
            ".Spotlight-V100/store",
            ".fseventsd/",
            ".Trashes/501/x",
        ] {
            assert!(is_junk_path(p), "{p} 应是垃圾");
        }
        for p in ["report.docx", "docs/report.docx", "_.txt", "._", "DS_Store", "my.DS_Store", "Thumbs.db.txt", ".hidden", "__MACOSX_notes.txt", ""] {
            assert!(!is_junk_path(p), "{p} 不应是垃圾");
        }
    }

    #[test]
    fn sanitize_accepts_normal_paths() {
        assert_eq!(sanitize_rel_path("a/b/c.txt").unwrap(), "a/b/c.txt");
        assert_eq!(sanitize_rel_path("a\\b\\c.txt").unwrap(), "a/b/c.txt");
        assert_eq!(sanitize_rel_path("a//b///c").unwrap(), "a/b/c");
        assert_eq!(sanitize_rel_path("./a/b").unwrap(), "a/b");
        assert_eq!(sanitize_rel_path("././a").unwrap(), "a");
        assert_eq!(sanitize_rel_path("dir/").unwrap(), "dir");
        assert_eq!(sanitize_rel_path("文档/报告.docx").unwrap(), "文档/报告.docx");
        assert_eq!(sanitize_rel_path("a/C:/b").unwrap(), "a/C:/b");
        assert_eq!(sanitize_rel_path("...").unwrap(), "...");
        assert_eq!(sanitize_rel_path("..a/b..").unwrap(), "..a/b..");
    }

    #[test]
    fn sanitize_rejects_unsafe_paths() {
        for p in ["", "/", "./", ".", "..", "../x", "a/../b", "a/./b", "/etc/passwd", "\\\\server\\share\\x", "\\\\?\\C:\\x", "C:", "C:\\Windows", "c:/x", "a\u{0}b", "a\nb", "a/\x7fb"] {
            match sanitize_rel_path(p) {
                Err(Error::UnsafePath(_)) => {}
                other => panic!("{p:?} 应被拒绝，实际 {other:?}"),
            }
        }
    }

    #[test]
    fn windows_name_replaces_illegal_chars() {
        let (name, warns) = windows_safe_name("a:b*c?d\"e<f>g|h\\i/j");
        assert_eq!(name, "a：b＊c？d＂e＜f＞g｜h＼i／j");
        assert_eq!(warns, vec![NameWarning::IllegalCharsReplaced(9)]);
        let (name, warns) = windows_safe_name("tab\there");
        assert_eq!(name, "tab_here");
        assert_eq!(warns, vec![NameWarning::IllegalCharsReplaced(1)]);
        let (name, warns) = windows_safe_name("报告（最终版）.docx");
        assert_eq!(name, "报告（最终版）.docx");
        assert!(warns.is_empty());
    }

    #[test]
    fn windows_name_trims_trailing_dots_and_spaces() {
        let (name, warns) = windows_safe_name("report. ");
        assert_eq!(name, "report");
        assert_eq!(warns, vec![NameWarning::TrailingDotsOrSpacesTrimmed]);
        let (name, warns) = windows_safe_name("...");
        assert_eq!(name, "_");
        assert_eq!(warns, vec![NameWarning::TrailingDotsOrSpacesTrimmed, NameWarning::EmptyName]);
        let (name, warns) = windows_safe_name(" leading space kept");
        assert_eq!(name, " leading space kept");
        assert!(warns.is_empty());
    }

    #[test]
    fn windows_reserved_names() {
        for (input, expected) in [("CON", "CON_"), ("con.txt", "con_.txt"), ("Lpt1", "Lpt1_"), ("COM9.log", "COM9_.log"), ("nul.tar.gz", "nul_.tar.gz")] {
            let (name, warns) = windows_safe_name(input);
            assert_eq!(name, expected, "{input}");
            assert_eq!(warns, vec![NameWarning::ReservedName], "{input}");
        }
        for ok in ["CON1", "COM0", "COM10", "LPT", "console.txt", "aux_data", "nulls"] {
            let (name, warns) = windows_safe_name(ok);
            assert_eq!(name, ok);
            assert!(warns.is_empty(), "{ok}");
        }
    }

    #[test]
    fn apfs_short_names_untouched() {
        assert_eq!(fit_apfs_component("report.docx"), "report.docx");
        let exactly = "a".repeat(255);
        assert_eq!(fit_apfs_component(&exactly), exactly);
    }

    #[test]
    fn apfs_long_cjk_name_keeps_extension_and_graphemes() {
        // Windows 来的 100 个汉字（300 字节）+ 扩展名
        let long = format!("{}.docx", "汉".repeat(100));
        let fitted = fit_apfs_component(&long);
        assert!(fitted.len() <= 255, "{}", fitted.len());
        assert!(fitted.ends_with(".docx"));
        let stem = fitted.strip_suffix(".docx").unwrap();
        let (kept, hash) = stem.split_at(stem.len() - 7);
        assert!(hash.starts_with('~') && hash[1..].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(kept.chars().all(|c| c == '汉'), "截断不能切到汉字中间");
        // 幂等且稳定
        assert_eq!(fit_apfs_component(&long), fitted);
        assert_eq!(fit_apfs_component(&fitted), fitted);
    }

    #[test]
    fn apfs_long_names_with_same_prefix_differ() {
        let a = format!("{}A.txt", "x".repeat(300));
        let b = format!("{}B.txt", "x".repeat(300));
        assert_ne!(fit_apfs_component(&a), fit_apfs_component(&b));
    }

    #[test]
    fn apfs_does_not_split_combined_graphemes() {
        // 带组合符的 NFD 字母（2 码位一簇）、emoji 家族（多码位一簇）
        let cluster = "e\u{0301}";
        let long = cluster.repeat(200);
        let fitted = fit_apfs_component(&long);
        assert!(fitted.len() <= 255);
        let kept = &fitted[..fitted.len() - 7];
        assert_eq!(kept.len() % cluster.len(), 0, "截断必须落在簇边界");
        let family = "👨‍👩‍👧‍👦";
        let long = family.repeat(20);
        let fitted = fit_apfs_component(&long);
        assert!(fitted.len() <= 255);
        assert_eq!(fitted[..fitted.len() - 7].len() % family.len(), 0);
    }

    #[test]
    fn apfs_overlong_extension_is_not_preserved() {
        let long = format!("{}.{}", "a".repeat(200), "e".repeat(100));
        let fitted = fit_apfs_component(&long);
        assert!(fitted.len() <= 255);
        assert!(fitted.starts_with("aaaa"));
        let hidden = format!(".{}", "h".repeat(300));
        assert!(fit_apfs_component(&hidden).len() <= 255);
    }

    #[test]
    fn case_collisions() {
        let paths: Vec<String> = ["README", "src/a.rs", "readme", "src/A.rs", "Readme/", "other", "src/b.rs", "src/b.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let groups = find_case_collisions(&paths);
        assert_eq!(groups, vec![vec![0, 2, 4], vec![1, 3]]);
        // 不同目录不算冲突；NFC/NFD 算冲突
        let paths: Vec<String> = vec!["a/x".into(), "b/X".into(), "caf\u{e9}".into(), "cafe\u{301}".into()];
        assert_eq!(find_case_collisions(&paths), vec![vec![2, 3]]);
        assert!(find_case_collisions(&[]).is_empty());
    }
}
