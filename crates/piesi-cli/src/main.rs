//! `piesi` 命令行入口：识别、列目录、解压、压缩。也是语料回归和基准的执行工具。

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use clap::{Parser, Subcommand, ValueEnum};
use piesi_core::create::{create_archive, CreateFormat, CreateOptions};
use piesi_core::encoding::NameEncoding;
use piesi_core::facade::Archive;
use piesi_core::plan::{ConflictPolicy, ExtractOptions};
use piesi_core::progress::{CancelToken, OpContext, Progress, ProgressEvent};

/// Piesi 命令行。
#[derive(Parser)]
#[command(name = "piesi", version, about = "Piesi 压缩/解压命令行")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// 命令行可选的文件名编码。
#[derive(Clone, Copy, ValueEnum)]
enum EncodingArg {
    Utf8,
    Gb18030,
    Big5,
    ShiftJis,
    EucKr,
    Cp1251,
    Cp866,
    Cp1252,
}

impl From<EncodingArg> for NameEncoding {
    fn from(e: EncodingArg) -> Self {
        match e {
            EncodingArg::Utf8 => NameEncoding::Utf8,
            EncodingArg::Gb18030 => NameEncoding::Gb18030,
            EncodingArg::Big5 => NameEncoding::Big5,
            EncodingArg::ShiftJis => NameEncoding::ShiftJis,
            EncodingArg::EucKr => NameEncoding::EucKr,
            EncodingArg::Cp1251 => NameEncoding::Cp1251,
            EncodingArg::Cp866 => NameEncoding::Cp866,
            EncodingArg::Cp1252 => NameEncoding::Cp1252,
        }
    }
}

/// 子命令。
#[derive(Subcommand)]
enum Cmd {
    /// 识别格式
    Detect { path: PathBuf },
    /// 列出压缩包内容
    List {
        path: PathBuf,
        /// 指定文件名编码（默认自动识别）
        #[arg(long, value_enum)]
        encoding: Option<EncodingArg>,
        /// 密码
        #[arg(short, long)]
        password: Option<String>,
        /// 以 JSON 输出（语料回归用）
        #[arg(long)]
        json: bool,
    },
    /// 解压
    #[command(alias = "extract")]
    X {
        path: PathBuf,
        /// 目标目录（默认压缩包所在目录）
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, value_enum)]
        encoding: Option<EncodingArg>,
        #[arg(short, long)]
        password: Option<String>,
        /// 同名文件直接覆盖（默认改名）
        #[arg(long, conflicts_with = "skip")]
        overwrite: bool,
        /// 同名文件跳过（默认改名）
        #[arg(long)]
        skip: bool,
        /// 不自动建顶层文件夹
        #[arg(long)]
        no_smart_folder: bool,
        /// 保留 __MACOSX、.DS_Store 等垃圾
        #[arg(long)]
        keep_junk: bool,
        /// 只解压这些条目 id（`list --json` 里的 id）
        #[arg(long = "id")]
        ids: Vec<u32>,
        /// 不打印进度
        #[arg(short, long)]
        quiet: bool,
    },
    /// 校验压缩包（解码 + CRC，不写盘）
    #[command(alias = "test")]
    T {
        path: PathBuf,
        #[arg(long, value_enum)]
        encoding: Option<EncodingArg>,
        #[arg(short, long)]
        password: Option<String>,
        /// 只校验这些条目 id（`list --json` 里的 id）
        #[arg(long = "id")]
        ids: Vec<u32>,
        /// 不打印进度
        #[arg(short, long)]
        quiet: bool,
    },
    /// 创建压缩包
    #[command(alias = "add")]
    A {
        /// 输出文件（扩展名决定默认格式）
        archive: PathBuf,
        /// 要压缩的文件或目录
        paths: Vec<PathBuf>,
        /// 格式：zip / 7z / tgz / txz / tzst（默认按扩展名）
        #[arg(long)]
        format: Option<String>,
        /// 压缩级别 0-9（0 = 仅存储）
        #[arg(long, default_value_t = 5)]
        level: u8,
        /// 密码：zip 用 WinZip AES-256（AE-1 / AE-2，7-Zip / WinZip / Keka 可解），7z 用 AES-256
        #[arg(short, long)]
        password: Option<String>,
        /// 7z：连文件名一起加密
        #[arg(long)]
        encrypt_names: bool,
        /// 保留 .DS_Store、__MACOSX 等垃圾
        #[arg(long)]
        keep_junk: bool,
        /// 文件名改成 Windows 安全名
        #[arg(long)]
        windows_safe: bool,
        /// 「Mac 专用」：xattr / 资源分支 / Finder 信息写成 __MACOSX/._ AppleDouble 条目（只对 zip 生效）
        #[arg(long)]
        mac_metadata: bool,
        /// 线程数（默认 CPU 核数）
        #[arg(long, default_value_t = 0)]
        threads: usize,
        /// 7z solid 块上限（MB，0 = 非 solid）
        #[arg(long, default_value_t = 128)]
        solid_mb: u64,
        #[arg(short, long)]
        quiet: bool,
    },
}

