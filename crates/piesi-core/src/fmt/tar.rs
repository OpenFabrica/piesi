//! tar 及单文件压缩流驱动（待实现）。

use std::path::Path;

use crate::archive::{ArchiveReader, Entry, EntryId, EntrySink, ExtractReport, Format};
use crate::progress::OpContext;
use crate::Result;

/// tar / gz / bz2 / xz / zst 读取器。
pub struct TarReader {
    format: Format,
    entries: Vec<Entry>,
}

impl TarReader {
    /// 打开；tar 系没有目录区，索引在后台边读边建。
    pub fn open(_path: &Path, format: Format) -> Result<Self> {
        Err(crate::Error::Unsupported(format!("{}：驱动尚未实现", format.display_name())))
    }
}

impl ArchiveReader for TarReader {
    fn format(&self) -> Format {
        self.format
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract(&mut self, _ids: Option<&[EntryId]>, _sink: &mut dyn EntrySink, _ctx: &OpContext<'_>) -> Result<ExtractReport> {
        Err(crate::Error::Unsupported("tar".into()))
    }

    fn read_entry(&mut self, _id: EntryId, _out: &mut dyn std::io::Write, _ctx: &OpContext<'_>) -> Result<()> {
        Err(crate::Error::Unsupported("tar".into()))
    }
}
