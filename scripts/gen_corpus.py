#!/usr/bin/env python3
"""生成文件名编码语料：手工拼 ZIP（store 方法），覆盖 编码 × 标志 × version made by 的组合。

用法：python3 -I scripts/gen_corpus.py tests/corpus
输出：
  <out>/encoding/*.zip   小夹具，入库
  <out>/manifest.json    每个包一条 {file, expected_encoding, expected_source, expected_names, ambiguous, hints, note}
  <out>/raw_names.json   每个包的条目原始字节与标志，供 encoding_corpus.rs 不经 zip 读取器直接构造 Entry
脚本幂等：固定时间戳、固定顺序，重跑产物字节一致。manifest.json 与 raw_names.json 由本脚本整体生成，不要手改。

为什么不用 zipfile 模块：它只会把 str 名字按 cp437/UTF-8 写，没法写 GBK 原始字节，也没法控制 bit 11 和 0x7075 字段。
"""
from __future__ import annotations

import json
import os
import struct
import sys
import zlib

# ---- 名字集：每种语言给几个短名 + 带目录的路径，覆盖 2 字名、全角标点、二级字、长音符等陷阱 ----
NAME_SETS: dict[str, list[str]] = {
    "utf8": ["你好世界.txt", "résumé.pdf", "Übersicht/Käse Straße.txt", "日本語/한국어.txt", "emoji 😀/файл.txt"],
    "gb18030": ["你好世界.txt", "文档/报告.docx", "图片/照片（1）.jpg", "张喆的简历.pdf"],
    "big5": ["測試檔案.txt", "文件/報告.docx", "圖片/相片.jpg"],
    "shift_jis": ["日本語ファイル.txt", "資料/報告書.docx", "写真/東京タワー.jpg"],
    "euc_kr": ["한국어파일.txt", "문서/보고서.docx", "사진/서울.jpg"],
    "cp1251": ["Привет.txt", "Документы/Отчёт.docx", "Фото/Москва.jpg"],
}

# 编码 id → Python codec。cp932/cp949 分别对应 encoding_rs 的 Shift_JIS（WHATWG = Windows-31J）与 EUC-KR（= UHC）。
CODECS: dict[str, str] = {
    "utf8": "utf-8",
    "gb18030": "gb18030",
    "big5": "big5",
    "shift_jis": "cp932",
    "euc_kr": "cp949",
    "cp1251": "cp1251",
    "cp866": "cp866",
    "cp1252": "cp1252",
}

FLAG_UTF8 = 0x0800  # general purpose bit 11
HOST_DOS = 0
HOST_UNIX = 3
# 固定时间戳：2026-10-19 12:00:00（DOS 格式），保证幂等
DOS_TIME = (12 << 11) | (0 << 5) | 0
DOS_DATE = ((2026 - 1980) << 9) | (10 << 5) | 19


def crc32(data: bytes) -> int:
    """表驱动 CRC-32（IEEE），与 zlib.crc32 一致；自己实现是为了不依赖任何压缩库的行为细节。"""
    table = _crc_table()
    crc = 0xFFFFFFFF
    for b in data:
        crc = table[(crc ^ b) & 0xFF] ^ (crc >> 8)
    return crc ^ 0xFFFFFFFF


_TABLE: list[int] | None = None


def _crc_table() -> list[int]:
    """CRC-32 查找表，首次调用时生成。"""
    global _TABLE
    if _TABLE is None:
        table = []
        for n in range(256):
            c = n
            for _ in range(8):
                c = (0xEDB88320 ^ (c >> 1)) if (c & 1) else (c >> 1)
            table.append(c)
        _TABLE = table
    return _TABLE


def unicode_path_extra(raw_name: bytes, utf8_name: str) -> bytes:
    """Info-ZIP Unicode Path 扩展字段（0x7075）：版本 1 + 头部名字的 CRC32 + UTF-8 名字。"""
    data = b"\x01" + struct.pack("<I", crc32(raw_name)) + utf8_name.encode("utf-8")
    return struct.pack("<HH", 0x7075, len(data)) + data


