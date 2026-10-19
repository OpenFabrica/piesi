//! 格式识别：先看魔数，再看扩展名兜底。tar 没有可靠魔数（ustar 标记在 257 字节处），放最后。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::archive::Format;
use crate::Result;

/// 识别文件格式。读头 512 字节即可覆盖所有魔数，再读 257 字节处的 ustar 标记。
pub fn detect_format(path: &Path) -> Result<Option<Format>> {
    let mut f = File::open(path)?;
    let mut head = [0u8; 512];
    let n = f.read(&mut head)?;
    let head = &head[..n];

    if let Some(fmt) = detect_by_magic(head) {
        return Ok(Some(fmt));
    }
    // ustar 标记：偏移 257 处 "ustar"；老式 tar 没有标记，只能靠扩展名
    if n >= 262 && &head[257..262] == b"ustar" {
        return Ok(Some(Format::Tar));
    }
    // 空文件或者太短：按扩展名猜
    f.seek(SeekFrom::Start(0))?;
    Ok(detect_by_extension(path))
}

/// 用文件头魔数识别；压缩流外壳再看扩展名决定是不是 tar。
pub fn detect_by_magic(head: &[u8]) -> Option<Format> {
    if head.len() < 4 {
        return None;
    }
    // zip：PK\x03\x04（普通）、PK\x05\x06（空包）、PK\x07\x08（跨卷首段）
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08") {
        return Some(Format::Zip);
    }
    if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return Some(Format::SevenZ);
    }
    if head.starts_with(b"Rar!\x1A\x07") {
        return Some(Format::Rar);
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return Some(Format::Gz);
    }
    if head.starts_with(b"BZh") {
        return Some(Format::Bz2);
    }
    if head.starts_with(b"\xFD7zXZ\x00") {
        return Some(Format::Xz);
    }
    if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(Format::Zst);
    }
    None
}

/// 只按扩展名猜，魔数失败时兜底；同时用来把 gz/bz2/xz/zst 外壳升级成 tar.*。
pub fn detect_by_extension(path: &Path) -> Option<Format> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let ends = |s: &str| name.ends_with(s);
    if ends(".tar.gz") || ends(".tgz") {
        Some(Format::TarGz)
    } else if ends(".tar.bz2") || ends(".tbz") || ends(".tbz2") {
        Some(Format::TarBz2)
    } else if ends(".tar.xz") || ends(".txz") {
        Some(Format::TarXz)
    } else if ends(".tar.zst") || ends(".tzst") {
        Some(Format::TarZst)
    } else if ends(".tar") {
        Some(Format::Tar)
    } else if ends(".zip") || ends(".jar") || ends(".apk") || ends(".ipa") || ends(".zipx") || ends(".zip.001") {
        Some(Format::Zip)
    } else if ends(".7z") || ends(".7z.001") {
        Some(Format::SevenZ)
    } else if ends(".rar") {
        Some(Format::Rar)
    } else if ends(".gz") {
        Some(Format::Gz)
    } else if ends(".bz2") {
        Some(Format::Bz2)
    } else if ends(".xz") {
        Some(Format::Xz)
    } else if ends(".zst") {
        Some(Format::Zst)
    } else {
        None
    }
}

/// 压缩流外壳 + 文件名决定最终格式：`.tar.gz` 的魔数是 gz，但要按 tar.gz 处理。
pub fn refine_stream_format(magic: Format, path: &Path) -> Format {
    match (magic, detect_by_extension(path)) {
        (Format::Gz, Some(Format::TarGz)) => Format::TarGz,
        (Format::Bz2, Some(Format::TarBz2)) => Format::TarBz2,
        (Format::Xz, Some(Format::TarXz)) => Format::TarXz,
        (Format::Zst, Some(Format::TarZst)) => Format::TarZst,
        (m, _) => m,
    }
}
