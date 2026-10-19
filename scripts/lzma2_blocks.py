#!/usr/bin/env python3
# 基准辅助：走一遍 7z 文件里第一个 pack stream 的 LZMA2 chunk 链，数独立块（字典重置）和每块原始大小。
# 用途：对比 7zz 与 piesi 的多线程分块策略（块越大压缩率越好、并行度越低）。
# 用法：python3 -I scripts/lzma2_blocks.py 文件.7z [packed_size]
# 不给 packed_size 时用 `7zz l -slt` 读取「Packed Size」。
import struct
import subprocess
import sys


def packed_size(path):
    """用 7zz 读第一个 pack stream 的压缩后大小。"""
    out = subprocess.run(["7zz", "l", "-slt", path], capture_output=True, text=True, check=True).stdout
    for line in out.splitlines():
        if line.startswith("Packed Size = ") and line[14:].strip():
            return int(line[14:])
    raise SystemExit("找不到 Packed Size")


def walk(path, length):
    """解析 chunk 头：控制字节 0x80 以上是压缩块，1/2 是存储块，0 结束；mode 3 或 1 表示字典重置。"""
    with open(path, "rb") as f:
        f.seek(32)  # 7z 签名头 32 字节之后就是 pack stream
        data = f.read(length)
    pos = chunks = resets = cur = total = 0
    blocks = []
    while pos < len(data):
        c = data[pos]
        pos += 1
        if c == 0:
            break
        if c & 0x80:
            unp = ((c & 0x1F) << 16) + struct.unpack(">H", data[pos:pos + 2])[0] + 1
            pk = struct.unpack(">H", data[pos + 2:pos + 4])[0] + 1
            pos += 4
            mode = (c >> 5) & 3
            if mode >= 2:
                pos += 1  # 带 props 字节
            if mode == 3:
                resets += 1
                if cur:
                    blocks.append(cur)
                    cur = 0
            pos += pk
        elif c in (1, 2):
            unp = struct.unpack(">H", data[pos:pos + 2])[0] + 1
            pos += 2
            if c == 1:
                resets += 1
                if cur:
                    blocks.append(cur)
                    cur = 0
            pos += unp
        else:
            raise SystemExit(f"非法控制字节 {c:#x} @ {pos}")
        chunks += 1
        cur += unp
        total += unp
    if cur:
        blocks.append(cur)
    mb = [round(b / 2 ** 20, 1) for b in blocks]
    print(f"{path}: chunks={chunks} 独立块={len(blocks)} 原始总量={total} 块大小MB(min/max)={min(mb)}/{max(mb)} 前5块={mb[:5]}")


if __name__ == "__main__":
    p = sys.argv[1]
    n = int(sys.argv[2]) if len(sys.argv) > 2 else packed_size(p)
    walk(p, n)