/// 由 --format 或扩展名决定创建格式。
fn pick_format(explicit: Option<&str>, archive: &std::path::Path) -> anyhow::Result<CreateFormat> {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let by_ext = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(CreateFormat::TarGz)
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        Some(CreateFormat::TarXz)
    } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        Some(CreateFormat::TarZst)
    } else if name.ends_with(".7z") {
        Some(CreateFormat::SevenZ)
    } else if name.ends_with(".zip") {
        Some(CreateFormat::Zip)
    } else {
        None
    };
    match explicit.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("zip") => Ok(CreateFormat::Zip),
        Some("7z") => Ok(CreateFormat::SevenZ),
        Some("tgz") | Some("tar.gz") => Ok(CreateFormat::TarGz),
        Some("txz") | Some("tar.xz") => Ok(CreateFormat::TarXz),
        Some("tzst") | Some("tar.zst") => Ok(CreateFormat::TarZst),
        Some(other) => anyhow::bail!("不支持的格式：{other}"),
        None => by_ext.ok_or_else(|| anyhow::anyhow!("无法从扩展名判断格式，请用 --format")),
    }
}

/// 把进度打到 stderr 同一行。
struct StderrProgress {
    last: Mutex<String>,
}

impl Progress for StderrProgress {
    fn report(&self, e: &ProgressEvent) {
        let pct = match e.total_bytes {
            Some(t) if t > 0 => format!("{:>3}%", e.done_bytes * 100 / t),
            _ => "  --".to_owned(),
        };
        let line = format!("{pct}  {}  {}", human(e.done_bytes), e.current.as_deref().unwrap_or(""));
        let mut last = self.last.lock().unwrap();
        if *last != line {
            eprint!("\r\x1b[2K{line}");
            let _ = std::io::stderr().flush();
            *last = line;
        }
    }
}

