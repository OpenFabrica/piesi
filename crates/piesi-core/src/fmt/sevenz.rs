//! SevenZ 驱动（待实现）。

use std::path::Path;

use crate::archive::{ArchiveReader, Entry, EntryId, EntrySink, ExtractReport, Format};
use crate::progress::OpContext;
use crate::Result;

/// SevenZ 读取器。
pub struct SevenZReader {
    entries: Vec<Entry>,
}

impl SevenZReader {
    /// 打开并读取目录。
    pub fn open(_path: &Path) -> Result<Self> {
        Err(crate::Error::Unsupported("SevenZ：驱动尚未实现".into()))
    }
}

impl ArchiveReader for SevenZReader {
    fn format(&self) -> Format {
        Format::SevenZ
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn extract(&mut self, _ids: Option<&[EntryId]>, _sink: &mut dyn EntrySink, _ctx: &OpContext<'_>) -> Result<ExtractReport> {
        Err(crate::Error::Unsupported("SevenZ".into()))
    }

    fn read_entry(&mut self, _id: EntryId, _out: &mut dyn std::io::Write, _ctx: &OpContext<'_>) -> Result<()> {
        Err(crate::Error::Unsupported("SevenZ".into()))
    }
}
