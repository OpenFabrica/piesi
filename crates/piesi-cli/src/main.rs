//! `piesi` 命令行入口。

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Piesi 命令行。
#[derive(Parser)]
#[command(name = "piesi", version, about = "Piesi 压缩/解压命令行")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// 子命令。
#[derive(Subcommand)]
enum Cmd {
    /// 识别格式
    Detect { path: PathBuf },
    /// 列出压缩包内容
    List {
        path: PathBuf,
        /// 以 JSON 输出（语料回归用）
        #[arg(long)]
        json: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Detect { path } => {
            match piesi_core::detect_format(&path)? {
                Some(f) => println!("{}", f.display_name()),
                None => println!("unknown"),
            }
        }
        Cmd::List { path, json } => {
            let reader = piesi_core::open(&path)?;
            let entries = reader.entries();
            if json {
                println!("{}", serde_json::to_string_pretty(entries)?);
            } else {
                for e in entries {
                    println!("{:>12}  {}", e.size, piesi_core::encoding::decode_name(e, piesi_core::encoding::NameEncoding::Utf8));
                }
            }
        }
    }
    Ok(())
}