/// 字节数转人类可读。
fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Detect { path } => match piesi_core::detect_format(&path)? {
            Some(f) => println!("{}", f.display_name()),
            None => println!("unknown"),
        },
        Cmd::List { path, encoding, password, json } => {
            let mut archive = Archive::open_system(&path)?;
            if let Some(pw) = password.as_deref() {
                archive.set_password(Some(pw))?;
            }
            if let Some(enc) = encoding {
                archive.set_encoding(enc.into());
            }
            if json {
                let d = archive.detection();
                let rows: Vec<serde_json::Value> = archive
                    .entries()
                    .iter()
                    .zip(archive.names())
                    .map(|(e, n)| {
                        serde_json::json!({
                            "id": e.id, "name": n, "kind": e.kind, "size": e.size,
                            "packed": e.packed_size, "mtime": e.mtime, "crc32": e.crc32,
                            "encrypted": e.encrypted, "method": e.method.to_string(), "solid_group": e.solid_group,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "format": archive.format().display_name(),
                        "encoding": d.encoding, "confidence": d.confidence, "source": d.source, "ambiguous": d.ambiguous, "weak_encryption": archive.has_weak_encryption(),
                        "entries": rows,
                    }))?
                );
            } else {
                let d = archive.detection();
                eprintln!("格式 {}  编码 {:?}（{:?}，置信度 {:.2}{}）", archive.format().display_name(), d.encoding, d.source, d.confidence, if d.ambiguous { "，不确定" } else { "" });
                for (e, n) in archive.entries().iter().zip(archive.names()) {
                    let mark = match e.kind {
                        piesi_core::EntryKind::Dir => "/",
                        piesi_core::EntryKind::Symlink => "@",
                        piesi_core::EntryKind::File => "",
                    };
                    println!("{:>4}  {:>12}  {}{}{}", e.id, e.size, n, mark, if e.encrypted { "  [加密]" } else { "" });
                }
            }
        }
        Cmd::X { path, output, encoding, password, overwrite, skip, no_smart_folder, keep_junk, ids, quiet } => {
            let mut archive = Archive::open_system(&path)?;
            if let Some(pw) = password.as_deref() {
                archive.set_password(Some(pw))?;
            }
            if archive.has_weak_encryption() && !quiet {
                eprintln!("警告：此包用的是老式 ZipCrypto 加密，已知可被破解；只能读取，建议重新用 AES 打包");
            }
            if let Some(enc) = encoding {
                archive.set_encoding(enc.into());
            }
            let mut opts = ExtractOptions::beside_archive(&path);
            if let Some(o) = output {
                opts.dest = o;
            }
            opts.smart_folder = !no_smart_folder;
            opts.skip_junk = !keep_junk;
            opts.conflict = if overwrite {
                ConflictPolicy::Overwrite
            } else if skip {
                ConflictPolicy::Skip
            } else {
                ConflictPolicy::Rename
            };
            let d = archive.detection();
            if d.ambiguous && encoding.is_none() {
                eprintln!("提示：文件名编码不确定（当前按 {:?}），可用 --encoding 指定。", d.encoding);
            }
            let progress = StderrProgress { last: Mutex::new(String::new()) };
            let token = CancelToken::new();
            let silent = piesi_core::progress::NoProgress;
            let ctx = if quiet { OpContext::new(&silent, &token) } else { OpContext::new(&progress, &token) };
            let selected = if ids.is_empty() { None } else { Some(ids.as_slice()) };
            let outcome = archive.extract(selected, &opts, &ctx)?;
            if !quiet {
                eprint!("\r\x1b[2K");
            }
            eprintln!("已解压 {} 项到 {}（跳过 {}，垃圾 {}）", outcome.report.extracted, outcome.root.display(), outcome.report.skipped, outcome.skipped_junk);
            for w in &outcome.report.warnings {
                eprintln!("警告：{w}");
            }
        }
        Cmd::T { path, encoding, password, ids, quiet } => {
            let mut archive = Archive::open_system(&path)?;
            if let Some(pw) = password.as_deref() {
                archive.set_password(Some(pw))?;
            }
            if archive.has_weak_encryption() && !quiet {
                eprintln!("警告：此包用的是老式 ZipCrypto 加密，已知可被破解；只能读取，建议重新用 AES 打包");
            }
            if let Some(enc) = encoding {
                archive.set_encoding(enc.into());
            }
            let progress = StderrProgress { last: Mutex::new(String::new()) };
            let token = CancelToken::new();
            let silent = piesi_core::progress::NoProgress;
            let ctx = if quiet { OpContext::new(&silent, &token) } else { OpContext::new(&progress, &token) };
            let selected = if ids.is_empty() { None } else { Some(ids.as_slice()) };
            // 损坏、密码错误都会以 Err 冒出来，进程退出码非 0
            let report = archive.verify(selected, &ctx)?;
            if !quiet {
                eprint!("\r\x1b[2K");
            }
            eprintln!("校验通过：{} 项（跳过 {}）", report.extracted, report.skipped);
            for w in &report.warnings {
                eprintln!("警告：{w}");
            }
        }
        Cmd::A { archive, paths, format, level, password, encrypt_names, keep_junk, windows_safe, mac_metadata, threads, solid_mb, quiet } => {
            anyhow::ensure!(!paths.is_empty(), "至少给一个要压缩的路径");
            let opts = CreateOptions {
                format: pick_format(format.as_deref(), &archive)?,
                level,
                password,
                encrypt_names,
                skip_junk: !keep_junk,
                windows_safe,
                mac_metadata,
                threads,
                solid_block_bytes: solid_mb * 1024 * 1024,
            };
            let progress = StderrProgress { last: Mutex::new(String::new()) };
            let token = CancelToken::new();
            let silent = piesi_core::progress::NoProgress;
            let ctx = if quiet { OpContext::new(&silent, &token) } else { OpContext::new(&progress, &token) };
            let report = create_archive(&archive, &paths, &opts, &ctx)?;
            if !quiet {
                eprint!("\r\x1b[2K");
            }
            let ratio = if report.input_bytes > 0 { report.output_bytes as f64 * 100.0 / report.input_bytes as f64 } else { 0.0 };
            eprintln!("已创建 {}：{} 项，{} → {}（{:.1}%），跳过垃圾 {}", archive.display(), report.entries, human(report.input_bytes), human(report.output_bytes), ratio, report.skipped_junk);
            for w in &report.warnings {
                eprintln!("警告：{w}");
            }
        }
    }
    Ok(())
}
