#!/bin/sh
# 生成 RAR 驱动的测试夹具（每个 < 100 KB）。
# 依赖：RARLAB 的 rar 命令行 6.x（https://www.rarlab.com/rar/rarmacos-arm-624.tar.gz，解包后用
# RAR=/path/to/rar 指定；Homebrew 的 cask 因 Gatekeeper 已被禁用）、python3。
# 注意：rar 7.0 起去掉了 -ma4，不能再生成 RAR4 包，所以必须用 6.x。
# 源文件树内容是确定性的，tests/drivers_common.rs 里的 expected_* 必须与这里一致。
set -eu
cd "$(dirname "$0")"
RAR=${RAR:-rar}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# 用 python 生成确定性的源文件树（与 sevenz/tar 的 gen.sh 相同）
python3 -I - "$WORK/src" <<'PY'
import os, sys
root = sys.argv[1]
def lcg(n, seed):
    # 64 位 LCG，取高 8 位；Rust 测试里用同样的参数重新生成来校验内容
    x = seed
    out = bytearray()
    for _ in range(n):
        x = (x * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        out.append(x >> 56)
    return bytes(out)
os.makedirs(os.path.join(root, "dir"))
os.makedirs(os.path.join(root, "emptydir"))
os.makedirs(os.path.join(root, "bin"))
with open(os.path.join(root, "hello.txt"), "wb") as f:
    f.write(b"hello piesi\n")
with open(os.path.join(root, "dir", "nested.txt"), "wb") as f:
    f.write(b"nested " * 300 + b"\n")
open(os.path.join(root, "empty.txt"), "wb").close()
with open(os.path.join(root, "中文名.txt"), "wb") as f:
    f.write("中文内容\n".encode("utf-8"))
os.symlink("hello.txt", os.path.join(root, "link.txt"))
exe = bytearray(lcg(16384, 1))
for i in range(0, len(exe), 7):
    exe[i] = 0xE8
with open(os.path.join(root, "bin", "tiny.exe"), "wb") as f:
    f.write(bytes(exe))
with open(os.path.join(root, "random.bin"), "wb") as f:
    f.write(lcg(49152, 2))
with open(os.path.join(root, "comment.txt"), "wb") as f:
    f.write("Piesi RAR 注释\n".encode("utf-8"))
PY

rm -f ./*.rar ./*.r0?
OUT=$PWD
cd "$WORK/src"
FILES="hello.txt dir empty.txt emptydir 中文名.txt bin"
# -y 全部确认，-idq 安静，-ep1 不带上层目录，-r 递归目录
R="$RAR a -y -idq -ep1 -r"

$R -ma5 -s-            "$OUT/rar5.rar" $FILES
$R -ma4 -s-            "$OUT/rar4.rar" $FILES
$R -ma5 -s             "$OUT/solid5.rar" $FILES
$R -ma4 -s             "$OUT/solid4.rar" $FILES
$R -ma5 -m0            "$OUT/store5.rar" $FILES
$R -ma5 -ppass123      "$OUT/pw-data5.rar" $FILES
$R -ma5 -hppass123     "$OUT/pw-header5.rar" $FILES
$R -ma4 -ppass123      "$OUT/pw-data4.rar" $FILES
$R -ma4 -hppass123     "$OUT/pw-header4.rar" $FILES
# 符号链接按链接存（-ol）：RAR5 用 redir 记录，RAR4 把目标存成文件内容
$R -ma5 -ol            "$OUT/symlink5.rar" link.txt hello.txt
$R -ma4 -ol            "$OUT/symlink4.rar" link.txt hello.txt
# 分卷：随机数据不可压缩，-v20k 切成 3 卷；RAR4 用旧式 .r00 编号（-vn）
$R -ma5 -v20k          "$OUT/vol5.rar" random.bin hello.txt
$R -ma4 -vn -v20k      "$OUT/vol4.rar" random.bin hello.txt
# 注释
$R -ma5 -zcomment.txt  "$OUT/comment5.rar" hello.txt
# 只有空文件与空目录
$R -ma5                "$OUT/empty-only.rar" empty.txt emptydir

ls -la "$OUT"
