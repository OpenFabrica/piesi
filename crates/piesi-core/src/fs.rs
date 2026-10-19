//! 落盘层：所有解压输出都经 cap-std 的 `Dir` 写入，系统调用层保证写不出目标目录；
//! 另负责 quarantine 传播、符号链接延后创建、目录时间回写。

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};

use crate::archive::ExtractReport;
use crate::warning::{Warning, WarningKind};
use crate::{Error, Result};

/// 落盘时要回写的元数据。
#[derive(Clone, Copy, Debug, Default)]
pub struct Meta {
    /// 修改时间，Unix 秒
    pub mtime: Option<i64>,
    /// Unix 权限位（只取低 12 位用）
    pub mode: Option<u32>,
}

/// 面向文件系统的接收端：按已经规划好的相对路径写入。驱动不直接碰它，中间由 plan 做映射。
pub trait FileSink {
    /// 创建目录（含中间目录）。
    fn make_dir(&mut self, rel: &str, meta: &Meta) -> Result<()>;
    /// 写入文件内容并回写元数据，返回写入字节数。
    fn write_file(&mut self, rel: &str, meta: &Meta, data: &mut dyn Read) -> Result<u64>;
    /// 登记符号链接；实现方应延后到 `finish` 再真正创建。
    fn add_symlink(&mut self, rel: &str, target: &[u8], meta: &Meta) -> Result<()>;
    /// 收尾：创建符号链接、回写目录时间、处理 quarantine。
    fn finish(&mut self) -> Result<ExtractReport>;
}

/// macOS 的隔离属性名。
pub const QUARANTINE_XATTR: &str = "com.apple.quarantine";

/// 读取压缩包自身的 quarantine 值；没有返回 None。非 macOS 永远 None。
pub fn read_quarantine(path: &Path) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        xattr::get(path, QUARANTINE_XATTR).ok().flatten()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// 写到真实目录的接收端。
pub struct DirSink {
    root: Dir,
    root_path: PathBuf,
    /// 源压缩包的 quarantine 值：Some 则给每个输出打上同样的值；None 则确保输出没有它
    quarantine: Option<Vec<u8>>,
    /// 延后创建的符号链接：(相对路径, 目标, 元数据)
    pending_links: Vec<(String, Vec<u8>, Meta)>,
    /// 延后回写时间的目录：解压过程中往目录里写文件会刷新它的 mtime，必须最后统一回写
    pending_dir_meta: Vec<(String, Meta)>,
    report: ExtractReport,
}

impl DirSink {
    /// 打开（必要时创建）目标目录。`quarantine` 传 `read_quarantine(压缩包路径)` 的结果。
    pub fn create(root: &Path, quarantine: Option<Vec<u8>>) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let dir = Dir::open_ambient_dir(root, ambient_authority())?;
        Ok(Self {
            root: dir,
            root_path: root.to_path_buf(),
            quarantine,
            pending_links: Vec::new(),
            pending_dir_meta: Vec::new(),
            report: ExtractReport::default(),
        })
    }

    /// 目标根目录的绝对路径。
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// 确保 `rel` 的父目录存在。
    fn ensure_parent(&self, rel: &str) -> Result<()> {
        if let Some(parent) = Path::new(rel).parent() {
            if !parent.as_os_str().is_empty() {
                self.root.create_dir_all(parent)?;
            }
        }
        Ok(())
    }

    /// 给刚写出的文件回写 mtime、权限、quarantine。
    fn apply_file_meta(&self, file: std::fs::File, rel: &str, meta: &Meta) -> Result<()> {
        if let Some(mode) = meta.mode {
            use std::os::unix::fs::PermissionsExt;
            // 只保留权限位，丢掉文件类型位；同时强制属主可读写，免得解压出一个自己都删不掉的文件
            let perm = (mode & 0o7777) | 0o600;
            file.set_permissions(std::fs::Permissions::from_mode(perm))?;
        }
        if let Some(mtime) = meta.mtime {
            let t = unix_to_system_time(mtime);
            let _ = file.set_times(std::fs::FileTimes::new().set_modified(t));
        }
        drop(file);
        self.apply_quarantine(rel)
    }

    /// 按源压缩包的状态同步输出的 quarantine：有则打上同值，无则去掉。
    /// 这样 app（LSFileQuarantineEnabled 会给所有新文件打标）和 CLI（什么都不打）行为一致。
    fn apply_quarantine(&self, rel: &str) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let full = self.root_path.join(rel);
            match &self.quarantine {
                Some(v) => {
                    xattr::set(&full, QUARANTINE_XATTR, v)?;
                }
                None => match xattr::remove(&full, QUARANTINE_XATTR) {
                    Ok(()) => {}
                    Err(e) if e.raw_os_error() == Some(libc_enoattr()) || e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                },
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = rel;
        }
        Ok(())
    }
}

