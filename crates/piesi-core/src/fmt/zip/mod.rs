//! ZIP 读取器（自写容器解析；编解码用 crates）。待实现。

use std::path::Path;

use crate::archive::{ArchiveReader, Entry, EntryId, EntrySink, ExtractReport, Format};
use crate::progress::OpContext;
use crate::Result;

/// ZIP 读取器。
pub struct ZipReader {
    entries: Vec<Entry>,
}

impl ZipReader {
    /// 打开并解析中央目录。
    pub fn open(_path: &Path) -> Result<Self> {
        Err(crate::Error::Unsupported("ZIP：驱动尚未实现".into()))
    }
}

impl ArchiveReader for ZipReader {
    fn format(&self) -> Format {
        Format::Zip
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract(&mut self, _ids: Option<&[EntryId]>, _sink: &mut dyn EntrySink, _ctx: &OpContext<'_>) -> Result<ExtractReport> {
        Err(crate::Error::Unsupported("zip".into()))
    }

    fn read_entry(&mut self, _id: EntryId, _out: &mut dyn std::io::Write, _ctx: &OpContext<'_>) -> Result<()> {
        Err(crate::Error::Unsupported("zip".into()))
    }
}
