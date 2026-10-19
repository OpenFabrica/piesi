//! 格式驱动注册处：按 `Format` 分发到各驱动。

pub mod detect;
pub mod rar;
pub mod sevenz;
pub mod tar;
pub mod zip;

use std::path::Path;

use crate::archive::{ArchiveReader, Format};
use crate::Result;

/// 按已识别的格式打开对应驱动。
pub fn open_with_format(path: &Path, format: Format) -> Result<Box<dyn ArchiveReader>> {
    match format {
        Format::Zip => Ok(Box::new(zip::ZipReader::open(path)?)),
        Format::SevenZ => Ok(Box::new(sevenz::SevenZReader::open(path)?)),
        Format::Rar => Ok(Box::new(rar::RarReader::open(path)?)),
        f if f.is_tar() || f.is_single_stream() => Ok(Box::new(tar::TarReader::open(path, f)?)),
        f => Err(crate::Error::Unsupported(f.display_name().to_owned())),
    }
}
