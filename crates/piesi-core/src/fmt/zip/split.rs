//! 分卷定位：`.zip.001` 字节分卷直接串联成一条流；PKWARE `.z01` 多盘分卷按盘号串联，
//! 并记录每盘在逻辑流里的起点，中央目录里的（盘号, 盘内偏移）据此换算成逻辑偏移。

use std::path::{Path, PathBuf};

use crate::volumes::{collect_volumes, ChainedReader};
use crate::{DetailKind, Error, ErrorDetail, Result};

/// 盘数上限：PKWARE 的盘号是 16 位（zip64 为 32 位），实际没人超过这个数，拿它挡住畸形输入。
const MAX_DISKS: u32 = 65_536;

/// 打开后的数据源：串联流 + 每盘在逻辑流里的起点（单盘时只有一个 0）。
pub struct Source {
    pub reader: ChainedReader,
    pub disk_base: Vec<u64>,
}

impl Source {
    /// 按路径列表串联。`multi_disk` 为真时每个文件算一盘并记录起点；否则整条流算一盘。
    fn from_parts(parts: Vec<PathBuf>, multi_disk: bool) -> Result<Self> {
        let mut disk_base = vec![0u64];
        if multi_disk {
            let mut total = 0u64;
            disk_base.clear();
            for p in &parts {
                disk_base.push(total);
                let len = std::fs::metadata(p)?.len();
                total = total.checked_add(len).ok_or_else(|| Error::Corrupt(DetailKind::VolumeTotalOverflow.into()))?;
            }
        }
        let reader = ChainedReader::open(&parts)?;
        Ok(Self { reader, disk_base })
    }

    /// 是否按多盘打开。
    pub fn is_multi_disk(&self) -> bool {
        self.disk_base.len() > 1
    }

    /// 逻辑流总长。
    pub fn len(&self) -> u64 {
        self.reader.len()
    }
}

/// 文件名是否以 `.001` 结尾（7-Zip 风格字节分卷首段）。
fn is_byte_split(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.len() > 4 && n[n.len() - 4..].eq_ignore_ascii_case(".001"))
        .unwrap_or(false)
}

/// 打开首卷：`.001` 走字节分卷（zip 眼里仍是单盘，偏移直接用），其它按单文件。
pub fn open_initial(path: &Path) -> Result<Source> {
    let parts = if is_byte_split(path) { collect_volumes(path)? } else { vec![path.to_path_buf()] };
    Source::from_parts(parts, false)
}

/// PKWARE 多盘：`name.zip` 的兄弟卷是 `name.z01`…`name.z(N-1)`，最后一盘就是 `name.zip` 本身。
pub fn open_pkware(path: &Path, total_disks: u32) -> Result<Source> {
    if !(2..=MAX_DISKS).contains(&total_disks) {
        return Err(Error::Corrupt(ErrorDetail::with(DetailKind::VolumeCountInvalid, total_disks)));
    }
    let mut parts = Vec::with_capacity(total_disks as usize);
    for i in 1..total_disks {
        let p = sibling(path, i).ok_or_else(|| Error::Corrupt(ErrorDetail::with(DetailKind::VolumeMissing, sibling_name(path, i))))?;
        parts.push(p);
    }
    parts.push(path.to_path_buf());
    Source::from_parts(parts, true)
}

/// 修复扫描用：不知道盘数，连续探测 `.z01`、`.z02`… 直到缺失；一个都没有就按单文件打开。
pub fn probe_pkware(path: &Path) -> Result<Source> {
    let mut parts = Vec::new();
    for i in 1..MAX_DISKS {
        match sibling(path, i) {
            Some(p) => parts.push(p),
            None => break,
        }
    }
    if parts.is_empty() {
        return open_initial(path);
    }
    parts.push(path.to_path_buf());
    Source::from_parts(parts, true)
}

/// 第 `i` 盘的文件名（不含目录），大小写跟随主文件扩展名。
fn sibling_name(path: &Path, i: u32) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let upper = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.chars().any(|c| c.is_ascii_uppercase()))
        .unwrap_or(false);
    if upper {
        format!("{stem}.Z{i:02}")
    } else {
        format!("{stem}.z{i:02}")
    }
}

/// 第 `i` 盘的路径，存在才返回；先试跟主文件同大小写的名字，再试另一种。
fn sibling(path: &Path, i: u32) -> Option<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    let first = sibling_name(path, i);
    let other = if first.contains(".Z") { format!("{stem}.z{i:02}") } else { format!("{stem}.Z{i:02}") };
    for name in [first, other] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}
