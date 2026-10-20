//! AppleDouble（`__MACOSX/._name`）打包：把 macOS 文件的 Finder 信息、扩展属性、资源分支编成和
//! ditto / 归档工具（都走 copyfile）逐字节一致的 `._` 文件，zip 的「Mac 专用」预设用。
//!
//! 布局以本机 `ditto -c -k --sequesterRsrc` 的产物为准（2026-10-19 用 Python 逐字节核对），全部大端：
//!
//! ```text
//! 偏移   长度  内容
//! 0      4     magic 0x00051607
//! 4      4     version 0x00020000
//! 8      16    "Mac OS X        "（16 字节，空格补齐）
//! 24     2     numEntries = 2（永远是 Finder Info + 资源分支两条，没有资源分支时长度写 0）
//! 26     12    entry[0]：type 9（Finder Info），offset 50，length = total_size - 50
//! 38     12    entry[1]：type 2（资源分支），offset = total_size，length = 资源分支长度
//! 50     32    Finder Info（没有就全 0）
//! 82     2     pad（0）
//! 84     36    attr_header："ATTR"、debug_tag、total_size、data_start、data_length、reserved[3]、flags、num_attrs
//! 120    …     attr_entry 表：offset u32、length u32、flags u16、namelen u8、name（含结尾 NUL），每条 4 字节对齐
//! data_start   各 xattr 值按条目顺序紧挨着放（不对齐）
//! total_size   资源分支原样
//! ```
//!
//! 为什么 Finder Info 条目的长度覆盖整个 ATTR 块：这是 XNU `vfs_xattr.c` 的 `attr_header` 约定——
//! AppleDouble 标准只认 32 字节 Finder Info，Apple 把扩展属性藏在同一个条目的「尾巴」里，老工具只读前 32
//! 字节，copyfile 看到长度大于 32 再去找 "ATTR" 魔数。

use std::path::Path;

use crate::warning::{Warning, WarningKind};

/// Finder Info 固定 32 字节。
pub const FINDER_INFO_LEN: usize = 32;
/// 单个 fork（一个 xattr 值或资源分支）超过这个大小就跳过并给 warning，防止把一个巨型资源分支整个读进内存。
pub const MAX_FORK_BYTES: u64 = 64 * 1024 * 1024;

const AD_MAGIC: u32 = 0x0005_1607;
const AD_VERSION: u32 = 0x0002_0000;
const AD_FILLER: &[u8; 16] = b"Mac OS X        ";
const AD_ENTRY_FINDER_INFO: u32 = 9;
const AD_ENTRY_RESOURCE_FORK: u32 = 2;
/// Finder Info 在文件里的偏移：26 字节头 + 2 条 12 字节的条目表。
const FINDER_INFO_OFFSET: usize = 26 + 2 * 12;
/// "ATTR" 头的偏移：Finder Info 之后再加 2 字节 pad。
const ATTR_HEADER_OFFSET: usize = FINDER_INFO_OFFSET + FINDER_INFO_LEN + 2;
/// attr_entry 表的起点：ATTR 头固定 36 字节。
const ATTR_ENTRIES_OFFSET: usize = ATTR_HEADER_OFFSET + 36;
/// XNU 规定 ATTR 头 + 条目表必须放进 64 KB（数据区不限），超过的属性只能丢掉。
const ATTR_MAX_HDR_SIZE: usize = 65_536;

/// 一个文件或目录的 Mac 元数据：Finder 信息、扩展属性（已剔除 quarantine、资源分支、Finder 信息本身）、资源分支。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MacMetadata {
    /// 32 字节 Finder 信息；全 0 视为没有
    pub finder_info: Option<[u8; FINDER_INFO_LEN]>,
    /// (名字, 值)，按名字字节序排好，名字不含结尾 NUL
    pub xattrs: Vec<(Vec<u8>, Vec<u8>)>,
    pub resource_fork: Vec<u8>,
}

impl MacMetadata {
    /// 什么都没有：这种文件不写 `._` 条目。
    pub fn is_empty(&self) -> bool {
        self.finder_info.is_none() && self.xattrs.is_empty() && self.resource_fork.is_empty()
    }
}

/// `rel` 对应的 AppleDouble 包内路径：`a/b/c.txt` → `__MACOSX/a/b/._c.txt`，`c.txt` → `__MACOSX/._c.txt`。
pub fn apple_double_rel(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, name)) => format!("__MACOSX/{dir}/._{name}"),
        None => format!("__MACOSX/._{rel}"),
    }
}

