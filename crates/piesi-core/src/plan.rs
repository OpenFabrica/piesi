//! 解压计划：决定每个条目落到哪里、冲突怎么办、要不要建顶层文件夹，
//! 并把驱动的 `EntrySink` 调用映射到文件系统的 `FileSink`。

use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::archive::{Entry, EntryId, EntryKind, EntrySink};
use crate::fs::{FileSink, Meta};
use crate::names;
use crate::Result;

/// 目标位置已有同名文件时怎么办。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConflictPolicy {
    Overwrite,
    Skip,
    Rename,
}

/// 解压选项。
#[derive(Clone, Debug)]
pub struct ExtractOptions {
    /// 用户选的目标目录（默认是压缩包所在目录）
    pub dest: PathBuf,
    /// 压缩包去掉扩展名后的名字，智能建文件夹时用它
    pub archive_stem: String,
    /// 全部解压且顶层有多个项目时，自动建 `<archive_stem>/`，不把一堆文件撒进目标目录
    pub smart_folder: bool,
    pub conflict: ConflictPolicy,
    /// 跳过 `__MACOSX`、`.DS_Store` 这类垃圾
    pub skip_junk: bool,
}

impl ExtractOptions {
    /// 按设计默认值：解压到压缩包旁边、智能建文件夹、冲突改名、跳过垃圾。
    pub fn beside_archive(archive: &Path) -> Self {
        let dest = archive.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        Self {
            dest,
            archive_stem: archive_stem(archive),
            smart_folder: true,
            conflict: ConflictPolicy::Rename,
            skip_junk: true,
        }
    }
}

/// 压缩包名去掉扩展名：`a.tar.gz` → `a`，`b.7z.001` → `b`，`c.zip` → `c`。
pub fn archive_stem(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    const SUFFIXES: &[&str] = &[
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".zip.001", ".7z.001", ".tgz", ".tbz", ".tbz2", ".txz", ".tzst",
        ".zip", ".7z", ".rar", ".tar", ".gz", ".bz2", ".xz", ".zst", ".jar", ".apk", ".ipa", ".zipx",
    ];
    for s in SUFFIXES {
        if lower.ends_with(s) && name.len() > s.len() {
            return name[..name.len() - s.len()].to_owned();
        }
    }
    name
}

/// 计划里的一项：条目 id 与它在目标根目录下的相对路径。
#[derive(Clone, Debug)]
pub struct PlannedItem {
    pub id: EntryId,
    pub rel: String,
    pub kind: EntryKind,
}

/// 解压计划：UI 可以在真正解压前展示冲突、总大小和警告。
#[derive(Debug)]
pub struct ExtractPlan {
    /// 实际写入的根目录（`dest` 或 `dest/<stem>`）
    pub root: PathBuf,
    pub items: Vec<PlannedItem>,
    pub total_bytes: u64,
    /// 目标里已存在、会被策略处理的路径
    pub conflicts: Vec<String>,
    pub warnings: Vec<String>,
    pub skipped_junk: u64,
    rel_by_id: HashMap<EntryId, String>,
}

impl ExtractPlan {
    /// 计划里全部条目 id（按包内顺序），交给驱动的 `extract`。
    pub fn ids(&self) -> Vec<EntryId> {
        self.items.iter().map(|i| i.id).collect()
    }

    /// 条目在目标根目录下的相对路径；不在计划里返回 None。
    pub fn rel_of(&self, id: EntryId) -> Option<&str> {
        self.rel_by_id.get(&id).map(String::as_str)
    }
}

