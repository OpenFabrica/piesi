#!/usr/bin/env bash
# 端到端冒烟：用 piesi 创建 zip/7z/tgz，再用 7zz、unzip、bsdtar 交叉验证；反向用 7zz 建包、piesi 解压并比对。
# 用法：scripts/smoke.sh（需要 /opt/homebrew/bin/7zz）
set -euo pipefail
cd "$(dirname "$0")/.."
SEVENZ=${SEVENZ:-/opt/homebrew/bin/7zz}
# 没有显式指定 PIESI 就每次都构建（增量构建很快），免得拿上一次的旧二进制测新代码
if [ -z "${PIESI:-}" ]; then
  CARGO_TARGET_DIR=target/smoke cargo build --release -p piesi-cli -q
  PIESI=$(pwd)/target/smoke/release/piesi
fi
T=$(mktemp -d /tmp/piesi-smoke.XXXXXX)
trap 'rm -rf "$T"' EXIT
SRC="$T/源目录 测试"
mkdir -p "$SRC/子目录/深层/更深" "$SRC/__MACOSX" "$SRC/空目录"
printf '你好，世界\n' > "$SRC/中文文件.txt"
head -c 300000 /dev/urandom > "$SRC/子目录/随机.bin"
python3 -I -c "import sys; sys.stdout.write('lorem ipsum ' * 50000)" > "$SRC/子目录/深层/文本.txt"
: > "$SRC/子目录/深层/更深/空文件"
printf 'junk' > "$SRC/.DS_Store"
printf 'junk' > "$SRC/__MACOSX/._x"
ln -s "子目录/深层/文本.txt" "$SRC/链接"
cp "$SRC/中文文件.txt" "$SRC/子目录/README.md"

fail() { echo "失败：$*" >&2; exit 1; }

echo "## 1. piesi 创建 → 第三方工具校验"
( cd "$T" && "$PIESI" a -q out.zip "源目录 测试" && "$PIESI" a -q out.7z "源目录 测试" && "$PIESI" a -q out.tgz "源目录 测试" && "$PIESI" a -q out.txz "源目录 测试" && "$PIESI" a -q out.tzst "源目录 测试" )
"$SEVENZ" t "$T/out.zip" >/dev/null || fail "7zz 校验 out.zip"
"$SEVENZ" t "$T/out.7z" >/dev/null || fail "7zz 校验 out.7z"
unzip -tqq "$T/out.zip" || fail "unzip 校验 out.zip"
bsdtar -tf "$T/out.tgz" >/dev/null || fail "bsdtar 读 out.tgz"
bsdtar -tf "$T/out.txz" >/dev/null || fail "bsdtar 读 out.txz"
bsdtar -tf "$T/out.tzst" >/dev/null || fail "bsdtar 读 out.tzst"
# 垃圾文件不应进包；中文名应是 UTF-8 + bit 11
unzip -l "$T/out.zip" | grep -q "__MACOSX" && fail "zip 里有 __MACOSX"
unzip -l "$T/out.zip" | grep -q ".DS_Store" && fail "zip 里有 .DS_Store"
python3 -I - "$T/out.zip" <<'PY' || fail "zip 文件名标志检查"
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
names = [i.filename for i in z.infolist()]
assert any('中文文件.txt' in n for n in names), names
assert all((i.flag_bits & 0x800) for i in z.infolist()), "bit 11 未全部置位"
assert not any('__MACOSX' in n or '.DS_Store' in n for n in names)
print("  zip 条目：", len(names), "个，bit 11 全部置位，无垃圾")
PY
echo "  zip/7z/tgz/txz/tzst 全部通过第三方校验"

echo "## 2. piesi 解压自己建的包 → 与源目录比对"
for f in out.zip out.7z out.tgz out.txz out.tzst; do
  d="$T/x-$f"; mkdir -p "$d"
  "$PIESI" x -q -o "$d" "$T/$f"
  diff -r --exclude=.DS_Store --exclude=__MACOSX "$SRC" "$d/源目录 测试" || fail "$f 解压内容与源不一致"
  [ -L "$d/源目录 测试/链接" ] || fail "$f 符号链接没保留"
  [ -d "$d/源目录 测试/空目录" ] || fail "$f 空目录没保留"
