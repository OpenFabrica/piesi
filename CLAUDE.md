# Piesi

现代原生 Mac 压缩/解压应用，Rust 核心可跨平台，开源（MIT OR Apache-2.0）。
设计方案见 `docs/design.md`，以它为准；本文件只写工作规则。

## 仓库结构

- `crates/piesi-core`：引擎（格式驱动、编码识别、安全解压、压缩计划）。
- `crates/piesi-ffi`：UniFFI 导出层，只放导出函数与 record/enum，不放逻辑。
- `crates/piesi-cli`：命令行 `piesi`，也是语料回归测试入口。
- `tests/corpus`：多来源、多编码的测试压缩包和 `manifest.json`；大文件在 `generated/`（不入库，脚本重建）。
- `apple/`：macOS app（SwiftUI + AppKit）与 Quick Look 扩展，工程由 `xcodegen` 生成。
- `scripts/`：语料生成、基准、构建 XCFramework、打包公证。

## 常用命令

- 构建与测试核心：`cargo build --workspace` / `cargo test --workspace`
- 生成语料：`python3 -I scripts/gen_corpus.py tests/corpus`
- 端到端冒烟（与 7zz/unzip/bsdtar 交叉验证）：`scripts/smoke.sh`
- 性能基准（对比 7zz，结果追加到 bench/results.md）：`scripts/bench.sh [mixed|big|all]`
- 构建 XCFramework：`scripts/build-core.sh`
- 生成 Xcode 工程：`cd apple && xcodegen generate`
- 打包签名公证：`scripts/package.sh`（签名身份与公证 profile 见 `~/.claude/docs/apple-signing.md`）

## 工作规则

- 所有开发在 git worktree 里做（当前 `../Piesi-mvp`，分支 `feat/mvp`），主工作树只做合并。
- 注释用中文；每个新增或修改的函数、类型、模块顶部一行说明；复杂逻辑上方写「为什么这样写」。
- 解析不可信压缩包的代码一律 Rust，不引入 C 解析器（unrar、liblzma 只做编解码，不做容器解析）。
- 新依赖先在 `Cargo.toml` 的 `[workspace.dependencies]` 声明并锁版本；`zip` crate 只用来写，不用来读。
- 语料回归（`cargo test -p piesi-cli --test corpus`）是合并门禁，改格式驱动必须补语料。
- 版本号与发版规则：@docs/release.md
