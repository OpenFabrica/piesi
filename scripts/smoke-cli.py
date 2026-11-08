#!/usr/bin/env python3
"""在实际目标系统黑盒验证 CLI，可用于构建产物和安装后的程序。"""

import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import zipfile


def check(binary: Path, work: Path, *args: str, success: bool = True) -> str:
    """执行 CLI 并校验成功或失败语义；输出按 UTF-8 解码。"""
    result = subprocess.run([str(binary), *args], cwd=work, capture_output=True, text=True, encoding="utf-8", timeout=120)
    if (result.returncode == 0) != success:
        raise AssertionError(f"{args[0]} 退出码 {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def main() -> None:
    """覆盖中文路径、各创建格式、加密、错误输入与路径穿越防护。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--fixtures", type=Path, default=Path(__file__).resolve().parents[1] / "crates/piesi-core/tests/fixtures")
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="piesi-smoke-") as temp:
        work = Path(temp)
        check(binary, work, "--version")
        source = work / "中文目录"
        source.mkdir()
        data = "压缩与解压测试\n".encode() * 100
        (source / "内容.txt").write_bytes(data)
        modified = 1_704_209_400
        os.utime(source / "内容.txt", (modified, modified))
        (source / "empty.txt").write_bytes(b"")
        for extension in ("zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz", "tar.zst"):
            name = f"中文压缩包.{extension}"
            check(binary, work, "a", name, source.name, "-q")
            if extension == "zip":
                with zipfile.ZipFile(work / name) as archive:
                    assert archive.getinfo("中文目录/内容.txt").date_time == datetime.datetime.fromtimestamp(modified).timetuple()[:6]
            entries = json.loads(check(binary, work, "list", name, "--json"))["entries"]
            assert any(e["name"] == "中文目录/内容.txt" for e in entries), entries
            assert "unknown" not in check(binary, work, "detect", name)
            check(binary, work, "t", name, "-q")
            destination = work / f"out-{extension}"
            check(binary, work, "x", name, "-o", str(destination), "--no-smart-folder", "-q")
            assert (destination / source.name / "内容.txt").read_bytes() == data
            assert abs((destination / source.name / "内容.txt").stat().st_mtime - modified) < 2
            assert (destination / source.name / "empty.txt").read_bytes() == b""
        for extension in ("gz", "bz2", "xz", "zst"):
            name = f"内容.txt.{extension}"
            check(binary, work, "a", name, str(source / "内容.txt"), "-q")
            check(binary, work, "t", name, "-q")
            destination = work / f"out-{extension}"
            check(binary, work, "x", name, "-o", str(destination), "--no-smart-folder", "-q")
            assert (destination / "内容.txt").read_bytes() == data
            check(binary, work, "a", f"invalid.{extension}", source.name, "-q", success=False)
        check(binary, work, "a", "invalid.tar.gz", source.name, "-p", "password", "-q", success=False)
        check(binary, work, "a", "one-thread.zip", source.name, "--threads", "1", "-q")
        check(binary, work, "t", "one-thread.zip", "-q")
        for extension in ("zip", "7z"):
            name = f"encrypted.{extension}"
            extra = ["--encrypt-names"] if extension == "7z" else []
            check(binary, work, "a", name, source.name, "-p", "密码-123", "-q", *extra)
            check(binary, work, "t", name, "-p", "wrong", "-q", success=False)
            check(binary, work, "t", name, "-p", "密码-123", "-q")
            destination = work / f"encrypted-{extension}"
            check(binary, work, "x", name, "-p", "密码-123", "-o", str(destination), "--no-smart-folder", "-q")
            assert (destination / source.name / "内容.txt").read_bytes() == data
        for version in (4, 5):
            name = f"中文归档-RAR{version}.rar"
            shutil.copyfile(args.fixtures / "rar" / f"rar{version}.rar", work / name)
            entries = json.loads(check(binary, work, "list", name, "--json"))["entries"]
            assert any(entry["name"] == "中文名.txt" for entry in entries), entries
            check(binary, work, "t", name, "-q")
            destination = work / f"rar-{version}"
            check(binary, work, "x", name, "-o", str(destination), "--no-smart-folder", "-q")
            assert (destination / "hello.txt").read_bytes() == b"hello piesi\n"
            assert (destination / "中文名.txt").read_bytes() == "中文内容\n".encode()
        with zipfile.ZipFile(work / "unsafe.zip", "w") as archive:
            archive.writestr("../escaped.txt", b"must not escape")
            archive.writestr("safe.txt", b"safe")
        destination = work / "unsafe-out"
        check(binary, work, "x", "unsafe.zip", "-o", str(destination), "--no-smart-folder", "-q")
        assert (destination / "safe.txt").read_bytes() == b"safe"
        assert not (work / "escaped.txt").exists()
        check(binary, work, "t", "missing.zip", "-q", success=False)
        (work / "broken.zip").write_bytes(b"PK\x03\x04broken")
        check(binary, work, "t", "broken.zip", "-q", success=False)
    print(f"CLI smoke passed: {binary}")


if __name__ == "__main__":
    main()
