#!/usr/bin/env python3
"""用相同输入、级别与校验方式比较创建性能；预热后记录每次耗时及输出大小。"""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import time
import zipfile
import zlib


def run(command: list[str], timeout: int) -> float:
    """执行一次实际命令，失败时保留诊断信息。"""
    started = time.perf_counter()
    result = subprocess.run(command, capture_output=True, timeout=timeout)
    elapsed = time.perf_counter() - started
    if result.returncode:
        raise RuntimeError(f"{command[0]} exit={result.returncode}: {result.stderr.decode(errors='replace')}")
    return elapsed


def fingerprint(path: Path) -> tuple[str, int]:
    """计时前单独计算源文件哈希与 CRC，避免把输入校验耗时算进压缩。"""
    sha = hashlib.sha256()
    crc = 0
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            sha.update(block)
            crc = zlib.crc32(block, crc)
    return sha.hexdigest(), crc


def main() -> None:
    """基准只读取输入文件，创建结果全部放进可自动清理的独立临时目录。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("files", type=Path, nargs="+")
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--formats", nargs="+", default=["zip"])
    parser.add_argument("--level", type=int, default=5, choices=range(10))
    parser.add_argument("--threads", type=int, default=0)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--sevenzip", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.repeats < 1 or args.threads < 0:
        parser.error("repeats 必须至少为 1，threads 不能为负数")
    versions = [("current", args.binary.resolve())]
    if args.baseline:
        versions.insert(0, ("baseline", args.baseline.resolve()))
    for _, binary in versions:
        if not binary.is_file():
            parser.error(f"找不到二进制：{binary}")
    rows = []
    with tempfile.TemporaryDirectory(prefix="piesi-bench-") as temporary:
        for source in args.files:
            source = source.resolve(strict=True)
            if not source.is_file():
                parser.error(f"该基准接收普通文件：{source}")
            sha, crc = fingerprint(source)
            for fmt in args.formats:
                runs = []
                for version, binary in versions:
                    dest = Path(temporary) / f"archive-{version}.{fmt}"
                    command = [str(binary), "a", "-q", "--level", str(args.level), "--threads", str(args.threads), str(dest), str(source)]
                    run(command, args.timeout)
                    runs.append((version, binary, dest, command, []))
                # 交错并轮换执行顺序，减少温度、缓存及后台负载随时间变化带来的单边偏差。
                for iteration in range(args.repeats):
                    order = runs if iteration % 2 == 0 else list(reversed(runs))
                    for _, _, _, command, times in order:
                        times.append(run(command, args.timeout))
                for version, binary, dest, _, times in runs:
                    run([str(binary), "t", "-q", str(dest)], args.timeout)
                    if args.sevenzip:
                        run([str(args.sevenzip.resolve()), "t", str(dest)], args.timeout)
                    if fmt == "zip":
                        with zipfile.ZipFile(dest) as archive:
                            entries = [e for e in archive.infolist() if not e.is_dir()]
                            assert len(entries) == 1
                            assert entries[0].CRC == crc and entries[0].file_size == source.stat().st_size
                    row = {
                        "input": source.name, "input_bytes": source.stat().st_size, "input_sha256": sha,
                        "format": fmt, "version": version, "binary_sha256": fingerprint(binary)[0],
                        "level": args.level, "threads": args.threads, "seconds": times,
                        "median_seconds": statistics.median(times), "output_bytes": dest.stat().st_size,
                        "verified": True,
                    }
                    rows.append(row)
                    print(f"{source.name} {fmt} {version}: {row['median_seconds']:.4f}s / {row['output_bytes']} bytes", flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({"platform": platform.platform(), "warmup": 1, "results": rows}, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