/// 一条 attr_entry 在表里占的字节数：11 字节定长 + 名字（含 NUL），向上对齐到 4。
/// 公式照抄 XNU 的 `ATTR_ENTRY_LENGTH`，所以 `namelen` 是含 NUL 的长度。
fn attr_entry_len(namelen_with_nul: usize) -> usize {
    (11 + namelen_with_nul + 3) & !3
}

/// 把元数据编成 AppleDouble 字节流。输入已经过 `read` 的大小/数量限制，这里不会失败。
pub fn encode(meta: &MacMetadata) -> Vec<u8> {
    let entries_len: usize = meta.xattrs.iter().map(|(n, _)| attr_entry_len(n.len() + 1)).sum();
    let data_start = ATTR_ENTRIES_OFFSET + entries_len;
    let data_length: usize = meta.xattrs.iter().map(|(_, v)| v.len()).sum();
    let total_size = data_start + data_length;

    let mut out = Vec::with_capacity(total_size + meta.resource_fork.len());
    out.extend_from_slice(&AD_MAGIC.to_be_bytes());
    out.extend_from_slice(&AD_VERSION.to_be_bytes());
    out.extend_from_slice(AD_FILLER);
    out.extend_from_slice(&2u16.to_be_bytes());
    // 条目表：Finder Info 条目的长度一直盖到数据区末尾（见模块说明），资源分支紧跟在后面
    for (kind, offset, length) in [
        (AD_ENTRY_FINDER_INFO, FINDER_INFO_OFFSET, total_size - FINDER_INFO_OFFSET),
        (AD_ENTRY_RESOURCE_FORK, total_size, meta.resource_fork.len()),
    ] {
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(length as u32).to_be_bytes());
    }
    out.extend_from_slice(&meta.finder_info.unwrap_or([0; FINDER_INFO_LEN]));
    out.extend_from_slice(&[0, 0]);
    debug_assert_eq!(out.len(), ATTR_HEADER_OFFSET);
    // attr_header：debug_tag 和 flags copyfile 都写 0
    out.extend_from_slice(b"ATTR");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(total_size as u32).to_be_bytes());
    out.extend_from_slice(&(data_start as u32).to_be_bytes());
    out.extend_from_slice(&(data_length as u32).to_be_bytes());
    out.extend_from_slice(&[0u8; 12]);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(meta.xattrs.len() as u16).to_be_bytes());
    debug_assert_eq!(out.len(), ATTR_ENTRIES_OFFSET);
    // attr_entry 表：每条记自己的值在数据区里的绝对偏移；值区不对齐，条目表才对齐
    let mut value_offset = data_start;
    for (name, value) in &meta.xattrs {
        let start = out.len();
        out.extend_from_slice(&(value_offset as u32).to_be_bytes());
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.push((name.len() + 1) as u8);
        out.extend_from_slice(name);
        out.push(0);
        out.resize(start + attr_entry_len(name.len() + 1), 0);
        value_offset += value.len();
    }
    debug_assert_eq!(out.len(), data_start);
    for (_, value) in &meta.xattrs {
        out.extend_from_slice(value);
    }
    debug_assert_eq!(out.len(), total_size);
    out.extend_from_slice(&meta.resource_fork);
    out
}

