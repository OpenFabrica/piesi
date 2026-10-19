#!/bin/sh
# 生成 7z 驱动的测试夹具（每个 < 100 KB）。
# 依赖：7zz（brew install sevenzip）、python3。源文件树内容是确定性的，
# tests/drivers_common.rs 里的 expected_* 必须与这里一致。
set -eu
cd "$(dirname "$0")"
SEVENZ=${SEVENZ:-/opt/homebrew/bin/7zz}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# 用 python 生成确定性的源文件树：文本、空文件、空目录、符号链接、伪 exe、随机数据
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
# 伪 exe：每第 7 个字节是 x86 call 指令 0xE8，让 BCJ/BCJ2 过滤器真的有事可做
exe = bytearray(lcg(16384, 1))
for i in range(0, len(exe), 7):
    exe[i] = 0xE8
with open(os.path.join(root, "bin", "tiny.exe"), "wb") as f:
    f.write(bytes(exe))
with open(os.path.join(root, "random.bin"), "wb") as f:
    f.write(lcg(49152, 2))
PY

rm -f ./*.7z ./*.7z.0??
cd "$WORK/src"
# 公共文件列表（不含 random.bin，它只用于分卷包）
FILES="hello.txt dir empty.txt emptydir 中文名.txt bin"
Z="$SEVENZ a -y -bso0 -bsp0 -t7z"
OUT=$(cd - >/dev/null && pwd)

$Z -ms=on  "$OUT/solid.7z" $FILES
$Z -ms=off "$OUT/nonsolid.7z" $FILES
$Z -ms=on -ppass123         "$OUT/aes-data.7z" $FILES
$Z -ms=on -ppass123 -mhe=on "$OUT/aes-header.7z" $FILES
# BCJ2：-mx=9 对 exe 自动启用；为保险显式指定过滤器
$Z -mx=9 -mf=BCJ2  "$OUT/bcj2.7z" bin/tiny.exe hello.txt
$Z -mf=BCJ         "$OUT/bcj.7z" bin/tiny.exe
$Z -mf=ARM64       "$OUT/arm64.7z" bin/tiny.exe hello.txt
$Z -mf=Delta:4     "$OUT/delta.7z" bin/tiny.exe
$Z -m0=LZMA        "$OUT/lzma1.7z" $FILES
$Z -m0=PPMd        "$OUT/ppmd.7z" hello.txt dir
$Z -m0=BZip2       "$OUT/bzip2.7z" hello.txt dir
$Z -m0=Copy        "$OUT/copy.7z" hello.txt dir
# 符号链接按链接存（-snl），不跟随
$Z -snl            "$OUT/symlink.7z" link.txt hello.txt
# 分卷：随机数据不可压缩，-v20k 会切成 3 卷
$Z -v20k           "$OUT/vol.7z" random.bin hello.txt
# 空包（只有空目录与空文件，没有数据流）
$Z                 "$OUT/empty-only.7z" empty.txt emptydir

ls -la "$OUT"