done
echo "  五种格式解压后与源目录一致（含符号链接、空目录、空文件）"

echo "## 3. 7zz 建包（含 BCJ2、solid、AES、GBK 名）→ piesi 解压"
( cd "$T" && "$SEVENZ" a -bso0 -t7z -mx=7 -ms=on ref.7z "源目录 测试" && "$SEVENZ" a -bso0 -tzip ref.zip "源目录 测试" && "$SEVENZ" a -bso0 -t7z -pSECRET -mhe=on enc.7z "源目录 测试" )
for f in ref.7z ref.zip; do
  d="$T/y-$f"; mkdir -p "$d"
  "$PIESI" x -q -o "$d" "$T/$f"
  diff -r --exclude=.DS_Store --exclude=__MACOSX "$SRC" "$d/源目录 测试" || fail "${f}（7zz 产物）解压不一致"
done
d="$T/y-enc"; mkdir -p "$d"
"$PIESI" x -q -o "$d" -p SECRET "$T/enc.7z"
diff -r --exclude=.DS_Store --exclude=__MACOSX "$SRC" "$d/源目录 测试" || fail "头加密 7z 解压不一致"
"$PIESI" x -q -o "$T/y-wrong" -p WRONG "$T/enc.7z" 2>/dev/null && fail "错密码竟然成功"
echo "  7zz 产物（solid、头加密）解压一致，错密码正确拒绝"

echo "## 4. GBK 文件名 zip（WinRAR/老 7-Zip 风格）→ 自动识别"
python3 -I - "$T/gbk.zip" <<'PY'
import sys, zlib, struct, time
names = ["项目资料/", "项目资料/年度总结报告.docx", "项目资料/会议纪要_第三季度.txt", "合同扫描件.pdf"]
out = bytearray(); cd = bytearray(); offset = 0
for n in names:
    raw = n.encode('gb18030'); data = b'' if n.endswith('/') else ("内容:"+n).encode('utf-8')
    crc = zlib.crc32(data) & 0xffffffff
    lh = struct.pack('<IHHHHHIIIHH', 0x04034b50, 20, 0, 0, 0, 0x21, crc, len(data), len(data), len(raw), 0) + raw
    out += lh + data
    cd += struct.pack('<IHHHHHHIIIHHHHHII', 0x02014b50, 20, 20, 0, 0, 0, 0x21, crc, len(data), len(data), len(raw), 0, 0, 0, 0, 0x10 if n.endswith('/') else 0, offset) + raw
    offset += len(lh) + len(data)
eocd = struct.pack('<IHHHHIIH', 0x06054b50, 0, 0, len(names), len(names), len(cd), len(out), 0)
open(sys.argv[1], 'wb').write(bytes(out) + bytes(cd) + eocd)
PY
"$PIESI" list "$T/gbk.zip" --json > "$T/gbk.json" 2>/dev/null
python3 -I - "$T/gbk.json" <<'PY' || fail "GBK 识别"
import sys, json
j = json.load(open(sys.argv[1]))
assert j['encoding'] == 'Gb18030', j['encoding']
names = sorted(e['name'] for e in j['entries'])
assert '项目资料/年度总结报告.docx' in names, names
print("  识别为", j['encoding'], "置信度 %.2f" % j['confidence'], "不确定" if j['ambiguous'] else "确定", "；名字正确")
PY
d="$T/z-gbk"; mkdir -p "$d"; "$PIESI" x -q -o "$d" "$T/gbk.zip"
[ -f "$d/gbk/项目资料/年度总结报告.docx" ] || fail "GBK 包解压后文件名不对"
echo "  GBK 包解压后路径正确"

echo "## 5. 安全：zip-slip 与越界符号链接"
python3 -I - "$T/slip.zip" <<'PY'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], 'w') as z:
    z.writestr('../../evil.txt', 'x'); z.writestr('/abs.txt', 'x'); z.writestr('ok/fine.txt', 'fine')