/// 读取路径上的 Mac 元数据，不跟随符号链接。跳过 quarantine（隔离标记不该跟着包走）和 decmpfs（文件系统
/// 透明压缩的内部数据，写出去再写回会毁掉文件）；超过 `MAX_FORK_BYTES` 的单个 fork 跳过并记 warning。
/// 文件系统不支持 xattr（ENOTSUP）当作没有元数据。非 macOS 永远返回空。
#[cfg(target_os = "macos")]
pub fn read(path: &Path, warnings: &mut Vec<Warning>) -> std::io::Result<MacMetadata> {
    let mut meta = MacMetadata::default();
    let names = match sys::list(path) {
        Ok(n) => n,
        Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => return Ok(meta),
        Err(e) => return Err(e),
    };
    // listxattr 和 getxattr 之间属性可能被别的进程删掉，ENOATTR 当作它本来就不存在
    let fetch = |name: &[u8]| -> std::io::Result<Option<Vec<u8>>> {
        match sys::get(path, name) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.raw_os_error() == Some(libc::ENOATTR) => Ok(None),
            Err(e) => Err(e),
        }
    };
    let subject = || path.display().to_string();
    let mut header_len = ATTR_ENTRIES_OFFSET;
    let mut data_len: u64 = 0;
    for name in names {
        match name.as_slice() {
            b"com.apple.FinderInfo" => {
                if let Some(v) = fetch(&name)? {
                    match <[u8; FINDER_INFO_LEN]>::try_from(v.as_slice()) {
                        // 全 0 的 Finder Info 等于没有，ditto 解包时也会忽略
                        Ok(fi) if fi != [0; FINDER_INFO_LEN] => meta.finder_info = Some(fi),
                        Ok(_) => {}
                        Err(_) => warnings.push(Warning::new(WarningKind::MacFinderInfoInvalid, subject())),
                    }
                }
            }
            b"com.apple.ResourceFork" => {
                if sys::size(path, &name)? > MAX_FORK_BYTES {
                    warnings.push(Warning::new(WarningKind::MacResourceForkTooLarge, subject()));
                    continue;
                }
                if let Some(v) = fetch(&name)? {
                    meta.resource_fork = v;
                }
            }
            b"com.apple.quarantine" | b"com.apple.decmpfs" => {}
            _ => {
                let display = String::from_utf8_lossy(&name).into_owned();
                if name.len() + 1 > u8::MAX as usize {
                    warnings.push(Warning::with_detail(WarningKind::MacXattrNameTooLong, subject(), display));
                    continue;
                }
                let size = sys::size(path, &name)?;
                // 单值超过 MAX_FORK_BYTES、条目表超 64 KB 硬上限、值区偏移超出 u32：都只能丢掉这个属性
                let entry_len = attr_entry_len(name.len() + 1);
                if size > MAX_FORK_BYTES || header_len + entry_len > ATTR_MAX_HDR_SIZE || data_len + size > u32::MAX as u64 - ATTR_MAX_HDR_SIZE as u64 {
                    warnings.push(Warning::with_detail(WarningKind::MacXattrTooLarge, subject(), display));
                    continue;
                }
                let Some(value) = fetch(&name)? else { continue };
                header_len += entry_len;
                data_len += value.len() as u64;
                meta.xattrs.push((name, value));
            }
        }
    }
    // APFS 的 listxattr 本来就按名字排序，这里再排一次保证不同文件系统上输出也一致
    meta.xattrs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(meta)
}

/// 非 macOS 没有 Finder 信息和资源分支，「Mac 专用」预设不产生任何 `._` 条目。
#[cfg(not(target_os = "macos"))]
pub fn read(_path: &Path, _warnings: &mut Vec<Warning>) -> std::io::Result<MacMetadata> {
    Ok(MacMetadata::default())
}