/// 生成解压计划。`names` 是和 `entries` 对齐的已解码名字；`selected` 为 None 表示全部。
pub fn plan_extract(entries: &[Entry], names: &[String], selected: Option<&[EntryId]>, opts: &ExtractOptions) -> Result<ExtractPlan> {
    let mut warnings = Vec::new();
    let mut skipped_junk = 0u64;

    // 1. 选中集合：选了目录就连同它下面的全部条目
    let chosen: Vec<&Entry> = match selected {
        None => entries.iter().collect(),
        Some(ids) => {
            let mut set: HashSet<EntryId> = ids.iter().copied().collect();
            let mut prefixes: Vec<String> = Vec::new();
            for &id in ids {
                if let Some(e) = entries.get(id as usize) {
                    if e.kind == EntryKind::Dir {
                        let mut p = names[id as usize].trim_end_matches('/').to_owned();
                        p.push('/');
                        prefixes.push(p);
                    }
                }
            }
            if !prefixes.is_empty() {
                for e in entries {
                    if prefixes.iter().any(|p| names[e.id as usize].starts_with(p.as_str())) {
                        set.insert(e.id);
                    }
                }
            }
            entries.iter().filter(|e| set.contains(&e.id)).collect()
        }
    };

    // 2. 路径安全化 + 垃圾过滤 + APFS 长度限制
    let mut items: Vec<PlannedItem> = Vec::with_capacity(chosen.len());
    for e in &chosen {
        let name = &names[e.id as usize];
        if opts.skip_junk && names::is_junk_path(name) {
            skipped_junk += 1;
            continue;
        }
        let rel = match names::sanitize_rel_path(name) {
            Ok(r) => r,
            Err(err) => {
                warnings.push(format!("跳过条目 #{}：{err}", e.id));
                continue;
            }
        };
        if rel.is_empty() {
            continue;
        }
        let rel = rel.split('/').map(names::fit_apfs_component).collect::<Vec<_>>().join("/");
        items.push(PlannedItem { id: e.id, rel, kind: e.kind });
    }

    // 3. 智能建文件夹：只在全部解压时考虑；顶层不止一个项目，或唯一项目是文件，就套一层
    let mut root = opts.dest.clone();
    if selected.is_none() && opts.smart_folder {
        let tops: HashSet<&str> = items.iter().map(|i| i.rel.split('/').next().unwrap_or("")).collect();
        let single_dir = tops.len() == 1
            && items.iter().any(|i| i.kind == EntryKind::Dir && !i.rel.contains('/'))
            || (tops.len() == 1 && items.iter().all(|i| i.rel.contains('/')));
        if !single_dir {
            let mut folder = opts.archive_stem.clone();
            if folder.is_empty() {
                folder = "extracted".to_owned();
            }
            root = opts.dest.join(&folder);
            if opts.conflict == ConflictPolicy::Rename && root.exists() {
                root = free_path(&opts.dest, &folder, true);
            }
        }
    }

    // 4. 大小写冲突：macOS 默认文件系统不区分大小写，同目录下 README 和 readme 会互相覆盖
    let mut seen_lower: HashMap<String, usize> = HashMap::new();
    for i in 0..items.len() {
        let key = items[i].rel.to_lowercase();
        if let Some(&first) = seen_lower.get(&key) {
            if items[first].kind == EntryKind::Dir && items[i].kind == EntryKind::Dir {
                continue; // 两个目录合并即可
            }
            let renamed = with_suffix(&items[i].rel, 2);
            warnings.push(format!("大小写冲突：{} 改名为 {}", items[i].rel, renamed));
            items[i].rel = renamed;
        } else {
            seen_lower.insert(key, i);
        }
    }

    // 5. 与目标目录已有文件的冲突
    let mut conflicts = Vec::new();
    let mut kept: Vec<PlannedItem> = Vec::with_capacity(items.len());
    for mut item in items {
        if item.kind != EntryKind::Dir && root.join(&item.rel).symlink_metadata().is_ok() {
            conflicts.push(item.rel.clone());
            match opts.conflict {
                ConflictPolicy::Overwrite => {}
                ConflictPolicy::Skip => continue,
                ConflictPolicy::Rename => {
                    item.rel = free_rel(&root, &item.rel);
                }
            }
        }
        kept.push(item);
    }

    let total_bytes = kept
        .iter()
        .filter(|i| i.kind == EntryKind::File)
        .map(|i| entries[i.id as usize].size)
        .sum();
    let rel_by_id = kept.iter().map(|i| (i.id, i.rel.clone())).collect();
    Ok(ExtractPlan { root, items: kept, total_bytes, conflicts, warnings, skipped_junk, rel_by_id })
}

/// 给路径最后一段加 ` (n)`，扩展名保留：`a/b.txt` → `a/b (2).txt`。
fn with_suffix(rel: &str, n: u32) -> String {
    let (dir, file) = match rel.rfind('/') {
        Some(i) => (&rel[..=i], &rel[i + 1..]),
        None => ("", rel),
    };
    let (stem, ext) = match file.rfind('.') {
        Some(i) if i > 0 => (&file[..i], &file[i..]),
        _ => (file, ""),
    };
    format!("{dir}{stem} ({n}){ext}")
}

/// 在 `root` 下找一个不存在的相对路径。
fn free_rel(root: &Path, rel: &str) -> String {
    for n in 2..10_000 {
        let candidate = with_suffix(rel, n);
        if root.join(&candidate).symlink_metadata().is_err() {
            return candidate;
        }
    }
    with_suffix(rel, 10_000)
}

/// 在 `dir` 下找一个不存在的子路径名。
fn free_path(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    for n in 2..10_000 {
        let candidate = if is_dir { format!("{name} ({n})") } else { with_suffix(name, n) };
        let p = dir.join(&candidate);
        if p.symlink_metadata().is_err() {
            return p;
        }
    }
    dir.join(format!("{name} (10000)"))
}

