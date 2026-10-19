# Piesi 对 sevenz-rust2 的补丁说明

## 来源

- 上游：[sevenz-rust2](https://github.com/hasenbanck/sevenz-rust2) 0.24.1，Apache-2.0，crates.io 发布版（git `ab17c9b9184df49941ffcb0df4f41cb06d106ef8`）。
- 本目录是 `~/.cargo/registry/src/*/sevenz-rust2-0.24.1` 的副本，根 `Cargo.toml` 用 `[patch.crates-io]` 指到这里。
- 去掉了和构建无关的东西：`tests/`（含 1.5 MB 二进制夹具）、`examples/`、`.github/`、`Cargo.lock`、`Cargo.toml.orig`、`.cargo_vcs_info.json`、`.gitattributes`、`.gitignore`。
- `Cargo.toml` 按上游 `Cargo.toml.orig` 重写：去掉 `[[example]]` 表和 `[dev-dependencies]`，加 `publish = false`，加 `liblzma` feature 与可选依赖 `liblzma`。

## 改了什么

| 文件 | 改动 |
|---|---|
| `Cargo.toml` | 新增 feature `liblzma = ["dep:liblzma"]`，可选依赖 `liblzma = "0.4"` |
| `src/codec.rs` | 挂 `pub(crate) mod liblzma`（feature 门控） |
| `src/codec/liblzma.rs` | **新文件**：liblzma 的 LZMA2 单线程 / 多线程编码器与解码器，以及两者共用的有序线程池 |
| `src/decoder.rs` | `ID_LZMA2` 分支：feature 开时用 `Lzma2Decoder` / `Lzma2DecoderMt`；枚举变体 `Lzma2` / `Lzma2Mt` 名字不变，只换装载类型，`Read` 分发不用改。另加 `clamp_dict_to_output`（不受 feature 门控）：LZMA / LZMA2 的解码字典按本 coder 的解压大小收紧（下限 4096），防恶意头用 1 字节属性让解码器开场分配 4 GB；PPMd 的模型内存不能这样收紧（改了模型重启点就解不出来），只能靠内存上限 |
| `src/reader.rs` | `MAX_MEM_LIMIT_KB` 从无限改成 2 GiB：PPMd 模型内存只能封顶，防恶意头换几 GB 分配；LZMA2 字典已收紧，不受影响 |
| `src/encoder.rs` | `ID_LZMA2` 分支：feature 开时用 `Lzma2Encoder` / `Lzma2EncoderMt`；枚举变体同理，`write(&[])` 触发 `finish()` 的隐式约定照旧 |

feature 关掉时所有改动都被 `#[cfg]` 掉，行为与上游完全一致。LZMA1、BCJ*、Delta、BCJ2 仍走 lzma-rust2，没有动。

看完整 diff：

```sh
diff -ruN ~/.cargo/registry/src/index.crates.io-*/sevenz-rust2-0.24.1/src vendor/sevenz-rust2/src
```

## 为什么

P0 基准（Apple M5 Pro 18 核，2 GB 单文件）：lzma-rust2 的纯 Rust LZMA2 压缩比 7zz 慢 2 倍（48 s vs 24 s），
解压慢 2.5 倍（3.7 s vs 1.5 s，已经用了它的多线程读取器）。设计方案 5.1 的兜底就是把 LZMA2 编解码换成 liblzma：
7z 的 LZMA2 packed stream 就是 liblzma raw LZMA2 过滤器的位流，两边可以直接互换；liblzma 本来就是 tar.xz 的依赖，不新增 C 代码。
容器解析仍然全是 Rust，C 只做编解码，符合「不引入 C 解析器」的规则。

## 实现要点

- **编码（多线程）**：照 7-Zip 的做法，输入按 `Lzma2Options` 的 `chunk_size`（不小于字典）切块，每块独立跑一个 raw LZMA2 编码器，
  块首天然带字典重置；去掉每块末尾的 0x00 结束标记后按序拼接，整条流最后补一个 0x00。`threads` ≤ 1 走单流。
  `chunk_size` 为 None 时报错，与上游 `Lzma2WriterMt` 一致。在飞的块数不超过线程数（内存 ≈ 线程数 × chunk_size）。
- **解码（多线程）**：按「字典重置」chunk（控制字节 ≥ 0xE0 或 == 0x01）切成独立单元，每个单元交给一个独立的 raw 解码器并行解，
  按序交出，输出与单线程逐字节一致。7-Zip 多线程写出的流每个块都是这样的单元。
  - 解压后小于 1 MiB 的流直接单线程，不起线程（非 solid 包每个文件一个块，别为小文件起线程）。
  - 内存：在飞单元数 ≤ 线程数，且在飞单元解压后总字节 ≤ 线程数 × 64 MiB；单个单元解压后超过 256 MiB（7-Zip 的块上限）
    就不再整单元进内存，剩余部分切回单线程流式解码（`Lzma2DecoderMt::start_fallback`）。
  - 线程按需起，池被 drop 就退出；没有 0x00 结束标记就 EOF 的流报 `UnexpectedEof`，与单线程 liblzma 一致。
  - worker 里的任务用 `catch_unwind` 包住：池自己持有一份结果通道发送端，worker panic 退出会让主线程永远等不到那个序号；
    panic 变成 `io::Error::other("LZMA2 worker panicked")` 按序发回。
- **参数映射**：lzma-rust2 的 `LzmaOptions`（dict_size / lc / lp / pb / mode / nice_len / mf / depth_limit）逐项翻译成 liblzma 的
  `LzmaOptions`，Hc4 → HashChain4、Bt4 → BinaryTree4、Fast/Normal 同名。`preset_dict` 忽略（7z 没有预置字典）。
- **flush**：单线程编码器用 LZMA2 的 SyncFlush（raw 编码器不支持 FullFlush，liblzma crate 自带的 `write::XzEncoder::flush` 会报错，
  所以没有复用它）；多线程编码器和上游一样把半截块也派出去。
- **速度前提**：liblzma-sys 自带的 config.h 没开 `TUKLIB_FAST_UNALIGNED_ACCESS`，编码器会退化成逐字节比较匹配，慢 2.5 倍。
  根目录 `.cargo/config.toml` 用 `CFLAGS_<目标>` 给 C 编译器补了这个宏；换构建环境时要确认它仍然生效
  （128 MB 单线程 level 5 约 4 s，没生效约 12 s）。
- **参数在哪定**：字典 / 块 / nice_len / 线程数的策略在 `crates/piesi-core/src/create/sevenz_writer.rs`，本 crate 只照单执行。

## 验证

- `cargo test -p sevenz-rust2 --lib`：`codec::liblzma::tests` 四个回环 / 边界 / 截断用例。
- `cargo test -p piesi-core`：7z 驱动、写入器（含 7zz 交叉校验）、健壮性（截断 / 翻转夹具不 panic）。
- 上游完整测试集：把 registry 里的源码整体拷到临时目录，覆盖上本目录的 `Cargo.toml`、`src/codec.rs`、`src/decoder.rs`、
  `src/encoder.rs`、`src/codec/liblzma.rs`，补回 `[dev-dependencies]`（rand、tempfile）和空的 `[workspace]`，
  跑 `cargo test --features liblzma,deflate,zstd`。

## 升级上游时怎么重打

1. `cargo fetch` 新版本后，把 `~/.cargo/registry/src/*/sevenz-rust2-<新版本>/src`、`LICENSE`、`README.md`、`CHANGELOG.md`、`rustfmt.toml` 拷过来覆盖。
2. 按新版本的 `Cargo.toml.orig` 重写 `Cargo.toml`，保留上面说的改动（`publish`、`liblzma` feature 与可选依赖，去掉 example / dev-dependencies）。
3. 重新套上 `src/codec.rs`、`src/decoder.rs`、`src/encoder.rs` 的三处小改动（都带「Piesi 补丁」注释，先用上一步的 diff 命令对照）。
   `src/codec/liblzma.rs` 只依赖 `lzma_rust2::{LzmaOptions, Lzma2Options, EncodeMode, MfType}` 的公开字段和 liblzma crate，
   上游没改这些就不用动。
4. 更新本文件的版本号和 git sha，更新根 `Cargo.toml` 里 `sevenz-rust2` 的版本，`cargo update -p sevenz-rust2`。
5. 跑上面「验证」里的三组测试，再跑一次 `scripts/bench.sh big` 确认性能没退。
