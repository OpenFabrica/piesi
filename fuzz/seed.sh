#!/usr/bin/env bash
# fuzz/seed.sh —— 用测试夹具生成种子语料：每个文件前面加一个「扩展名选择字节」（和 open_any.rs 的 EXTS 顺序一致）。
# 用法：fuzz/seed.sh（在仓库任意位置执行）；输出到 fuzz/corpus/open_any/。
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=fuzz/corpus/open_any
mkdir -p "$OUT"
# 和 open_any.rs 里 EXTS 的顺序一一对应
EXTS=(zip 7z rar tar tar.gz tar.xz tar.zst tar.bz2 gz xz zst bz2 tgz jar zip.001 7z.001)
n=0
while IFS= read -r f; do
  name=$(basename "$f")
  idx=-1
  for i in "${!EXTS[@]}"; do
    case "$name" in *".${EXTS[$i]}") idx=$i ;; esac
  done
  [ "$idx" -ge 0 ] || continue
  # 超过 256 KB 的夹具跳过：libFuzzer 的 max_len 以内才有用
  [ "$(stat -f %z "$f")" -le 262144 ] || continue
  { printf "\\x$(printf %02x "$idx")"; cat "$f"; } > "$OUT/$(printf %03d "$n")-$name"
  n=$((n + 1))
done < <(find crates/piesi-core/tests/fixtures tests/corpus -type f \( -name "*.zip" -o -name "*.7z" -o -name "*.rar" -o -name "*.tar" -o -name "*.gz" -o -name "*.xz" -o -name "*.zst" -o -name "*.bz2" -o -name "*.tgz" -o -name "*.001" \) | sort)
echo "种子 ${n} 个 → ${OUT}"
