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
        mac_metadata: false,
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
        merge_into_existing: false,
    };
    let plan = archive.plan(extract_req.clone()).unwrap();
    assert!(Path::new(&plan.root).starts_with(&out), "根目录应在目标下：{}", plan.root);
    assert_eq!(plan.total_bytes, 10_005);
    assert!(plan.conflicts.is_empty());
    assert_eq!(plan.existing_folder, None);
    assert!(plan.archive_bytes > 0);
    assert!(plan.entries >= 3, "至少 源/、中文.txt、子目录/、b.bin 中的 3 项，实际 {}", plan.entries);

    // ids_under：目录下全部条目（含目录自身），整包 = 全部
    let under = archive.ids_under("源/子目录".into());
    assert!(!under.is_empty() && under.len() < archive.entry_count() as usize, "{under:?}");
    assert_eq!(archive.ids_under(String::new()).len(), archive.entry_count() as usize);

    let result = archive.extract(extract_req.clone(), Arc::new(Quiet), CancelHandle::new()).unwrap();
    assert_eq!(result.root, plan.root);
    assert_eq!(result.top_level.len(), 1, "只有一个顶层项目：{:?}", result.top_level);
    assert!(Path::new(&result.top_level[0]).exists());
    assert_eq!(find_file(&out, "中文.txt").unwrap(), b"hello");
    assert_eq!(find_file(&out, "b.bin").unwrap().len(), 10_000);
    assert!(find_file(&out, ".DS_Store").is_none());

    // 再解一次：顶层文件夹已存在 → 报出来，默认另建「名字-2」
    let again = archive.plan(extract_req.clone()).unwrap();
    let existing = again.existing_folder.clone().expect("顶层文件夹已存在");
    assert_eq!(again.new_folder, Some(format!("{existing}-2")));
    assert!(again.conflicts.is_empty());
    let merged = archive.plan(ExtractRequest { merge_into_existing: true, ..extract_req.clone() }).unwrap();
    assert!(!merged.conflicts.is_empty(), "解进已有文件夹会有同名文件");

    // 空 dest = 压缩包所在目录
    let beside = archive.plan(ExtractRequest { dest: String::new(), smart_folder: true, conflict: Conflict::Skip, skip_junk: true, ids: vec![], merge_into_existing: false }).unwrap();
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
        mac_metadata: false,
    };
    let err = create_archive(req, Arc::new(Quiet), cancel).unwrap_err();
    assert!(matches!(err, piesi_ffi::PiesiError::Cancelled), "{err:?}");
    assert!(!dest.exists(), "取消后不应留下半个文件");
    fs::remove_dir_all(&dir).unwrap();
}


#[test]
fn verify_detects_corruption() {
    let dir = scratch("verify");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    // 仅存储（level 0），数据原样进包，方便定位后翻转一个字节
    let payload: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(src.join("data.bin"), &payload).unwrap();
    let dest = dir.join("out.zip");
    let req = CreateRequest {
        dest: dest.to_string_lossy().into_owned(),
        sources: vec![src.to_string_lossy().into_owned()],
        format: CreateFormat::Zip,
        level: 0,
        password: None,
        encrypt_names: false,
        skip_junk: true,
        windows_safe: false,
        mac_metadata: false,
    };
    create_archive(req, Arc::new(Quiet), CancelHandle::new()).unwrap();

    let archive = open_archive(dest.to_string_lossy().into_owned(), None).unwrap();
    let ok = archive.verify(vec![], Arc::new(Quiet), CancelHandle::new()).unwrap();
    assert!(ok.entries >= 1);
    drop(archive);

    let mut bytes = fs::read(&dest).unwrap();
    let pos = bytes.windows(payload.len()).position(|w| w == payload.as_slice()).expect("存储模式下应能找到原始数据");
    bytes[pos + 10_000] ^= 0xFF;
    fs::write(&dest, &bytes).unwrap();
    let archive = open_archive(dest.to_string_lossy().into_owned(), None).unwrap();
    let err = archive.verify(vec![], Arc::new(Quiet), CancelHandle::new()).unwrap_err();
    // CRC 对不上归为「压缩包损坏」，细节是结构化的 CrcMismatch（条目名、期望值、实际值），壳层据此拼本地化句子
    match &err {
        piesi_ffi::PiesiError::Corrupt { detail } => {
            assert_eq!(detail.kind, piesi_ffi::DetailKind::CrcMismatch, "{err:?}");
            assert_eq!(detail.args.len(), 3, "{err:?}");
        }
        other => panic!("损坏应报 Corrupt：{other:?}"),
    }
    fs::remove_dir_all(&dir).unwrap();
}

/// 不安全路径的原因是结构化的（壳层用它翻译 UnsafePathSkipped 警告）；安全的名字返回 None。
#[test]
fn unsafe_path_reason_is_structured() {
    let d = piesi_ffi::unsafe_path_reason("../etc/passwd".into()).expect("`..` 应判为不安全");
    assert_eq!(d.kind, piesi_ffi::DetailKind::PathDotComponent);
    assert_eq!(d.args, vec!["..".to_owned(), "../etc/passwd".to_owned()]);
    assert_eq!(d.to_string(), "路径含 `..` 分量：../etc/passwd");
    assert_eq!(piesi_ffi::unsafe_path_reason("/abs".into()).map(|d| d.kind), Some(piesi_ffi::DetailKind::PathAbsolute));
    assert!(piesi_ffi::unsafe_path_reason("a/b.txt".into()).is_none());
}
