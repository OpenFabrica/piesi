//! 单文件 gz / bz2 / xz / zst 写入，不套 TAR 容器。
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use super::{compression, CreateOptions, Sources};
use crate::progress::{OpContext, Throttle};
use crate::{Error, Result};

/// 写入单个普通文件；调用方已检查根路径数量和类型，这里再检查过滤后的条目。
pub(super) fn write_stream(dest: &Path, sources: &Sources, opts: &CreateOptions, ctx: &OpContext<'_>) -> Result<()> {
    let [item] = sources.items.as_slice() else {
        return Err(Error::Unsupported(crate::DetailKind::CreateSingleFileRequired.into()));
    };
    let mut input = File::open(&item.path)?;
    let mut out = compression::encoder(dest, opts, item.size)?;
    let mut throttle = Throttle::new(ctx, Some(item.size), Some(1));
    throttle.begin_entry(&item.rel)?;
    let mut buf = vec![0; 256 * 1024];
    loop {
        ctx.cancel.check()?;
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        throttle.add_bytes(n as u64)?;
    }
    out.finish_boxed()?;
    throttle.finish();
    Ok(())
}