/// macOS 上 removexattr 对不存在的属性返回 ENOATTR（93）。
#[cfg(target_os = "macos")]
fn libc_enoattr() -> i32 {
    93
}

/// Unix 秒转 SystemTime，负数（1970 前）按 epoch 处理。
fn unix_to_system_time(secs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH
    }
}

/// 符号链接目标是否会跑出根目录：把 `link 所在目录/target` 做纯词法归一化后看有没有越过顶层。
/// 不解析真实文件系统，因为创建时目标可能还不存在；词法检查足够拦住 `../../etc/passwd` 这类。
pub fn symlink_escapes(link_rel: &str, target: &[u8]) -> bool {
    let target = String::from_utf8_lossy(target);
    let target_path = Path::new(target.as_ref());
    if target_path.is_absolute() {
        return true;
    }
    let base = Path::new(link_rel).parent().unwrap_or(Path::new(""));
    let mut depth: i64 = base.components().filter(|c| matches!(c, Component::Normal(_))).count() as i64;
    for c in target_path.components() {
        match c {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    false
}

impl FileSink for DirSink {
    fn make_dir(&mut self, rel: &str, meta: &Meta) -> Result<()> {
        self.root.create_dir_all(rel)?;
        if meta.mtime.is_some() || meta.mode.is_some() {
            self.pending_dir_meta.push((rel.to_owned(), *meta));
        }
        self.apply_quarantine(rel)
    }

    fn write_file(&mut self, rel: &str, meta: &Meta, data: &mut dyn Read) -> Result<u64> {
        self.ensure_parent(rel)?;
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        let file = self.root.open_with(rel, &opts)?.into_std();
        // 1 MB 写缓冲：io::copy 按它的容量读解码器、写文件；默认 8 KB 在 2 GB 文件上多花约 0.5 s 系统调用（dd 实测 1.6 s 对 1.1 s）
        let mut writer = io::BufWriter::with_capacity(1 << 20, file);
        let n = io::copy(data, &mut writer)?;
        writer.flush()?;
        let file = writer.into_inner().map_err(|e| Error::Io(e.into_error()))?;
        self.apply_file_meta(file, rel, meta)?;
        self.report.extracted += 1;
        Ok(n)
    }

    fn add_symlink(&mut self, rel: &str, target: &[u8], meta: &Meta) -> Result<()> {
        // 越界的链接直接跳过并记录，不做成普通文件：静默改类型反而更难排查
        if symlink_escapes(rel, target) {
            self.report.skipped += 1;
            self.report.warnings.push(Warning::with_detail(WarningKind::SymlinkEscapeSkipped, rel, String::from_utf8_lossy(target)));
            return Ok(());
        }
        self.pending_links.push((rel.to_owned(), target.to_vec(), *meta));
        Ok(())
    }

    fn finish(&mut self) -> Result<ExtractReport> {
        // 符号链接最后创建：解压过程中任何写入都不会经过它，杜绝「先放链接再往里写」的逃逸手法
        let links = std::mem::take(&mut self.pending_links);
        for (rel, target, _meta) in links {
            self.ensure_parent(&rel)?;
            if self.root.exists(&rel) {
                self.root.remove_file(&rel)?;
            }
            let target_str = String::from_utf8_lossy(&target).into_owned();
            match self.root.symlink(&target_str, &rel) {
                Ok(()) => {
                    self.report.extracted += 1;
                    self.apply_quarantine(&rel)?;
                }
                Err(e) => {
                    self.report.skipped += 1;
                    self.report.warnings.push(Warning::with_detail(WarningKind::SymlinkCreateFailed, rel, e.to_string()));
                }
            }
        }
        // 目录时间最后回写，且先深后浅：回写父目录后再动子目录会把父目录 mtime 再次刷新
        let mut dirs = std::mem::take(&mut self.pending_dir_meta);
        dirs.sort_by(|a, b| b.0.matches('/').count().cmp(&a.0.matches('/').count()).then(b.0.cmp(&a.0)));
        for (rel, meta) in dirs {
            if let Ok(d) = self.root.open_dir(&rel) {
                let f = d.into_std_file();
                if let Some(mode) = meta.mode {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = f.set_permissions(std::fs::Permissions::from_mode((mode & 0o7777) | 0o700));
                }
                if let Some(mtime) = meta.mtime {
                    let _ = f.set_times(std::fs::FileTimes::new().set_modified(unix_to_system_time(mtime)));
                }
            }
        }
        Ok(std::mem::take(&mut self.report))
    }
}

/// 把内容写到同目录的临时文件再原子替换目标：中途失败不会留下半个文件。
/// 用于以后的「编辑压缩包后保存」和写压缩包。
pub fn atomic_replace(target: &Path, write: impl FnOnce(&mut std::fs::File) -> Result<()>) -> Result<()> {
    let dir = target.parent().ok_or_else(|| Error::Other("目标路径没有父目录".into()))?;
    let tmp = dir.join(format!(
        ".{}.piesi-tmp-{}",
        target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        std::process::id()
    ));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        write(&mut f)?;
        f.sync_all()?;
        std::fs::rename(&tmp, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写文件、建目录、合法与越界符号链接的完整回合。
    #[test]
    fn dir_sink_roundtrip() {
        let root = std::env::temp_dir().join(format!("piesi-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut sink = DirSink::create(&root, None).unwrap();
        sink.make_dir("a/b", &Meta { mtime: Some(1_600_000_000), mode: Some(0o755) }).unwrap();
        let n = sink
            .write_file("a/b/hello.txt", &Meta { mtime: Some(1_600_000_000), mode: Some(0o644) }, &mut &b"hello"[..])
            .unwrap();
        assert_eq!(n, 5);
        sink.add_symlink("a/link", b"b/hello.txt", &Meta::default()).unwrap();
        sink.add_symlink("a/evil", b"../../etc/passwd", &Meta::default()).unwrap();
        sink.add_symlink("a/abs", b"/etc/passwd", &Meta::default()).unwrap();
        let report = sink.finish().unwrap();
        assert_eq!(report.extracted, 2);
        assert_eq!(report.skipped, 2);
        assert_eq!(std::fs::read_to_string(root.join("a/link")).unwrap(), "hello");
        assert!(!root.join("a/evil").exists() && std::fs::symlink_metadata(root.join("a/evil")).is_err());
        let mtime = std::fs::metadata(root.join("a/b/hello.txt")).unwrap().modified().unwrap();
        assert_eq!(mtime.duration_since(UNIX_EPOCH).unwrap().as_secs(), 1_600_000_000);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 词法越界判断。
    #[test]
    fn escape_detection() {
        assert!(!symlink_escapes("a/b/link", b"../x"));
        assert!(!symlink_escapes("a/b/link", b"../../x"));
        assert!(symlink_escapes("a/b/link", b"../../../x"));
        assert!(symlink_escapes("link", b"/abs"));
        assert!(!symlink_escapes("link", b"./same"));
    }
}
