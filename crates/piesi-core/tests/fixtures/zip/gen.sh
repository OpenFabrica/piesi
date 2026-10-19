#!/bin/bash
# 生成 zip 读取器的测试夹具。依赖本机工具：/usr/bin/zip、/opt/homebrew/bin/7zz、/opt/homebrew/bin/zstd、python3。
# 每个夹具 < 50 KB。源文件放在 src/，集成测试按条目名到 src/ 下取期望内容。
# 时间戳固定为 2024-01-02 15:30:00（Asia/Shanghai），即 UTC 1704180600，测试据此校验 UT/NTFS 时间字段。
set -euo pipefail
cd "$(dirname "$0")"
export TZ=Asia/Shanghai
ZIP=/usr/bin/zip
SZ=/opt/homebrew/bin/7zz
ZSTD=/opt/homebrew/bin/zstd
STAMP=202401021530.00

rm -rf src
mkdir -p src/data
python3 -I - <<'EOF'
import random
r = random.Random(42)
open('src/hello.txt', 'w').write('Hello, Piesi!\n')
open('src/empty.txt', 'w').write('')
open('src/data/lorem.txt', 'w').write('Lorem ipsum dolor sit amet, consectetur adipiscing elit. ' * 400 + '\n')
open('src/bin.dat', 'wb').write(r.randbytes(12000))
EOF
find src -exec touch -t "$STAMP" {} +
rm -f ./*.zip ./*.z01 ./*.z02 ./*.zip.0*

cd src
# 7zz：各压缩方法；小文件与随机数据 7zz 会自动退回 Store，只有 lorem.txt 真正用上指定方法
for pair in Copy:store Deflate:deflate Deflate64:deflate64 BZip2:bzip2 LZMA:lzma XZ:xz PPMd:ppmd; do
    method=${pair%%:*}
    name=${pair##*:}
    $SZ a -tzip -mm="$method" "../$name.zip" hello.txt empty.txt bin.dat data >/dev/null
done
# WinZip AES（7zz 写的是 AE-2：CRC 字段为 0）
$SZ a -tzip -pTEST -mem=AES256 ../aes256.zip hello.txt empty.txt data/lorem.txt >/dev/null
$SZ a -tzip -pTEST -mem=AES128 ../aes128.zip hello.txt bin.dat >/dev/null
# ZipCrypto；从 stdin 读会带 data descriptor（校验字节改用 DOS 时间高字节）
$ZIP -q -P TEST ../zipcrypto.zip hello.txt bin.dat data/lorem.txt
$ZIP -q -P TEST ../zipcrypto_dd.zip - <data/lorem.txt
$ZIP -q ../datadesc.zip - <data/lorem.txt
# 目录条目 + Info-ZIP UT 时间戳
$ZIP -q -r ../dirs.zip data hello.txt
# 符号链接
ln -sfn hello.txt link
$ZIP -q -y ../symlink.zip link hello.txt
rm link
# 7-Zip 风格字节分卷 .zip.001/.002
$SZ a -tzip -v8k ../vol.zip bin.dat data/lorem.txt hello.txt >/dev/null
cd ..

# 其余靠 python 手工拼：zstd、zip64、编码相关、PKWARE 多盘、AE-1、截断、前缀垃圾
python3 -I - "$ZSTD" <<'EOF'
import struct, sys, zlib, zipfile, subprocess, os

ZSTD = sys.argv[1]
DOS_DATE = ((2024 - 1980) << 9) | (1 << 5) | 2
DOS_TIME = (15 << 11) | (30 << 5)
HELLO = open('src/hello.txt', 'rb').read()
LOREM = open('src/data/lorem.txt', 'rb').read()
BIN = open('src/bin.dat', 'rb').read()


def deflate(data):
    c = zlib.compressobj(9, zlib.DEFLATED, -15)
    return c.compress(data) + c.flush()


def local_header(name, flags, method, crc, csize, usize, extra=b''):
    return struct.pack('<IHHHHHIIIHH', 0x04034b50, 20, flags, method, DOS_TIME, DOS_DATE,
                       crc, csize, usize, len(name), len(extra)) + name + extra


def cd_header(name, flags, method, crc, csize, usize, offset, extra=b'', disk=0, ext=0, vmb=20):
    return struct.pack('<IHHHHHHIIIHHHHHII', 0x02014b50, vmb, 20, flags, method, DOS_TIME, DOS_DATE,
                       crc, csize, usize, len(name), len(extra), 0, disk, 0, ext, offset) + name + extra


def eocd(n, cdsize, cdoff, comment=b'', this_disk=0, cd_disk=0):
    return struct.pack('<IHHHHIIH', 0x06054b50, this_disk, cd_disk, n, n, cdsize, cdoff, len(comment)) + comment


def pack(data, method):
    if method == 0:
        return data
    if method == 8:
        return deflate(data)
    if method == 93:
        return subprocess.run([ZSTD, '-q', '-19', '-c'], input=data, capture_output=True, check=True).stdout
    raise ValueError(method)


def build(path, entries, comment=b''):
    """entries: [(name_bytes, data, method, flags, extra_local, extra_cd)] → 普通单盘 zip"""
    out = bytearray()
    cd = bytearray()
    for name, data, method, flags, exl, exc in entries:
        packed = pack(data, method)
        crc = zlib.crc32(data)
        off = len(out)
        out += local_header(name, flags, method, crc, len(packed), len(data), exl) + packed
        cd += cd_header(name, flags, method, crc, len(packed), len(data), off, exc)
    cdoff = len(out)
    out += cd + eocd(len(entries), len(cd), cdoff, comment)
    open(path, 'wb').write(out)


# zstd（方法 93），7zz 不会写
build('zstd.zip', [(b'data/lorem.txt', LOREM, 93, 0, b'', b''), (b'hello.txt', HELLO, 93, 0, b'', b'')])

# 原始 GBK 名字，无任何 Unicode 提示
gbk = '乱码测试.txt'.encode('gbk')
build('gbk_names.zip', [(gbk, b'gbk test\n', 8, 0, b'', b'')])

# GBK 名字 + 0x7075 Info-ZIP Unicode 路径（版本 1 + 原名 CRC + UTF-8 名）
gbk2 = '中文名.txt'.encode('gbk')
up = b'\x01' + struct.pack('<I', zlib.crc32(gbk2)) + '中文名.txt'.encode('utf-8')
up_field = struct.pack('<HH', 0x7075, len(up)) + up
build('unicode_path.zip', [(gbk2, b'unicode path test\n', 0, 0, up_field, up_field)])

# bit 11 的 UTF-8 中文名（zipfile 对非 cp437 名字自动置位）
with zipfile.ZipFile('utf8_names.zip', 'w', zipfile.ZIP_DEFLATED) as zf:
    for name, data in [('中文/文件名.txt', '中文内容\n'), ('日本語.txt', 'にほんご\n')]:
        zi = zipfile.ZipInfo(name, date_time=(2024, 1, 2, 15, 30, 0))
        zi.compress_type = zipfile.ZIP_DEFLATED
        zf.writestr(zi, data.encode('utf-8'))

# zip64：zipfile 的 force_zip64 只把本地头写成 zip64 形式（大小 0xFFFFFFFF + 扩展字段）
with zipfile.ZipFile('zip64_py.zip', 'w') as zf:
    for name, data in [('hello.txt', HELLO), ('data/lorem.txt', LOREM)]:
        zi = zipfile.ZipInfo(name, date_time=(2024, 1, 2, 15, 30, 0))
        zi.compress_type = zipfile.ZIP_DEFLATED
        with zf.open(zi, 'w', force_zip64=True) as f:
            f.write(data)

# 全量 zip64：目录头大小/偏移/盘号全部饱和，EOCD 全部饱和，带 zip64 EOCD 记录与定位记录
out = bytearray()
cd = bytearray()
ents = [(b'hello.txt', HELLO, 0), (b'data/lorem.txt', LOREM, 8)]
for name, data, method in ents:
    packed = pack(data, method)
    crc = zlib.crc32(data)
    off = len(out)
    exl = struct.pack('<HHQQ', 1, 16, len(data), len(packed))
    out += local_header(name, 0, method, crc, 0xFFFFFFFF, 0xFFFFFFFF, exl) + packed
    exc = struct.pack('<HHQQQI', 1, 28, len(data), len(packed), off, 0)
    cd += cd_header(name, 0, method, crc, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF, exc, disk=0xFFFF, vmb=45)
cdoff = len(out)
out += cd
z64pos = len(out)
out += struct.pack('<IQHHIIQQQQ', 0x06064b50, 44, 45, 45, 0, 0, len(ents), len(ents), len(cd), cdoff)
out += struct.pack('<IIQI', 0x07064b50, 0, z64pos, 1)
out += eocd(0xFFFF, 0xFFFFFFFF, 0xFFFFFFFF, this_disk=0xFFFF, cd_disk=0xFFFF)
open('zip64_full.zip', 'wb').write(out)

# 空包与注释
with zipfile.ZipFile('empty.zip', 'w'):
    pass
with zipfile.ZipFile('comment.zip', 'w') as zf:
    zf.comment = b'Piesi test comment'
    zi = zipfile.ZipInfo('hello.txt', date_time=(2024, 1, 2, 15, 30, 0))
    zf.writestr(zi, HELLO)

# PKWARE 多盘分卷（模拟 zip -s 的布局；zip 本身最小分卷 64 KB，夹具放不下）：
# 首盘以 PK\x07\x08 标记开头，本地头偏移相对所在盘，bin.dat 的数据跨两个盘边界
DISK = 5000
stream = bytearray(b'PK\x07\x08')
recs = []
for name, data, method in [(b'hello.txt', HELLO, 0), (b'bin.dat', BIN, 0), (b'data/lorem.txt', LOREM, 8)]:
    packed = pack(data, method)
    crc = zlib.crc32(data)
    off = len(stream)
    stream += local_header(name, 0, method, crc, len(packed), len(data)) + packed
    recs.append((name, method, crc, len(packed), len(data), off))


def locate(off):
    d = min(off // DISK, 2)
    return d, off - d * DISK


cdoff = len(stream)
cd = bytearray()
for name, method, crc, csize, usize, off in recs:
    d, rel = locate(off)
    cd += cd_header(name, 0, method, crc, csize, usize, rel, disk=d)
stream += cd
cd_disk, cd_rel = locate(cdoff)
stream += eocd(len(recs), len(cd), cd_rel, this_disk=2, cd_disk=cd_disk)
assert len(stream) > 2 * DISK + len(cd) + 22, '最后一盘必须装下整个中央目录'
open('pkware_split.z01', 'wb').write(stream[:DISK])
open('pkware_split.z02', 'wb').write(stream[DISK:2 * DISK])
open('pkware_split.zip', 'wb').write(stream[2 * DISK:])


# AE-1：把 7zz 的 AE-2 包改成 AE-1（厂商版本 1 + 真实 CRC）；HMAC 不覆盖头部，改头不影响认证
def patch_ae1(src, dst, contents):
    d = bytearray(open(src, 'rb').read())
    e = d.rfind(b'PK\x05\x06')
    n, cdsize, cdoff = struct.unpack('<HII', d[e + 10:e + 20])
    p = cdoff
    for _ in range(n):
        nl, el, cl = struct.unpack('<HHH', d[p + 28:p + 34])
        name = bytes(d[p + 46:p + 46 + nl])
        off = struct.unpack('<I', d[p + 42:p + 46])[0]
        crc = zlib.crc32(contents[name])
        d[p + 16:p + 20] = struct.pack('<I', crc)
        x = d.find(b'\x01\x99', p + 46 + nl, p + 46 + nl + el)
        d[x + 4:x + 6] = b'\x01\x00'
        lnl, lel = struct.unpack('<HH', d[off + 26:off + 30])
        d[off + 14:off + 18] = struct.pack('<I', crc)
        x = d.find(b'\x01\x99', off + 30 + lnl, off + 30 + lnl + lel)
        d[x + 4:x + 6] = b'\x01\x00'
        p += 46 + nl + el + cl
    open(dst, 'wb').write(d)


patch_ae1('aes256.zip', 'aes256_ae1.zip', {b'hello.txt': HELLO, b'empty.txt': b'', b'data/lorem.txt': LOREM})

# 截断：一个切在中央目录里（所有条目数据完整），一个切在最后一个条目的数据里
d = open('deflate.zip', 'rb').read()
e = d.rfind(b'PK\x05\x06')
n, cdsize, cdoff = struct.unpack('<HII', d[e + 10:e + 20])
open('truncated.zip', 'wb').write(d[:cdoff + 10])
offs = []
p = cdoff
for _ in range(n):
    nl, el, cl = struct.unpack('<HHH', d[p + 28:p + 34])
    offs.append(struct.unpack('<I', d[p + 42:p + 46])[0])
    p += 46 + nl + el + cl
open('truncated_data.zip', 'wb').write(d[:max(offs) + 40])

# 前缀垃圾（自解压 stub 之类）：所有偏移整体偏小，读取器要按 EOCD 位置修正
open('prefix.zip', 'wb').write(b'JUNK' * 100 + d)
EOF

ls -la ./*.zip ./*.z01 ./*.z02 ./*.zip.0* | awk '{ if ($5 >= 51200) print "TOO BIG:", $0 }'
echo "fixtures generated"
