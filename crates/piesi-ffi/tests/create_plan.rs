//! FFI 层端到端：用导出的 create_archive 压一个目录，再 open_archive → plan → extract 回读。
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use piesi_ffi::{
    create_archive, format_extension, open_archive, CancelHandle, Conflict, CreateFormat, CreateRequest, ExtractRequest,
    ProgressInfo, ProgressListener,
};

/// 什么都不做的进度监听，只为满足签名。
struct Quiet;

impl ProgressListener for Quiet {
    fn on_progress(&self, _info: ProgressInfo) {}
}

/// 每个测试独立的临时目录。
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("piesi-ffi-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 在 root 下递归找名字相同的文件，返回内容。
fn find_file(root: &Path, name: &str) -> Option<Vec<u8>> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(v) = find_file(&path, name) {
                return Some(v);
            }
        } else if path.file_name().map(|n| n == name).unwrap_or(false) {
            return fs::read(&path).ok();
        }
    }
    None
}

#[test]
fn create_then_plan_and_extract_zip() {
    let dir = scratch("zip");
    let src = dir.join("源");
    fs::create_dir_all(src.join("子目录")).unwrap();
    fs::write(src.join("中文.txt"), b"hello").unwrap();
    fs::write(src.join("子目录/b.bin"), vec![7u8; 10_000]).unwrap();
    fs::write(src.join(".DS_Store"), b"junk").unwrap();

    let dest = dir.join(format!("out.{}", format_extension(CreateFormat::Zip)));
    let req = CreateRequest {
        dest: dest.to_string_lossy().into_owned(),
        sources: vec![src.to_string_lossy().into_owned()],
        format: CreateFormat::Zip,
        level: 5,
        password: None,
        encrypt_names: false,
        skip_junk: true,
        windows_safe: false,
    };
    let report = create_archive(req, Arc::new(Quiet), CancelHandle::new()).unwrap();
    assert_eq!(report.skipped_junk, 1, "应跳过 .DS_Store");
    assert!(report.output_bytes > 0);

    let archive = open_archive(dest.to_string_lossy().into_owned(), Some("zh-CN".into())).unwrap();
    // dest 为空 = 解到压缩包旁边；解压目标是 dir，和源目录 源/ 分开放
    let out = dir.join("解压");
    fs::create_dir_all(&out).unwrap();
    let extract_req = ExtractRequest {
        dest: out.to_string_lossy().into_owned(),
        smart_folder: true,
        conflict: Conflict::Rename,
        skip_junk: true,
        ids: vec![],
    };
    let plan = archive.plan(extract_req.clone()).unwrap();
    assert!(Path::new(&plan.root).starts_with(&out), "根目录应在目标下：{}", plan.root);
    assert_eq!(plan.total_bytes, 10_005);
    assert!(plan.conflicts.is_empty());
    assert!(plan.archive_bytes > 0);
    assert!(plan.entries >= 3, "至少 源/、中文.txt、子目录/、b.bin 中的 3 项，实际 {}", plan.entries);

    let result = archive.extract(extract_req, Arc::new(Quiet), CancelHandle::new()).unwrap();
    assert_eq!(result.root, plan.root);
    assert_eq!(find_file(&out, "中文.txt").unwrap(), b"hello");
    assert_eq!(find_file(&out, "b.bin").unwrap().len(), 10_000);
    assert!(find_file(&out, ".DS_Store").is_none());

    // 空 dest = 压缩包所在目录
    let beside = archive.plan(ExtractRequest { dest: String::new(), smart_folder: true, conflict: Conflict::Skip, skip_junk: true, ids: vec![] }).unwrap();
    assert!(Path::new(&beside.root).starts_with(&dir));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn create_cancelled_leaves_no_file() {
    let dir = scratch("cancel");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.bin"), vec![1u8; 200_000]).unwrap();
    let dest = dir.join("out.7z");
    let cancel = CancelHandle::new();
    cancel.cancel();
    let req = CreateRequest {
        dest: dest.to_string_lossy().into_owned(),
        sources: vec![src.to_string_lossy().into_owned()],
        format: CreateFormat::SevenZ,
        level: 1,
        password: None,
        encrypt_names: false,
        skip_junk: true,
        windows_safe: false,
    };
    let err = create_archive(req, Arc::new(Quiet), cancel).unwrap_err();
    assert!(matches!(err, piesi_ffi::PiesiError::Cancelled), "{err:?}");
    assert!(!dest.exists(), "取消后不应留下半个文件");
    fs::remove_dir_all(&dir).unwrap();
}