/// 把驱动的 `EntrySink` 调用按计划映射到 `FileSink`。
/// 不在计划里的条目（被策略跳过）要把数据读空再丢掉，solid 格式的驱动才能继续往下走。
pub struct PlanSink<'a> {
    plan: &'a ExtractPlan,
    sink: &'a mut dyn FileSink,
}

impl<'a> PlanSink<'a> {
    /// 组装映射器。
    pub fn new(plan: &'a ExtractPlan, sink: &'a mut dyn FileSink) -> Self {
        Self { plan, sink }
    }

    /// 条目的落盘元数据。
    fn meta(entry: &Entry) -> Meta {
        Meta { mtime: entry.mtime, mode: entry.unix_mode }
    }
}

impl EntrySink for PlanSink<'_> {
    fn dir(&mut self, entry: &Entry) -> Result<()> {
        match self.plan.rel_of(entry.id) {
            Some(rel) => self.sink.make_dir(rel, &Self::meta(entry)),
            None => Ok(()),
        }
    }

    fn file(&mut self, entry: &Entry, data: &mut dyn Read) -> Result<u64> {
        match self.plan.rel_of(entry.id) {
            Some(rel) => self.sink.write_file(rel, &Self::meta(entry), data),
            None => Ok(io::copy(data, &mut io::sink())?),
        }
    }

    fn symlink(&mut self, entry: &Entry, target: &[u8]) -> Result<()> {
        match self.plan.rel_of(entry.id) {
            Some(rel) => self.sink.add_symlink(rel, target, &Self::meta(entry)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个文件条目。
    fn file(id: u32, name: &str, size: u64) -> Entry {
        Entry::new(id, name.as_bytes().to_vec(), EntryKind::File, size)
    }

    /// 构造一个目录条目。
    fn dir(id: u32, name: &str) -> Entry {
        Entry::new(id, name.as_bytes().to_vec(), EntryKind::Dir, 0)
    }

    /// 顶层多个文件 → 套一层文件夹；唯一顶层目录 → 不套。
    #[test]
    fn smart_folder_rules() {
        let tmp = std::env::temp_dir().join(format!("piesi-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let opts = ExtractOptions { dest: tmp.clone(), archive_stem: "pkg".into(), smart_folder: true, conflict: ConflictPolicy::Rename, skip_junk: true };

        let entries = vec![file(0, "a.txt", 1), file(1, "b.txt", 2), file(2, "__MACOSX/._a.txt", 3)];
        let names: Vec<String> = entries.iter().map(|e| String::from_utf8(e.raw_name.clone()).unwrap()).collect();
        let plan = plan_extract(&entries, &names, None, &opts).unwrap();
        assert_eq!(plan.root, tmp.join("pkg"));
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.skipped_junk, 1);
        assert_eq!(plan.total_bytes, 3);

        let entries = vec![dir(0, "top/"), file(1, "top/a.txt", 1), file(2, "top/sub/b.txt", 2)];
        let names: Vec<String> = entries.iter().map(|e| String::from_utf8(e.raw_name.clone()).unwrap()).collect();
        let plan = plan_extract(&entries, &names, None, &opts).unwrap();
        assert_eq!(plan.root, tmp);

        // 选中目录 = 连同子项
        let plan = plan_extract(&entries, &names, Some(&[0]), &opts).unwrap();
        assert_eq!(plan.items.len(), 3);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// 大小写冲突与已有文件冲突的改名。
    #[test]
    fn conflicts_rename() {
        let tmp = std::env::temp_dir().join(format!("piesi-plan2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("x.txt"), b"old").unwrap();
        let opts = ExtractOptions { dest: tmp.clone(), archive_stem: "pkg".into(), smart_folder: false, conflict: ConflictPolicy::Rename, skip_junk: true };
        let entries = vec![file(0, "README", 1), file(1, "readme", 1), file(2, "x.txt", 1)];
        let names: Vec<String> = entries.iter().map(|e| String::from_utf8(e.raw_name.clone()).unwrap()).collect();
        let plan = plan_extract(&entries, &names, None, &opts).unwrap();
        assert_eq!(plan.rel_of(1), Some("readme (2)"));
        assert_eq!(plan.rel_of(2), Some("x (2).txt"));
        assert_eq!(plan.conflicts, vec!["x.txt".to_string()]);
        assert_eq!(archive_stem(Path::new("/a/b.tar.gz")), "b");
        assert_eq!(archive_stem(Path::new("c.7z.001")), "c");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
