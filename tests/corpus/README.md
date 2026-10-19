# 测试语料

- `encoding/*.zip`：文件名编码夹具，由 `python3 -I scripts/gen_corpus.py tests/corpus` 生成（幂等，可重跑）。
  覆盖 编码 {UTF-8, GB18030, Big5, Shift_JIS, EUC-KR, CP1251} × 标志 {无, bit 11, 0x7075} × version made by {DOS, Unix}，
  以及 Finder 风格、WinRAR 风格、bit 11 撒谎、新旧混合、2 字名、GBK 撞车合法 UTF-8、CP866、CP1252、纯 ASCII。
- `manifest.json`：每个包一条 `{file, expected_encoding, expected_source, expected_names, ambiguous, hints, note, expected_junk?}`，
  由脚本整体生成，不要手改；要加包就改脚本。
- `raw_names.json`：每个包的条目原始字节与标志，`crates/piesi-core/tests/encoding_corpus.rs` 用它绕过 zip 读取器直接构造 `Entry`。
- `generated/`：大体积语料（不入库，脚本重建）。