class ZipEntry:
    """一个待写入的条目：原始名字字节、标志、是否带 0x7075、是否目录、内容。"""

    def __init__(self, utf8_name: str, raw_name: bytes, flags: int = 0, with_7075: bool = False, data: bytes = b""):
        self.utf8_name = utf8_name
        self.raw_name = raw_name
        self.flags = flags
        self.with_7075 = with_7075
        self.is_dir = utf8_name.endswith("/")
        self.data = b"" if self.is_dir else data


def build_zip(entries: list[ZipEntry], host: int) -> bytes:
    """手工拼 ZIP：local header + 数据 … central directory … EOCD，全部 store 方法。"""
    local_parts: list[bytes] = []
    central_parts: list[bytes] = []
    offset = 0
    for e in entries:
        extra = unicode_path_extra(e.raw_name, e.utf8_name) if e.with_7075 else b""
        crc = crc32(e.data)
        local = struct.pack(
            "<IHHHHHIIIHH",
            0x04034B50, 20, e.flags, 0, DOS_TIME, DOS_DATE, crc, len(e.data), len(e.data), len(e.raw_name), len(extra),
        ) + e.raw_name + extra
        local_parts.append(local + e.data)
        if host == HOST_UNIX:
            mode = 0o40755 if e.is_dir else 0o100644
            ext_attr = (mode << 16) | (0x10 if e.is_dir else 0)
        else:
            ext_attr = 0x10 if e.is_dir else 0x20
        central = struct.pack(
            "<IHHHHHHIIIHHHHHII",
            0x02014B50, (host << 8) | 20, 20, e.flags, 0, DOS_TIME, DOS_DATE, crc, len(e.data), len(e.data),
            len(e.raw_name), len(extra), 0, 0, 0, ext_attr, offset,
        ) + e.raw_name + extra
        central_parts.append(central)
        offset += len(local) + len(e.data)
    cd = b"".join(central_parts)
    eocd = struct.pack("<IHHHHIIH", 0x06054B50, 0, 0, len(entries), len(entries), len(cd), offset, 0)
    return b"".join(local_parts) + cd + eocd


def with_dirs(names: list[str]) -> list[str]:
    """给带目录的路径补上目录条目（像真实工具那样目录在前、只出现一次）。"""
    out: list[str] = []
    seen: set[str] = set()
    for n in names:
        parts = n.split("/")
        for i in range(1, len(parts)):
            d = "/".join(parts[:i]) + "/"
            if d not in seen:
                seen.add(d)
                out.append(d)
        if n not in seen:
            seen.add(n)
            out.append(n)
    return out


def make_entries(names: list[str], encoding: str, flags: int = 0, with_7075: bool = False, ascii_skips_7075: bool = False,
                 add_dirs: bool = True) -> list[ZipEntry]:
    """按编码把名字转成原始字节并生成条目。`ascii_skips_7075` 模拟 WinRAR：纯 ASCII 名不写 0x7075；
    `add_dirs=False` 不自动补目录条目（往已有包里追加文件的场景）。"""
    entries = []
    for n in (with_dirs(names) if add_dirs else names):
        raw = n.encode(CODECS[encoding])
        extra = with_7075 and not (ascii_skips_7075 and raw.isascii())
        entries.append(ZipEntry(n, raw, flags=flags, with_7075=extra, data=(n + "\n").encode("utf-8")))
    return entries


class Fixture:
    """一个语料包的描述：文件名、条目、host、期望结果与说明。"""

    def __init__(self, name: str, entries: list[ZipEntry], host: int, expected_encoding: str, expected_source: str,
                 ambiguous: bool, note: str, locale: str | None = None, expected_junk: list[str] | None = None):
        self.name = name
        self.entries = entries
        self.host = host
        self.expected_encoding = expected_encoding
        self.expected_source = expected_source
        self.ambiguous = ambiguous
        self.note = note
        self.locale = locale
        self.expected_junk = expected_junk or []