PY
d="$T/z-slip"; mkdir -p "$d"; "$PIESI" x -q -o "$d" "$T/slip.zip" 2>"$T/slip.err" || true
[ ! -e "$T/evil.txt" ] && [ ! -e "/abs.txt" ] || fail "zip-slip 逃逸成功"
[ -f "$d/slip/ok/fine.txt" ] || [ -f "$d/ok/fine.txt" ] || fail "正常条目没解出"
echo "  越界条目被拒绝，正常条目照常解出"

echo "## 6. --mac-metadata：xattr / 资源分支 / Finder 信息 → __MACOSX/._ → ditto 还原"
MSRC="$T/mac源"; mkdir -p "$MSRC/子目录"
printf 'tagged\n' > "$MSRC/标签.txt"
xattr -w com.example.k v "$MSRC/标签.txt"
printf 'RSRC' > "$MSRC/标签.txt/..namedfork/rsrc"
xattr -wx com.apple.FinderInfo "54455854 74747874 0400 0000 0000 0000 0000 0000 0000 0000 0000 0000 0000 0000" "$MSRC/标签.txt"
xattr -w com.apple.quarantine "0083;00000000;Safari;X" "$MSRC/标签.txt"
printf 'n\n' > "$MSRC/子目录/嵌套.txt"; xattr -w com.example.n nv "$MSRC/子目录/嵌套.txt"
( cd "$T" && "$PIESI" a -q --mac-metadata mac.zip "mac源" )
unzip -tqq "$T/mac.zip" || fail "unzip 校验 mac.zip"
# unzip -l 会把中文名打成乱码，条目名和 ._ 内容用 zipfile 看
python3 -I - "$T/mac.zip" <<'PY' || fail "Mac 专用包的 __MACOSX 条目检查"
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1]); names = [i.filename for i in z.infolist()]
assert '__MACOSX/mac源/._标签.txt' in names, names
assert '__MACOSX/mac源/子目录/._嵌套.txt' in names, names
assert names.index('__MACOSX/mac源/._标签.txt') == names.index('mac源/标签.txt') + 1, names
ad = z.read('__MACOSX/mac源/._标签.txt')
assert ad[:4] == b'\x00\x05\x16\x07' and ad[84:88] == b'ATTR', ad[:90]
assert b'com.apple.quarantine' not in ad, "quarantine 进包了"
assert b'com.example.k' in ad and ad.endswith(b'RSRC'), ad
print("  __MACOSX/._ 条目", sum('/._' in n for n in names), "个，紧跟各自条目，含 xattr 与资源分支，不含 quarantine")
PY
d="$T/m-ditto"; mkdir -p "$d"; ditto -x -k "$T/mac.zip" "$d"
[ "$(xattr -p com.example.k "$d/mac源/标签.txt")" = "v" ] || fail "ditto 解压后 xattr 没回来"
[ "$(cat "$d/mac源/标签.txt/..namedfork/rsrc")" = "RSRC" ] || fail "ditto 解压后资源分支没回来"
xattr -px com.apple.FinderInfo "$d/mac源/标签.txt" | head -1 | grep -q "^54 45 58 54 74 74 78 74 04 00" || fail "ditto 解压后 Finder 信息没回来"
xattr -p com.apple.quarantine "$d/mac源/标签.txt" >/dev/null 2>&1 && fail "quarantine 被带过来了"
[ "$(xattr -p com.example.n "$d/mac源/子目录/嵌套.txt")" = "nv" ] || fail "嵌套文件的 xattr 没回来"
[ ! -e "$d/__MACOSX" ] || fail "ditto 没有吃掉 __MACOSX"
d="$T/m-piesi"; mkdir -p "$d"; "$PIESI" x -q -o "$d" "$T/mac.zip"
[ ! -e "$d/mac源/__MACOSX" ] && [ ! -e "$d/__MACOSX" ] || fail "piesi 解自己的 Mac 专用包时没跳过 __MACOSX"
diff -r "$MSRC" "$d/mac源" || fail "Mac 专用包解压内容与源不一致"
echo "  __MACOSX/._ 条目位置正确、不含 quarantine；ditto 还原 xattr / 资源分支 / Finder 信息；piesi 解压时跳过"
echo "全部通过"
