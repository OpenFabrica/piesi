#!/bin/sh
# 生成 tar 系与单文件压缩流的测试夹具（每个 < 100 KB）。
# 依赖：/usr/bin/bsdtar、gzip、bzip2、xz（brew install xz）、zstd（brew install zstd）、python3。
# 源文件树内容是确定性的，tests/drivers_common.rs 里的 expected_* 必须与这里一致。
set -eu
cd "$(dirname "$0")"
TAR=${TAR:-/usr/bin/bsdtar}
# macOS 的 bsdtar 默认把 xattr 写成 ._ 开头的 AppleDouble 条目，夹具要干净的树
TAR="$TAR --no-mac-metadata"
export COPYFILE_DISABLE=1
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# 用 python 生成确定性的源文件树（与 sevenz/rar 的 gen.sh 相同）
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
# 长文件名（> 100 字节）触发 GNU 长名或 pax 扩展头
longdir = os.path.join(root, "long")
os.makedirs(longdir)
with open(os.path.join(longdir, "a" * 120 + ".txt"), "wb") as f:
    f.write(b"long name\n")
# 硬链接：同一 inode 第二次出现时 bsdtar 写成 hard link 条目
os.link(os.path.join(root, "hello.txt"), os.path.join(root, "hard.txt"))
PY

rm -f ./*.tar ./*.tar.gz ./*.tar.bz2 ./*.tar.xz ./*.tar.zst ./*.gz ./*.bz2 ./*.xz ./*.zst

# GBK 编码文件名的 tar：给编码识别层用，驱动只保证原始字节原样透传
python3 -I - "$PWD/gbk-names.tar" <<'PY'
import io, sys, tarfile
with tarfile.open(sys.argv[1], "w", format=tarfile.USTAR_FORMAT, encoding="gbk") as t:
    data = "GBK 文件名\n".encode("utf-8")
    ti = tarfile.TarInfo("中文名.txt")
    ti.size = len(data)
    ti.mtime = 1700000000
    ti.mode = 0o644
    t.addfile(ti, io.BytesIO(data))
PY

OUT=$PWD
cd "$WORK/src"
FILES="hello.txt dir empty.txt emptydir 中文名.txt bin"

# 普通 tar（短名，默认 restricted pax = 纯 ustar 头）
$TAR -cf "$OUT/plain.tar" $FILES
# 长文件名：默认格式走 pax 扩展头，gnutar 格式走 GNU 'L' 条目
$TAR -cf "$OUT/longname-pax.tar" long
$TAR --format=gnutar -cf "$OUT/longname-gnu.tar" long
# 符号链接与硬链接
$TAR -cf "$OUT/symlink.tar" link.txt hello.txt
$TAR -cf "$OUT/hardlink.tar" hello.txt hard.txt
# 压缩外壳：都用外部工具压，避免依赖 bsdtar 的编译选项
$TAR -cf - $FILES | gzip -9 -n > "$OUT/plain.tar.gz"
$TAR -cf - $FILES | bzip2 -9 > "$OUT/plain.tar.bz2"
$TAR -cf - $FILES | xz -9 > "$OUT/plain.tar.xz"
$TAR -cf - $FILES | zstd -19 -q > "$OUT/plain.tar.zst"
# 多成员 gzip：把 tar 切成两段分别 gzip 再拼接（pigz / --rsyncable 风格），MultiGzDecoder 必须能连续解
$TAR -cf - $FILES random.bin > "$WORK/multi.tar"
split -b 30k "$WORK/multi.tar" "$WORK/part."
: > "$OUT/multistream.tar.gz"
for p in "$WORK"/part.*; do gzip -c -n "$p" >> "$OUT/multistream.tar.gz"; done

# 单文件压缩流
gzip -9 -c hello.txt > "$OUT/hello.txt.gz"          # 带 FNAME（gzip 读文件时默认记录原名）
gzip -9 -n -c < hello.txt > "$OUT/noname.gz"         # 无 FNAME，名字只能去掉扩展名
bzip2 -9 -c hello.txt > "$OUT/hello.txt.bz2"
xz -9 -c hello.txt > "$OUT/hello.txt.xz"
zstd -19 -q -c hello.txt > "$OUT/hello.txt.zst"
# 两个 gzip 成员拼接成一个 .gz：解出来应是两段内容相连
{ gzip -c -n hello.txt; gzip -c -n dir/nested.txt; } > "$OUT/multi-member.gz"

ls -la "$OUT"
