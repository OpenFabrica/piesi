//! 文件名规则的公开接口测试：把解码后的名字依次过 sanitize → junk 过滤 → Windows 安全名 → APFS 截断 → 大小写冲突。

use piesi_core::error::Error;
use piesi_core::names::{find_case_collisions, fit_apfs_component, is_junk_path, sanitize_rel_path, windows_safe_name, NameWarning, APFS_COMPONENT_MAX_BYTES};

#[test]
fn zip_slip_samples_are_rejected() {
    let samples = [
        "../../etc/passwd",
        "..\\..\\Windows\\System32\\x.dll",
        "/etc/passwd",
        "C:\\Windows\\win.ini",
        "\\\\?\\C:\\x",
        "\\\\server\\share\\x",
        "a/../../b",
        "a\u{0}b",
    ];
    for s in samples {
        assert!(matches!(sanitize_rel_path(s), Err(Error::UnsafePath(_))), "{s:?} 应被拒绝");
    }
}

#[test]
fn typical_archive_paths_survive_sanitize() {
    for (input, expected) in [
        ("文档/报告.docx", "文档/报告.docx"),
        ("./照片/IMG_0001.jpg", "照片/IMG_0001.jpg"),
        ("合同\\采购合同（最终版）.pdf", "合同/采购合同（最终版）.pdf"),
        ("dir//sub/", "dir/sub"),
        ("name with spaces.txt", "name with spaces.txt"),
        ("..hidden", "..hidden"),
    ] {
        assert_eq!(sanitize_rel_path(input).unwrap(), expected);
    }
}

#[test]
fn root_dir_entries_are_reported_as_unsafe_so_caller_skips_them() {
    for s in ["./", ".", ""] {
        assert!(matches!(sanitize_rel_path(s), Err(Error::UnsafePath(_))), "{s:?}");
    }
}

#[test]
fn junk_filter_matches_finder_and_windows_leftovers() {
    assert!(is_junk_path("__MACOSX/项目/._报告.docx"));
    assert!(is_junk_path("项目/.DS_Store"));
    assert!(is_junk_path("项目/._报告.docx"));
    assert!(is_junk_path("Thumbs.db"));
    assert!(is_junk_path("照片/desktop.ini"));
    assert!(!is_junk_path("项目/报告.docx"));
    assert!(!is_junk_path("_notes.txt"));
}

#[test]
fn windows_preset_pipeline() {
    // 「发给 Windows」预设：替换非法字符、去尾点、保留名
    let (name, warns) = windows_safe_name("报告: 2024/Q1?.docx");
    assert_eq!(name, "报告： 2024／Q1？.docx");
    assert_eq!(warns, vec![NameWarning::IllegalCharsReplaced(3)]);
    let (name, warns) = windows_safe_name("aux.txt");
    assert_eq!(name, "aux_.txt");
    assert_eq!(warns, vec![NameWarning::ReservedName]);
    let (name, warns) = windows_safe_name("notes...");
    assert_eq!(name, "notes");
    assert_eq!(warns, vec![NameWarning::TrailingDotsOrSpacesTrimmed]);
    let (name, warns) = windows_safe_name("正常名字.txt");
    assert_eq!(name, "正常名字.txt");
    assert!(warns.is_empty());
}

#[test]
fn apfs_limit_is_bytes_not_chars() {
    // 86 个汉字 = 258 字节，刚好超过 255
    let name = "汉".repeat(86);
    assert_eq!(name.len(), 258);
    let fitted = fit_apfs_component(&name);
    assert!(fitted.len() <= APFS_COMPONENT_MAX_BYTES);
    assert!(fitted.contains('~'));
    // 85 个汉字 = 255 字节，不动
    let name = "汉".repeat(85);
    assert_eq!(fit_apfs_component(&name), name);
}

#[test]
fn case_collisions_detected_before_extraction() {
    let paths: Vec<String> = ["README.md", "readme.md", "src/Main.rs", "src/main.rs", "docs/a.txt"].iter().map(|s| s.to_string()).collect();
    assert_eq!(find_case_collisions(&paths), vec![vec![0, 1], vec![2, 3]]);
}
