#!/usr/bin/env bash
# P0 基准：对比 piesi 与 7zz 的压缩/解压耗时和峰值内存。
# 数据集：mixed（约 1 GB 的混合小文件目录）与 big（单个约 2 GB 文件），生成一次后缓存在 bench/data。
# 用法：scripts/bench.sh [mixed|big|all]（默认 all）；结果追加到 bench/results.md。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
DATA="$ROOT/bench/data"
OUT="$ROOT/bench/out"
PIESI="$ROOT/target/release/piesi"
SEVENZ=${SEVENZ:-/opt/homebrew/bin/7zz}
WHICH=${1:-all}
mkdir -p "$DATA" "$OUT"

# 混合数据：源码 + 文本 + 已压缩的二进制，各占一部分，贴近真实「项目目录」
gen_mixed() {
  local d="$DATA/mixed"
  [ -d "$d" ] && return
  echo "生成 mixed 数据集（约 1 GB）…"
  mkdir -p "$d"
  python3 -I - "$d" <<'PY'
import os, random, sys
root = sys.argv[1]
rnd = random.Random(42)
words = [w.strip() for w in open('/usr/share/dict/words', errors='ignore') if w.strip()] or ['lorem']
total = 0
i = 0
while total < 1_000_000_000:
    sub = os.path.join(root, f"dir{i % 97:03d}", f"sub{i % 7}")
    os.makedirs(sub, exist_ok=True)
    kind = rnd.random()
    if kind < 0.6:   # 文本/源码类，高可压缩
        n = rnd.randint(1_000, 200_000)
        data = ' '.join(rnd.choice(words) for _ in range(n // 6)).encode()
    elif kind < 0.9: # 半结构化，中等可压缩
        n = rnd.randint(10_000, 2_000_000)
        data = bytes(rnd.getrandbits(4) for _ in range(n))
    else:            # 已压缩/随机，几乎不可压缩
        n = rnd.randint(100_000, 20_000_000)
        data = os.urandom(n)
    with open(os.path.join(sub, f"f{i:06d}.dat"), 'wb') as f:
        f.write(data)
    total += len(data)
    i += 1
print(f"files={i} bytes={total}")
PY
}

# 单个大文件：中等可压缩，避免纯随机让所有工具都只是在拷贝
gen_big() {
  local f="$DATA/big.bin"
  [ -f "$f" ] && return
  echo "生成 big 数据集（约 2 GB）…"
  python3 -I - "$f" <<'PY'
import os, random, sys
rnd = random.Random(7)
chunk = bytes(rnd.getrandbits(5) for _ in range(1 << 20))
with open(sys.argv[1], 'wb') as f:
    for _ in range(2048):
        f.write(chunk[rnd.randint(0, 1000):] + os.urandom(4096))
PY
}

# 跑一条命令并记录：耗时（秒）、峰值内存（MB）
run() {
  local label=$1; shift
  local t0 t1 mem
  t0=$(python3 -c 'import time;print(time.time())')
  /usr/bin/time -l "$@" 2> "$OUT/time.txt" >/dev/null || { echo "失败：$label"; cat "$OUT/time.txt"; return 1; }
  t1=$(python3 -c 'import time;print(time.time())')
  mem=$(awk '/maximum resident set size/ {printf "%.0f", $1/1048576}' "$OUT/time.txt")
  printf "| %s | %.1f | %s |\n" "$label" "$(echo "$t1 - $t0" | bc)" "$mem" | tee -a "$ROOT/bench/results.md"
}

cargo build --release -p piesi-cli >/dev/null
{
  echo
  echo "## $(date '+%Y-%m-%d %H:%M') · $(sysctl -n machdep.cpu.brand_string) · $(sysctl -n hw.ncpu) 核"
  echo
  echo "| 任务 | 秒 | 峰值内存 MB |"
  echo "|---|---|---|"
} >> "$ROOT/bench/results.md"

bench_set() {
  local name=$1 src=$2
  rm -rf "$OUT"/*
  run "7zz 7z 压缩 $name (mx5)"   "$SEVENZ" a -t7z -mx5 -bd -y "$OUT/7zz-$name.7z" "$src"
  run "piesi 7z 压缩 $name (level5)" "$PIESI" a --format 7z --level 5 "$OUT/piesi-$name.7z" "$src"
  run "7zz 7z 解压 $name (7zz 产物)"   "$SEVENZ" x -bd -y -o"$OUT/x1" "$OUT/7zz-$name.7z"
  run "piesi 7z 解压 $name (7zz 产物)" "$PIESI" x -o "$OUT/x2" "$OUT/7zz-$name.7z"
  run "7zz zip 压缩 $name"   "$SEVENZ" a -tzip -mx5 -bd -y "$OUT/7zz-$name.zip" "$src"
  run "piesi zip 压缩 $name" "$PIESI" a --format zip --level 5 "$OUT/piesi-$name.zip" "$src"
  run "7zz zip 解压 $name"   "$SEVENZ" x -bd -y -o"$OUT/x3" "$OUT/7zz-$name.zip"
  run "piesi zip 解压 $name" "$PIESI" x -o "$OUT/x4" "$OUT/7zz-$name.zip"
  ls -l "$OUT"/*.7z "$OUT"/*.zip | awk '{print "| 大小 " $NF " | " $5 " | |"}' >> "$ROOT/bench/results.md"
}

case "$WHICH" in
  mixed) gen_mixed; bench_set mixed "$DATA/mixed" ;;
  big)   gen_big;   bench_set big "$DATA/big.bin" ;;
  all)   gen_mixed; gen_big; bench_set mixed "$DATA/mixed"; bench_set big "$DATA/big.bin" ;;
  *) echo "用法：$0 [mixed|big|all]"; exit 2 ;;
esac
echo "结果已追加到 bench/results.md"
