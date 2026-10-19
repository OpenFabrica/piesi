//! zip「Mac 专用」预设的集成测试：xattr / 资源分支 / Finder 信息写成 `__MACOSX/._名字` AppleDouble，
//! 用系统的 ditto、unzip 交叉验证，再用 piesi 自己回读确认这些条目按垃圾处理。只在 macOS 上跑。
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;

use piesi_core::create::{create_archive, CreateFormat, CreateOptions};
use piesi_core::names;
use piesi_core::progress::OpContext;

const K_NAME: &str = "com.example.k";
const TAGS_NAME: &str = "com.apple.metadata:_kMDItemUserTags";
const FINDER_INFO: &str = "com.apple.FinderInfo";
const RSRC: &str = "com.apple.ResourceFork";
const QUARANTINE: &str = "com.apple.quarantine";
/// Finder 标签「Red」的 bplist，Finder 写的就是这个
const TAGS_PLIST: &[u8] = b"bplist00\xa1\x01\x05Red\n6\x08\n\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x10";
const RSRC_DATA: &[u8] = b"RSRC-FORK-DATA-0123456789";

/// 每个测试独立的临时目录。
fn workdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("piesi-macmeta-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 32 字节 Finder 信息：type/creator `TEXT`/`ttxt`，flags 0x0400。
fn file_finder_info() -> [u8; 32] {
    let mut fi = [0u8; 32];
    fi[..8].copy_from_slice(b"TEXTttxt");
    fi[8] = 0x04;
    fi
}

/// 目录的 Finder 信息：只置 kHasCustomIcon。
fn dir_finder_info() -> [u8; 32] {
    let mut fi = [0u8; 32];
    fi[8] = 0x04;
    fi
}

/// 造一棵带各种 Mac 元数据的源目录：
/// `f.txt`（xattr、标签、资源分支、Finder 信息、quarantine）、`dir/`（Finder 信息）、`dir/n.txt`（xattr）、
/// `dir/plain.txt`（没设任何东西）、`emptydir/`、`link`（指向 f.txt 的符号链接）。
fn make_sources(src: &Path, with_quarantine: bool) {
    std::fs::create_dir_all(src.join("dir")).unwrap();
    std::fs::create_dir_all(src.join("emptydir")).unwrap();
    let f = src.join("f.txt");
    std::fs::write(&f, "hello\n").unwrap();
    xattr::set(&f, K_NAME, b"v").unwrap();
    xattr::set(&f, TAGS_NAME, TAGS_PLIST).unwrap();
    // 资源分支走 ..namedfork 路径写，确认是真的分支而不只是同名 xattr
    std::fs::write(src.join("f.txt/..namedfork/rsrc"), RSRC_DATA).unwrap();
    xattr::set(&f, FINDER_INFO, &file_finder_info()).unwrap();
    if with_quarantine {
        xattr::set(&f, QUARANTINE, b"0083;68f4c0d0;Safari;ABCDEF").unwrap();
    }
    xattr::set(src.join("dir"), FINDER_INFO, &dir_finder_info()).unwrap();
    std::fs::write(src.join("dir/n.txt"), "nested\n").unwrap();
    xattr::set(src.join("dir/n.txt"), "com.example.n", b"nv").unwrap();
    std::fs::write(src.join("dir/plain.txt"), "plain\n").unwrap();
    std::os::unix::fs::symlink("f.txt", src.join("link")).unwrap();
}

/// 用 piesi 打包（根是 f.txt、dir、emptydir、link 四个），返回包路径与 warning。
fn pack(work: &Path, src: &Path, mac_metadata: bool) -> (PathBuf, Vec<String>) {
    let out = work.join(if mac_metadata { "mac.zip" } else { "plain.zip" });
    let opts = CreateOptions { format: CreateFormat::Zip, level: 5, mac_metadata, ..Default::default() };
    let roots = ["f.txt", "dir", "emptydir", "link"].map(|n| src.join(n));
    let report = create_archive(&out, &roots, &opts, &OpContext::silent()).unwrap();
    (out, report.warnings)
}

/// 包里的全部条目名（zip 目录条目带尾部 `/`，这里统一去掉）。
fn entry_names(archive: &Path) -> Vec<String> {
    let reader = piesi_core::open(archive).unwrap();
    reader.entries().iter().map(|e| String::from_utf8(e.raw_name.clone()).unwrap().trim_end_matches('/').to_owned()).collect()
}

/// 跑一个外部命令，失败就带着输出 panic。
fn run(cmd: &str, args: &[&str], cwd: &Path) -> String {
    let o = Command::new(cmd).args(args).current_dir(cwd).output().unwrap();
    assert!(o.status.success(), "{cmd} {args:?} 失败：\n{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// `unzip -p` 取一个条目的原始字节。
fn unzip_entry(archive: &Path, name: &str) -> Vec<u8> {
    let o = Command::new("unzip").args(["-p", archive.to_str().unwrap(), name]).output().unwrap();
    assert!(o.status.success(), "unzip -p {name} 失败：{}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

/// 大端 u32。
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

/// 全流程：打包 → unzip 列目录/校验 → ditto -x -k 还原并逐项核对 → piesi 回读时 `__MACOSX` 全是垃圾。
#[test]
fn ditto_restores_xattr_rsrc_and_finder_info() {
    let work = workdir("roundtrip");
    let src = work.join("src");
    make_sources(&src, true);
    let (zip, warnings) = pack(&work, &src, true);
    assert!(warnings.is_empty(), "不该有 warning：{warnings:?}");

    // 条目布局：`._` 紧跟在自己的条目后面（中间最多隔几条第一次出现的 __MACOSX/…/ 目录条目），
    // 嵌套的放在 __MACOSX/dir/ 下，目录也有；符号链接没有
    let names = entry_names(&zip);
    let pos = |n: &str| names.iter().position(|x| x == n).unwrap_or_else(|| panic!("缺少条目 {n}：{names:?}"));
    let follows = |entry: &str, ad: &str| {
        let (a, b) = (pos(entry), pos(ad));
        assert!(a < b && names[a + 1..b].iter().all(|n| n.starts_with("__MACOSX") && !n.contains("/._")), "{ad} 应紧跟 {entry}：{names:?}");
    };
    follows("f.txt", "__MACOSX/._f.txt");
    follows("dir/n.txt", "__MACOSX/dir/._n.txt");
    follows("dir", "__MACOSX/._dir");
    assert!(names.contains(&"__MACOSX".to_owned()) && names.contains(&"__MACOSX/dir".to_owned()), "应有 __MACOSX 目录条目：{names:?}");
    assert!(!names.iter().any(|n| n == "__MACOSX/._link"), "符号链接不处理：{names:?}");
    // 「没有元数据就不写 `._`」：系统会自动给新文件和目录打 com.apple.provenance，所以 plain.txt、emptydir
    // 有没有 `._` 不能写死，按读出来的元数据是否为空来对账
    for rel in ["f.txt", "dir", "dir/n.txt", "dir/plain.txt", "emptydir"] {
        let mut w = Vec::new();
        let has_meta = !piesi_core::create::apple_double::read(&src.join(rel), &mut w).unwrap().is_empty();
        let has_ad = names.contains(&piesi_core::create::apple_double::apple_double_rel(rel));
        assert_eq!(has_ad, has_meta, "{rel}：有元数据 {has_meta}，有 `._` 条目 {has_ad}：{names:?}");
    }
    for n in names.iter().filter(|n| n.starts_with("__MACOSX")) {
        assert!(names::is_junk_path(n), "piesi 自己打开时 {n} 应按垃圾处理");
    }
    assert!(names.iter().filter(|n| !n.starts_with("__MACOSX")).all(|n| !names::is_junk_path(n)));

    // unzip 能列出、能校验
    let listing = run("unzip", &["-l", zip.to_str().unwrap()], &work);
    assert!(listing.contains("__MACOSX/._f.txt"), "unzip -l 应列出 __MACOSX/._f.txt：\n{listing}");
    assert!(listing.contains("__MACOSX/dir/._n.txt"), "{listing}");
    run("unzip", &["-tqq", zip.to_str().unwrap()], &work);

    // 我们写的 AppleDouble：头、条目表、ATTR 块的关键字段；quarantine 不在里面，Finder 信息和资源分支在
    let ad = unzip_entry(&zip, "__MACOSX/._f.txt");
    assert_eq!(be32(&ad, 0), 0x0005_1607);
    assert_eq!(be32(&ad, 4), 0x0002_0000);
    assert_eq!(&ad[8..24], b"Mac OS X        ");
    let total_size = be32(&ad, 92) as usize;
    assert_eq!((be32(&ad, 26), be32(&ad, 30), be32(&ad, 34) as usize), (9, 50, total_size - 50));
    assert_eq!((be32(&ad, 38), be32(&ad, 42) as usize, be32(&ad, 46) as usize), (2, total_size, RSRC_DATA.len()));
    assert_eq!(&ad[50..82], &file_finder_info());
    assert_eq!(&ad[84..88], b"ATTR");
    assert_eq!(&ad[total_size..], RSRC_DATA);
    let text = String::from_utf8_lossy(&ad);
    assert!(text.contains(K_NAME) && text.contains(TAGS_NAME), "xattr 名应在条目表里");
    assert!(!text.contains(QUARANTINE), "quarantine 不该进包");

    // ditto 解包后元数据全部回来
    let dest = work.join("ditto-out");
    std::fs::create_dir_all(&dest).unwrap();
    run("ditto", &["-x", "-k", zip.to_str().unwrap(), dest.to_str().unwrap()], &work);
    let f = dest.join("f.txt");
    assert_eq!(std::fs::read(&f).unwrap(), b"hello\n");
    assert_eq!(xattr::get(&f, K_NAME).unwrap().as_deref(), Some(&b"v"[..]), "xattr 没回来");
    assert_eq!(xattr::get(&f, TAGS_NAME).unwrap().as_deref(), Some(TAGS_PLIST), "Finder 标签没回来");
    assert_eq!(xattr::get(&f, RSRC).unwrap().as_deref(), Some(RSRC_DATA), "资源分支没回来");
    assert_eq!(std::fs::read(dest.join("f.txt/..namedfork/rsrc")).unwrap(), RSRC_DATA);
    assert_eq!(xattr::get(&f, FINDER_INFO).unwrap().as_deref(), Some(&file_finder_info()[..]), "Finder 信息没回来");
    assert_eq!(xattr::get(&f, QUARANTINE).unwrap(), None, "quarantine 不该被带过来");
    assert_eq!(xattr::get(dest.join("dir"), FINDER_INFO).unwrap().as_deref(), Some(&dir_finder_info()[..]), "目录的 Finder 信息没回来");
    assert_eq!(xattr::get(dest.join("dir/n.txt"), "com.example.n").unwrap().as_deref(), Some(&b"nv"[..]));
    assert_eq!(std::fs::read_link(dest.join("link")).unwrap(), Path::new("f.txt"));
    assert!(!dest.join("__MACOSX").exists(), "ditto 应把 __MACOSX 吃掉而不是解成目录");

    // unzip 也能正常解出（它不懂 AppleDouble，`._` 会原样落地）
    let dest2 = work.join("unzip-out");
    run("unzip", &["-q", zip.to_str().unwrap(), "-d", dest2.to_str().unwrap()], &work);
    assert_eq!(std::fs::read(dest2.join("__MACOSX/._f.txt")).unwrap(), ad);
    std::fs::remove_dir_all(&work).unwrap();
}

/// 不带 quarantine 的同一个文件，我们的 `._` 应与 ditto 写的逐字节相同。
#[test]
fn apple_double_is_byte_identical_to_ditto() {
    let work = workdir("ditto-cmp");
    let src = work.join("src");
    make_sources(&src, false);
    let (zip, _) = pack(&work, &src, true);
    let ditto_zip = work.join("ditto.zip");
    run("ditto", &["-c", "-k", "--sequesterRsrc", src.to_str().unwrap(), ditto_zip.to_str().unwrap()], &work);
    for name in ["__MACOSX/._f.txt", "__MACOSX/._dir", "__MACOSX/dir/._n.txt"] {
        let ours = unzip_entry(&zip, name);
        let theirs = unzip_entry(&ditto_zip, name);
        assert_eq!(ours, theirs, "{name} 与 ditto 的字节不同");
    }
    std::fs::remove_dir_all(&work).unwrap();
}

/// 不开选项：包里没有任何 `__MACOSX`。
#[test]
fn no_macosx_without_option() {
    let work = workdir("off");
    let src = work.join("src");
    make_sources(&src, true);
    let (zip, warnings) = pack(&work, &src, false);
    assert!(warnings.is_empty());
    let names = entry_names(&zip);
    assert!(!names.iter().any(|n| n.contains("__MACOSX") || n.contains("/._")), "{names:?}");
    std::fs::remove_dir_all(&work).unwrap();
}

/// 资源分支超过 64 MB：跳过它、记一条 warning，其它元数据照写。
#[test]
fn oversize_fork_is_skipped_with_warning() {
    let work = workdir("big");
    let src = work.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let f = src.join("huge.bin");
    std::fs::write(&f, "x").unwrap();
    xattr::set(&f, K_NAME, b"v").unwrap();
    xattr::set(&f, RSRC, &vec![0u8; 64 * 1024 * 1024 + 1]).unwrap();
    let out = work.join("big.zip");
    let opts = CreateOptions { format: CreateFormat::Zip, level: 1, mac_metadata: true, ..Default::default() };
    let report = create_archive(&out, std::slice::from_ref(&f), &opts, &OpContext::silent()).unwrap();
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("资源分支") && report.warnings[0].contains("64 MB"), "{}", report.warnings[0]);
    let ad = unzip_entry(&out, "__MACOSX/._huge.bin");
    assert_eq!(be32(&ad, 46), 0, "资源分支长度应为 0");
    assert!(String::from_utf8_lossy(&ad).contains(K_NAME));
    std::fs::remove_dir_all(&work).unwrap();
}
