//! 压缩侧：收集源文件、过滤垃圾、归一化名字，再按格式交给写入器。

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::archive::EntryKind;
use crate::encoding::normalize_for_write;
use crate::names;
use crate::progress::OpContext;
use crate::warning::{Warning, WarningKind};
use crate::{Error, Result};

/// 可创建的格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CreateFormat {
    Zip,
    SevenZ,
    TarGz,
    TarXz,
    TarZst,
}

impl CreateFormat {
    /// 默认扩展名。
    pub fn extension(self) -> &'static str {
        match self {
            CreateFormat::Zip => "zip",
            CreateFormat::SevenZ => "7z",
            CreateFormat::TarGz => "tar.gz",
            CreateFormat::TarXz => "tar.xz",
            CreateFormat::TarZst => "tar.zst",
        }
    }
}

/// 压缩选项。
#[derive(Clone, Debug)]
pub struct CreateOptions {
    pub format: CreateFormat,
    /// 0 = 仅存储，1 最快 … 9 最高；各格式自己映射
    pub level: u8,
    pub password: Option<String>,
    /// 7z：连文件名一起加密
    pub encrypt_names: bool,
    /// 跳过 `.DS_Store`、`__MACOSX` 等垃圾（默认开）
    pub skip_junk: bool,
    /// 把文件名改成 Windows 安全名（「发给 Windows」预设）
    pub windows_safe: bool,
    /// 「Mac 专用」预设：把 Finder 信息、扩展属性、资源分支写成 `__MACOSX/._名字` AppleDouble 条目
    /// （与 ditto / 归档工具兼容）。只对 zip 生效，其它格式暂时忽略
    pub mac_metadata: bool,
    /// 线程数，0 = CPU 核数
    pub threads: usize,
    /// 7z solid 块大小上限（字节），0 = 非 solid
    pub solid_block_bytes: u64,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            format: CreateFormat::Zip,
            level: 5,
            password: None,
            encrypt_names: false,
            skip_junk: true,
            windows_safe: false,
            mac_metadata: false,
            threads: 0,
            solid_block_bytes: 128 * 1024 * 1024,
        }
    }
}

/// 一个待压缩的源项。
#[derive(Clone, Debug)]
pub struct SourceItem {
    /// 包内路径（`/` 分隔、NFC、目录不带尾部 `/`）
    pub rel: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: Option<i64>,
    pub mode: Option<u32>,
    pub link_target: Option<Vec<u8>>,
}

/// 收集结果。
#[derive(Debug, Default)]
pub struct Sources {
    pub items: Vec<SourceItem>,
    pub total_bytes: u64,
    pub skipped_junk: u64,
    pub warnings: Vec<Warning>,
}

/// 遍历给定的根路径（文件或目录），生成包内条目列表。
/// 每个根以自己的文件名作为顶层名；目录递归，不跟随符号链接（链接本身作为条目）。
pub fn collect_sources(roots: &[PathBuf], opts: &CreateOptions) -> Result<Sources> {
    let mut out = Sources::default();
    for root in roots {
        let root_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Other(format!("无法取得文件名：{}", root.display())))?;
        walk(root, &root_name, opts, &mut out)?;
    }
    // 包内顺序按路径排序：输出稳定，同目录文件相邻也利于 solid 压缩率
    out.items.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// 递归收集一个路径。
fn walk(path: &Path, rel: &str, opts: &CreateOptions, out: &mut Sources) -> Result<()> {
    if opts.skip_junk && names::is_junk_path(rel) {
        out.skipped_junk += 1;
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(path)?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode())
    };
    #[cfg(not(unix))]
    let mode = None;

    let rel = finalize_rel(rel, opts, &mut out.warnings);
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        out.items.push(SourceItem {
            rel,
            path: path.to_path_buf(),
            kind: EntryKind::Symlink,
            size: 0,
            mtime,
            mode,
            link_target: Some(target.to_string_lossy().as_bytes().to_vec()),
        });
    } else if meta.is_dir() {
        out.items.push(SourceItem { rel: rel.clone(), path: path.to_path_buf(), kind: EntryKind::Dir, size: 0, mtime, mode, link_target: None });
        let mut children: Vec<_> = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let name = child.file_name().to_string_lossy().into_owned();
            walk(&child.path(), &format!("{rel}/{name}"), opts, out)?;
        }
    } else if meta.is_file() {
        out.total_bytes += meta.len();
        out.items.push(SourceItem { rel, path: path.to_path_buf(), kind: EntryKind::File, size: meta.len(), mtime, mode, link_target: None });
    } else {
        out.warnings.push(Warning::new(WarningKind::SpecialFileSkipped, path.display().to_string()));
    }
    Ok(())
}

/// 包内路径定稿：NFC 归一化；「发给 Windows」时逐段改成安全名并记录改动。
fn finalize_rel(rel: &str, opts: &CreateOptions, warnings: &mut Vec<Warning>) -> String {
    let nfc = normalize_for_write(rel);
    if !opts.windows_safe {
        return nfc;
    }
    let mut parts = Vec::new();
    for comp in nfc.split('/') {
        let (safe, _reasons) = names::windows_safe_name(comp);
        if safe != comp {
            warnings.push(Warning::with_detail(WarningKind::RenamedForWindows, comp, safe.clone()));
        }
        parts.push(safe);
    }
    parts.join("/")
}

/// 压缩结果。
#[derive(Debug, Default)]
pub struct CreateReport {
    pub entries: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub skipped_junk: u64,
    pub warnings: Vec<Warning>,
}

/// 创建压缩包。`dest` 已存在会被覆盖（调用方负责确认）。
pub fn create_archive(dest: &Path, roots: &[PathBuf], opts: &CreateOptions, ctx: &OpContext<'_>) -> Result<CreateReport> {
    let sources = collect_sources(roots, opts)?;
    let mut report = CreateReport {
        entries: sources.items.len() as u64,
        input_bytes: sources.total_bytes,
        skipped_junk: sources.skipped_junk,
        warnings: sources.warnings.clone(),
        ..Default::default()
    };
    match opts.format {
        CreateFormat::Zip => report.warnings.extend(crate::create::zip_writer::write_zip(dest, &sources, opts, ctx)?),
        CreateFormat::SevenZ => crate::create::sevenz_writer::write_7z(dest, &sources, opts, ctx)?,
        CreateFormat::TarGz | CreateFormat::TarXz | CreateFormat::TarZst => crate::create::tar_writer::write_tar(dest, &sources, opts, ctx)?,
    }
    report.output_bytes = std::fs::metadata(dest)?.len();
    Ok(report)
}

pub mod apple_double;
pub mod sevenz_writer;
pub mod tar_writer;
pub mod zip_writer;