def grid_fixtures() -> list[Fixture]:
    """编码 × 标志 × made by 的网格。"""
    fixtures: list[Fixture] = []
    for enc, names in NAME_SETS.items():
        for host, host_tag in ((HOST_DOS, "dos"), (HOST_UNIX, "unix")):
            # raw：原始字节、无标志 → 靠识别
            if enc == "utf8":
                exp, src, amb, note = "utf8", "valid_utf8", False, "UTF-8 字节、无 bit 11（Finder/ditto、旧 7-Zip 风格），全部合法 UTF-8 直接判定"
            else:
                exp, src, amb, note = enc, "scored", False, f"{enc} 原始字节、无标志、无扩展字段，必须靠候选打分识别"
            fixtures.append(Fixture(f"{enc}_raw_{host_tag}", make_entries(names, enc), host, exp, src, amb, note))
            # bit11：现代工具写法，名字一律 UTF-8 并设 bit 11
            fixtures.append(Fixture(
                f"{enc}_bit11_{host_tag}", make_entries(names, "utf8", flags=FLAG_UTF8), host, "utf8", "format", False,
                f"同一组名字由现代工具写出：UTF-8 + bit 11（{enc} 名字集）",
            ))
            # 7075：原始字节 + Info-ZIP Unicode Path 字段
            fixtures.append(Fixture(
                f"{enc}_7075_{host_tag}", make_entries(names, enc, with_7075=True), host, "utf8", "format", False,
                f"{enc} 原始字节 + 0x7075 UTF-8 名；名字取自扩展字段，不需要识别",
            ))
    return fixtures


def special_fixtures() -> list[Fixture]:
    """真实来源风格与边界情况。"""
    fixtures: list[Fixture] = []

    finder_names = ["项目/", "项目/报告.docx", "项目/.DS_Store", "__MACOSX/", "__MACOSX/项目/", "__MACOSX/项目/._报告.docx", "Thumbs.db"]
    fixtures.append(Fixture(
        "finder_style", make_entries(finder_names, "utf8"), HOST_UNIX, "utf8", "valid_utf8", False,
        "Finder「压缩」风格：UTF-8 字节、不设 bit 11、made by Unix，夹带 __MACOSX/._ 与 .DS_Store 垃圾",
        expected_junk=["项目/.DS_Store", "__MACOSX/", "__MACOSX/项目/", "__MACOSX/项目/._报告.docx", "Thumbs.db"],
    ))

    winrar_names = ["readme.txt", "合同/", "合同/采购合同（最终版）.pdf", "合同/附件①.xlsx", "照片/", "照片/IMG_0001.jpg", "照片/喆.jpg"]
    fixtures.append(Fixture(
        "winrar_style", make_entries(winrar_names, "gb18030", with_7075=True, ascii_skips_7075=True), HOST_DOS, "utf8", "format", False,
        "WinRAR 6.x 中文 Windows 风格：GBK 字节 + 0x7075、made by DOS；纯 ASCII 名没有 0x7075（它们本身合法 UTF-8）",
    ))

    fixtures.append(Fixture(
        "liar_bit11_gbk", make_entries(NAME_SETS["gb18030"], "gb18030", flags=FLAG_UTF8), HOST_DOS, "gb18030", "scored", False,
        "bit 11 撒谎：标志设了但字节是 GBK（非法 UTF-8）。标志与字节矛盾时信字节，回退到打分",
    ))

    mixed = make_entries(["新建文件夹/", "新建文件夹/旧文件.txt"], "gb18030") + make_entries(["新建文件夹/新增.txt"], "utf8", flags=FLAG_UTF8, add_dirs=False)
    fixtures.append(Fixture(
        "mixed_gbk_and_utf8", mixed, HOST_DOS, "gb18030", "scored", False,
        "同一包里旧条目是 GBK 无标志、新条目是 UTF-8 + bit 11（被不同工具更新过）：打分只看无标志条目，带标志的按 UTF-8",
    ))

    fixtures.append(Fixture(
        "short_gbk_two_chars", make_entries(["你好.txt"], "gb18030"), HOST_DOS, "gb18030", "scored", True,
        "只有一个 2 字名：GBK/Big5/EUC-KR 都能解出常用字，证据不足，靠 zh-CN locale 先验选 GBK 并标 ambiguous 让用户确认",
        locale="zh-CN",
    ))

    fixtures.append(Fixture(
        "accidental_utf8_gbk", make_entries(["图片.docx", "通知.txt"], "gb18030"), HOST_DOS, "gb18030", "scored", True,
        "GBK 的「图片」「通知」字节恰好是合法 UTF-8（会显示成 ͼƬ、֪ͨ）：脚本不一致守卫把它交给打分，并标 ambiguous",
        locale="zh-CN",
    ))

    fixtures.append(Fixture(
        "cp866_dos", make_entries(["Привет.txt", "Документы/Отчёт.docx"], "cp866"), HOST_DOS, "cp866", "scored", False,
        "DOS 时代俄文 PKZIP 风格：CP866 字节、made by DOS",
    ))

    fixtures.append(Fixture(
        "cp1252_western", make_entries(["résumé.pdf", "Übersicht/Käse Straße.txt"], "cp1252"), HOST_DOS, "cp1252", "scored", False,
        "西欧 Windows 工具产物：CP1252 字节、made by DOS",
    ))

    fixtures.append(Fixture(
        "ascii_only", make_entries(["readme.txt", "src/main.rs"], "utf8"), HOST_DOS, "utf8", "valid_utf8", False,
        "全部 ASCII：任何编码都一样，置信度 1",
    ))
    return fixtures


