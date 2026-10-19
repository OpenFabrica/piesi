# Piesi

默认干净、中文不乱码、能像 Finder 一样浏览压缩包的现代原生 Mac 压缩工具。
Rust 核心可跨平台，开源（MIT OR Apache-2.0）。

- 设计方案：`docs/design.md`
- 开发规则：`CLAUDE.md`
- 发版规范：`docs/release.md`

## 现状（P1 进行中）

核心引擎已可用，命令行 `piesi` 可以：

```sh
piesi detect 包.zip                 # 识别格式
piesi list 包.zip                   # 列目录，自动识别文件名编码；--encoding gb18030 可手动指定
piesi list 包.zip --json            # 机器可读输出（含 id、编码置信度）
piesi x 包.zip                      # 解压到包旁边，智能建文件夹、冲突改名、跳过 __MACOSX/.DS_Store
piesi x 包.7z -o 目标目录 -p 密码    # 指定目录与密码；--id 3 --id 7 只解指定条目
piesi t 包.zip                      # 校验（解码 + CRC，不写盘），损坏或密码错误时退出码非 0
piesi a 输出.zip 目录 文件…          # 创建 zip（UTF-8 + bit 11 + NFC，多线程）
piesi a 输出.7z --level 7 目录      # 创建 7z（多线程 LZMA2，solid 块 128 MB）
piesi a 输出.tar.zst 目录           # tar.gz / tar.xz / tar.zst
```

支持读取：zip（zip64、AES、ZipCrypto、`.zip.001`、`.z01` 分卷）、7z（BCJ/BCJ2/ARM64、solid、头加密、`.7z.001`）、rar（v4/v5、分卷、加密）、tar 及 tar.gz/bz2/xz/zst、单文件 gz/bz2/xz/zst。
支持创建：zip、7z、tar.gz、tar.xz、tar.zst。macOS app 与 Quick Look 扩展在 `apple/`。

## 开发

```sh
cargo test --workspace            # 单元 + 集成 + 语料回归
scripts/smoke.sh                  # 端到端：与 7zz / unzip / bsdtar 交叉验证
scripts/bench.sh mixed            # 与 7zz 对比耗时与峰值内存（结果在 bench/results.md）
scripts/build-core.sh --arch native && (cd apple && xcodegen generate && xcodebuild -scheme Piesi build)
```
