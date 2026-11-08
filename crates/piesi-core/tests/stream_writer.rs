//! 新增创建格式的回读、目录/链接保留和无效输入测试。
mod drivers_common;
use drivers_common::MemSink;
use piesi_core::create::{create_archive, CreateFormat, CreateOptions};
use piesi_core::progress::OpContext;

/// 测试目录独立，不和其他集成测试争用文件。
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("piesi-stream-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 六种新增格式都能由自己的读取器校验并还原原始内容。
#[test]
fn new_formats_roundtrip() {
    let dir = scratch("roundtrip");
    let src = dir.join("中文.txt");
    let content = "测试压缩流\nhello world\n".repeat(2000).into_bytes();
    std::fs::write(&src, &content).unwrap();
    for format in [CreateFormat::Tar, CreateFormat::TarBz2, CreateFormat::Gz, CreateFormat::Bz2, CreateFormat::Xz, CreateFormat::Zst] {
        let dest = dir.join(format!("中文.txt.{}", format.extension()));
        let report =
            create_archive(&dest, std::slice::from_ref(&src), &CreateOptions { format, level: 1, ..Default::default() }, &OpContext::silent()).unwrap();
        assert_eq!(report.input_bytes, content.len() as u64);
        let mut reader = piesi_core::open(&dest).unwrap();
        let mut sink = MemSink::default();
        reader.extract(None, &mut sink, &OpContext::silent()).unwrap();
        assert_eq!(sink.files.len(), 1, "{format:?}");
        assert_eq!(sink.files.values().next().unwrap(), &content, "{format:?}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// TAR 与 tar.bz2 能保留目录、空文件和符号链接，普通归档规则一致。
#[test]
fn tar_directory_roundtrip() {
    let dir = scratch("directory");
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("empty")).unwrap();
    std::fs::write(src.join("a.txt"), b"hello").unwrap();
    std::fs::write(src.join("zero"), []).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", src.join("link")).unwrap();
    for format in [CreateFormat::Tar, CreateFormat::TarBz2] {
        let dest = dir.join(format!("archive.{}", format.extension()));
        create_archive(&dest, std::slice::from_ref(&src), &CreateOptions { format, ..Default::default() }, &OpContext::silent()).unwrap();
        let mut reader = piesi_core::open(&dest).unwrap();
        let mut sink = MemSink::default();
        reader.extract(None, &mut sink, &OpContext::silent()).unwrap();
        assert_eq!(sink.files["src/a.txt"], b"hello");
        assert!(sink.files["src/zero"].is_empty());
        assert!(sink.dirs.contains(&"src/empty".into()));
        #[cfg(unix)]
        assert!(sink.links.contains(&("src/link".into(), b"a.txt".to_vec())));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// 不支持的密码和单文件流的目录/多源输入应在创建输出前拒绝。
#[test]
fn invalid_stream_requests_do_not_create_output() {
    let dir = scratch("invalid");
    let src = dir.join("a.txt");
    std::fs::write(&src, []).unwrap();
    for format in [CreateFormat::Gz, CreateFormat::Bz2, CreateFormat::Xz, CreateFormat::Zst] {
        let dest = dir.join(format!("bad.{}", format.extension()));
        let opts = CreateOptions { format, ..Default::default() };
        for sources in [vec![], vec![dir.clone()], vec![src.clone(), src.clone()]] {
            assert!(create_archive(&dest, &sources, &opts, &OpContext::silent()).is_err());
            assert!(!dest.exists());
        }
    }
    for format in [
        CreateFormat::Tar,
        CreateFormat::TarGz,
        CreateFormat::TarBz2,
        CreateFormat::TarXz,
        CreateFormat::TarZst,
        CreateFormat::Gz,
        CreateFormat::Bz2,
        CreateFormat::Xz,
        CreateFormat::Zst,
    ] {
        let dest = dir.join(format!("bad.{}", format.extension()));
        let opts = CreateOptions { format, password: Some("secret".into()), ..Default::default() };
        assert!(create_archive(&dest, std::slice::from_ref(&src), &opts, &OpContext::silent()).is_err());
        assert!(!dest.exists());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// 超过一个编码块的 GZip / BZip2 多成员流与空输入都必须完整回读。
#[test]
fn parallel_stream_and_empty_roundtrip() {
    let dir = scratch("parallel");
    let src = dir.join("content.bin");
    for content in [Vec::new(), b"parallel stream\n".repeat(800_000)] {
        std::fs::write(&src, &content).unwrap();
        for format in [CreateFormat::Gz, CreateFormat::Bz2, CreateFormat::TarGz, CreateFormat::TarBz2] {
            let out = dir.join(format!("test.{}", format.extension()));
            create_archive(&out, std::slice::from_ref(&src), &CreateOptions { format, threads: 3, level: 1, ..Default::default() }, &OpContext::silent()).unwrap();
            let mut reader = piesi_core::open(&out).unwrap();
            let mut sink = MemSink::default();
            reader.extract(None, &mut sink, &OpContext::silent()).unwrap();
            assert_eq!(sink.files.values().next().unwrap(), &content, "{format:?}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