def write_outputs(out_dir: str, fixtures: list[Fixture]) -> None:
    """写 zip、manifest.json、raw_names.json。"""
    zip_dir = os.path.join(out_dir, "encoding")
    os.makedirs(zip_dir, exist_ok=True)
    manifest = []
    raw_names = {}
    for f in fixtures:
        rel = f"encoding/{f.name}.zip"
        data = build_zip(f.entries, f.host)
        with open(os.path.join(out_dir, rel), "wb") as fh:
            fh.write(data)
        record = {
            "file": rel,
            "expected_encoding": f.expected_encoding,
            "expected_source": f.expected_source,
            "expected_names": [e.utf8_name for e in f.entries],
            "ambiguous": f.ambiguous,
            "hints": {"made_by_dos": f.host == HOST_DOS, "locale": f.locale},
            "note": f.note,
        }
        if f.expected_junk:
            record["expected_junk"] = f.expected_junk
        manifest.append(record)
        raw_names[rel] = {
            "made_by": f.host,
            "entries": [
                {
                    "raw_name": list(e.raw_name),
                    "flags": e.flags,
                    "unicode_name": e.utf8_name if e.with_7075 else None,
                    "is_dir": e.is_dir,
                }
                for e in f.entries
            ],
        }
    with open(os.path.join(out_dir, "manifest.json"), "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    with open(os.path.join(out_dir, "raw_names.json"), "w", encoding="utf-8") as fh:
        json.dump(raw_names, fh, ensure_ascii=False, indent=1)
        fh.write("\n")
    print(f"wrote {len(fixtures)} fixtures to {out_dir}")


def self_check() -> None:
    """自检：CRC 实现与 zlib 一致；生成的 zip 能被标准库读回且名字字节一致。"""
    import zipfile
    import io

    sample = b"piesi corpus self check"
    assert crc32(sample) == zlib.crc32(sample), "CRC32 实现与 zlib 不一致"
    plain = make_entries(["文档/报告.docx"], "gb18030")
    data = build_zip(plain, HOST_DOS)
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        infos = zf.infolist()
        assert len(infos) == 2, infos
        # 无 bit 11、无 0x7075：zipfile 按 cp437 解码，再编回去应得到原始 GBK 字节
        assert infos[1].filename.encode("cp437") == plain[1].raw_name
        assert zf.read(infos[1]) == plain[1].data
        assert zf.testzip() is None
    with_extra = make_entries(["文档/报告.docx"], "gb18030", with_7075=True)
    with zipfile.ZipFile(io.BytesIO(build_zip(with_extra, HOST_UNIX))) as zf:
        # 新版 Python 会读 0x7075 得到 UTF-8 名，旧版仍按 cp437；两种都算结构正确
        name = zf.infolist()[1].filename
        assert name == with_extra[1].utf8_name or name.encode("cp437") == with_extra[1].raw_name, name
        assert zf.testzip() is None


def main(argv: list[str]) -> int:
    """入口：参数是输出目录。"""
    if len(argv) != 2:
        print(f"用法：{argv[0]} <输出目录>", file=sys.stderr)
        return 2
    self_check()
    write_outputs(argv[1], grid_fixtures() + special_fixtures())
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