/// libc 的 listxattr / getxattr 薄封装，一律带 XATTR_NOFOLLOW（符号链接本身不处理，也不跟过去）。
#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::{c_char, c_void, CString};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// 路径转 C 字符串。
    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径含 NUL"))
    }

    /// 属性名转 C 字符串。
    fn c_name(name: &[u8]) -> io::Result<CString> {
        CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "属性名含 NUL"))
    }

    /// 列出全部扩展属性名（原始字节，不含 NUL）。
    /// 先问大小再读，两次调用之间属性可能变多（ERANGE），所以循环重试。
    pub fn list(path: &Path) -> io::Result<Vec<Vec<u8>>> {
        let c = c_path(path)?;
        loop {
            // SAFETY：传空缓冲区只查询需要的字节数；c 在调用期间有效
            let need = unsafe { libc::listxattr(c.as_ptr(), std::ptr::null_mut(), 0, libc::XATTR_NOFOLLOW) };
            if need < 0 {
                return Err(io::Error::last_os_error());
            }
            if need == 0 {
                return Ok(Vec::new());
            }
            let mut buf = vec![0u8; need as usize];
            // SAFETY：buf 的长度就是传入的 size，内核最多写这么多
            let got = unsafe { libc::listxattr(c.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len(), libc::XATTR_NOFOLLOW) };
            if got < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::ERANGE) {
                    continue;
                }
                return Err(e);
            }
            buf.truncate(got as usize);
            return Ok(buf.split(|&b| b == 0).filter(|s| !s.is_empty()).map(<[u8]>::to_vec).collect());
        }
    }

    /// 单个属性当前的大小，不读内容（资源分支可能很大，先看大小再决定读不读）。
    pub fn size(path: &Path, name: &[u8]) -> io::Result<u64> {
        let (c, n) = (c_path(path)?, c_name(name)?);
        // SAFETY：空缓冲区 + size 0 只返回大小；指针在调用期间有效
        let need = unsafe { libc::getxattr(c.as_ptr(), n.as_ptr(), std::ptr::null_mut(), 0, 0, libc::XATTR_NOFOLLOW) };
        if need < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(need as u64)
    }

    /// 读取单个属性的全部内容；大小变化导致 ERANGE 就重试。
    pub fn get(path: &Path, name: &[u8]) -> io::Result<Vec<u8>> {
        let (c, n) = (c_path(path)?, c_name(name)?);
        loop {
            // SAFETY：同 size
            let need = unsafe { libc::getxattr(c.as_ptr(), n.as_ptr(), std::ptr::null_mut(), 0, 0, libc::XATTR_NOFOLLOW) };
            if need < 0 {
                return Err(io::Error::last_os_error());
            }
            if need == 0 {
                return Ok(Vec::new());
            }
            let mut buf = vec![0u8; need as usize];
            // SAFETY：buf 长度即传入的 size；position 0 表示从头读（资源分支支持分段读，这里一次读完）
            let got = unsafe { libc::getxattr(c.as_ptr(), n.as_ptr(), buf.as_mut_ptr() as *mut c_void, buf.len(), 0, libc::XATTR_NOFOLLOW) };
            if got < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::ERANGE) {
                    continue;
                }
                return Err(e);
            }
            buf.truncate(got as usize);
            return Ok(buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 大端 u32 读取。
    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    /// 照抄 ditto 对一个带 3 个 xattr 的文件写出的数字：名字长 21/16/17（含 NUL）→ 条目 32/28/28，
    /// data_start 208，值 11+3000+1 → total_size 3220；资源分支紧随其后。
    #[test]
    fn layout_matches_ditto_numbers() {
        let meta = MacMetadata {
            finder_info: None,
            xattrs: vec![
                (b"com.apple.provenance".to_vec(), vec![1u8; 11]),
                (b"com.example.big".to_vec(), vec![b'A'; 3000]),
                (b"com.example.only".to_vec(), b"y".to_vec()),
            ],
            resource_fork: b"RS".to_vec(),
        };
        let b = encode(&meta);
        assert_eq!(be32(&b, 0), 0x0005_1607);
        assert_eq!(be32(&b, 4), 0x0002_0000);
        assert_eq!(&b[8..24], b"Mac OS X        ");
        assert_eq!(&b[24..26], &[0, 2]);
        assert_eq!((be32(&b, 26), be32(&b, 30), be32(&b, 34)), (9, 50, 3170));
        assert_eq!((be32(&b, 38), be32(&b, 42), be32(&b, 46)), (2, 3220, 2));
        assert_eq!(&b[50..82], &[0u8; 32]);
        assert_eq!(&b[82..84], &[0, 0]);
        assert_eq!(&b[84..88], b"ATTR");
        assert_eq!((be32(&b, 92), be32(&b, 96), be32(&b, 100)), (3220, 208, 3012));
        assert_eq!(&b[116..120], &[0, 0, 0, 3]);
        // 第 2 条：@152，值偏移 219、长 3000、namelen 16、名字后 NUL，补齐到 180
        assert_eq!((be32(&b, 152), be32(&b, 156)), (219, 3000));
        assert_eq!(&b[160..163], &[0, 0, 16]);
        assert_eq!(&b[163..179], b"com.example.big\0");
        assert_eq!(b[179], 0);
        assert_eq!(&b[208..219], &[1u8; 11]);
        assert_eq!(&b[3219..3220], b"y");
        assert_eq!(&b[3220..], b"RS");
        assert_eq!(b.len(), 3222);
    }

    /// 只有 Finder 信息：ATTR 头仍然写，num_attrs 0，数据区空，资源分支偏移 = 120。
    #[test]
    fn finder_info_only() {
        let mut fi = [0u8; 32];
        fi[8] = 0x04;
        let b = encode(&MacMetadata { finder_info: Some(fi), ..Default::default() });
        assert_eq!(b.len(), 120);
        assert_eq!((be32(&b, 30), be32(&b, 34)), (50, 70));
        assert_eq!((be32(&b, 42), be32(&b, 46)), (120, 0));
        assert_eq!(&b[50..82], &fi);
        assert_eq!((be32(&b, 92), be32(&b, 96), be32(&b, 100)), (120, 120, 0));
    }

    #[test]
    fn rel_mapping() {
        assert_eq!(apple_double_rel("f.txt"), "__MACOSX/._f.txt");
        assert_eq!(apple_double_rel("a/b/c.txt"), "__MACOSX/a/b/._c.txt");
        assert_eq!(apple_double_rel("dir"), "__MACOSX/._dir");
    }
}
