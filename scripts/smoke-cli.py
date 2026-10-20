#!/usr/bin/env python3
"""在实际目标系统黑盒验证 CLI，可用于构建产物和安装后的程序。"""

import argparse
import json
from pathlib import Path
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
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="piesi-smoke-") as temp:
        work = Path(temp)
        check(binary, work, "--version")
        source = work / "中文目录"
        source.mkdir()
        data = "压缩与解压测试\n".encode() * 100
        (source / "内容.txt").write_bytes(data)
        (source / "empty.txt").write_bytes(b"")
        for extension in ("zip", "7z", "tar.gz", "tar.xz", "tar.zst"):
            name = f"中文压缩包.{extension}"
            check(binary, work, "a", name, source.name, "-q")
            entries = json.loads(check(binary, work, "list", name, "--json"))["entries"]
            assert any(e["name"] == "中文目录/内容.txt" for e in entries), entries
            assert "unknown" not in check(binary, work, "detect", name)
            check(binary, work, "t", name, "-q")
            destination = work / f"out-{extension}"
            check(binary, work, "x", name, "-o", str(destination), "--no-smart-folder", "-q")
            assert (destination / source.name / "内容.txt").read_bytes() == data
            assert (destination / source.name / "empty.txt").read_bytes() == b""
        for extension in ("zip", "7z"):
            name = f"encrypted.{extension}"
            extra = ["--encrypt-names"] if extension == "7z" else []
            check(binary, work, "a", name, source.name, "-p", "密码-123", "-q", *extra)
            check(binary, work, "t", name, "-p", "wrong", "-q", success=False)
            check(binary, work, "t", name, "-p", "密码-123", "-q")
            destination = work / f"encrypted-{extension}"
            check(binary, work, "x", name, "-p", "密码-123", "-o", str(destination), "--no-smart-folder", "-q")
            assert (destination / source.name / "内容.txt").read_bytes() == data
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
