#!/usr/bin/env python3
"""在本机为已构建的 CLI 生成安装包与便携包；失败时不产出校验清单。"""

import argparse
import hashlib
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile


def run(*args: str, **kwargs) -> str:
    """执行打包工具，保留失败退出码并返回标准输出。"""
    try:
        return subprocess.check_output(args, text=True, **kwargs).strip()
    except subprocess.CalledProcessError as error:
        print(error.output, file=sys.stderr)
        raise


def package_linux(root: Path, stage: Path, out: Path, version: str) -> list[Path]:
    """生成依赖由 dpkg/rpm 自动分析的 amd64 安装包及便携 tar.gz。"""
    portable = out / f"piesi-cli-{version}-linux-x86_64.tar.gz"
    with tarfile.open(portable, "w:gz") as archive:
        archive.add(stage, arcname=f"piesi-cli-{version}")

    package = stage.parent / "deb"
    (package / "usr/bin").mkdir(parents=True)
    shutil.copy2(stage / "piesi", package / "usr/bin/piesi")
    docs = package / "usr/share/doc/piesi-cli"
    shutil.copytree(stage, docs, ignore=shutil.ignore_patterns("piesi"))
    control = package / "DEBIAN"
    control.mkdir()
    # dpkg-shlibdeps 需要源包元信息；使用与实际安装包一致的字段。
    source = stage.parent / "source/debian"
    source.mkdir(parents=True)
    (source / "control").write_text(
        "Source: piesi-cli\nMaintainer: OpenFabrica\n\nPackage: piesi-cli\n"
        "Architecture: amd64\nDescription: Piesi archive command-line tool\n",
        encoding="utf-8",
    )
    deps = run("dpkg-shlibdeps", "-O", f"-e{package / 'usr/bin/piesi'}", cwd=source.parent)
    depends = next(line.removeprefix("shlibs:Depends=") for line in deps.splitlines() if line.startswith("shlibs:Depends="))
    (control / "control").write_text(
        f"Package: piesi-cli\nVersion: {version}\nArchitecture: amd64\n"
        f"Maintainer: OpenFabrica\nDepends: {depends}\nSection: utils\nPriority: optional\n"
        f"Installed-Size: {sum(p.stat().st_size for p in package.rglob('*') if p.is_file()) // 1024}\n"
        "Homepage: https://github.com/OpenFabrica/piesi\n"
        "Description: Piesi archive command-line tool\n"
        " Create, list, test and extract ZIP, 7z, RAR and tar archives.\n",
        encoding="utf-8",
    )
    deb = out / f"piesi-cli_{version}_amd64.deb"
    run("dpkg-deb", "--root-owner-group", "--build", str(package), str(deb))

    rpm_root = stage.parent / "rpm"
    rpm_root.mkdir()
    spec = rpm_root / "piesi.spec"
    spec.write_text(
        f"Name: piesi-cli\nVersion: {version.replace('-', '.')}\nRelease: 1\n"
        "Summary: Piesi archive command-line tool\n"
        "License: (MIT OR Apache-2.0) AND LicenseRef-unRAR\n"
        "URL: https://github.com/OpenFabrica/piesi\n"
        "%description\nCreate, list, test and extract ZIP, 7z, RAR and tar archives.\n"
        "%install\nmkdir -p \"%{buildroot}/usr\"\n"
        f'cp -a "{package}/usr/." "%{{buildroot}}/usr/"\n'
        "%files\n/usr/bin/piesi\n%doc /usr/share/doc/piesi-cli\n",
        encoding="utf-8",
    )
    run("rpmbuild", "--define", f"_topdir {rpm_root}", "--define", "_build_id_links none", "-bb", str(spec))
    built, = rpm_root.glob("RPMS/x86_64/*.rpm")
    rpm = out / f"piesi-cli-{version}-1.x86_64.rpm"
    shutil.copy2(built, rpm)
    return [portable, deb, rpm]


def package_windows(root: Path, stage: Path, out: Path, version: str) -> list[Path]:
    """生成免管理员权限的 Inno Setup 安装程序与便携 ZIP。"""
    portable = out / f"piesi-cli-{version}-windows-x86_64.zip"
    with zipfile.ZipFile(portable, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(stage.iterdir()):
            archive.write(path, path.name)
    compiler = shutil.which("iscc") or str(Path(os.environ["ProgramFiles(x86)"]) / "Inno Setup 6/ISCC.exe")
    run(compiler, f"/DAppVersion={version}", f"/DStageDir={stage}", f"/DOutputDir={out}", str(root / "scripts/cli-installer.iss"))
    installer = out / f"piesi-cli-{version}-windows-x86_64-setup.exe"
    if not installer.is_file():
        raise FileNotFoundError(installer)
    return [portable, installer]


def main() -> None:
    """校验平台、版本与二进制后打包；清单只包含本轮成功生成的文件。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, default=Path("dist/cli"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    binary, out = args.binary.resolve(), args.out.resolve()
    actual = run(str(binary), "--version")
    if actual != f"piesi {version}":
        raise ValueError(f"版本不一致：{actual!r}，源码版本 {version}")
    system = platform.system()
    if system not in ("Windows", "Linux") or platform.machine().lower() not in ("amd64", "x86_64"):
        raise ValueError("此安装包流程只支持 Windows/Linux x86_64 本机构建")
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="piesi-package-") as temp:
        stage = Path(temp) / "stage"
        stage.mkdir()
        shutil.copy2(binary, stage / ("piesi.exe" if system == "Windows" else "piesi"))
        for name in ("LICENSE-MIT", "LICENSE-APACHE", "LICENSE-THIRD-PARTY"):
            shutil.copy2(root / name, stage / name)
        shutil.copy2(root / "docs/cli.md", stage / "README.md")
        artifacts = (package_windows if system == "Windows" else package_linux)(root, stage, out, version)
    manifest = out / f"SHA256SUMS-{system.lower()}-x86_64.txt"
    checksums = []
    for artifact in artifacts:
        with artifact.open("rb") as stream:
            checksums.append(f"{hashlib.file_digest(stream, 'sha256').hexdigest()}  {artifact.name}\n")
    # Windows 也固定使用 LF，保证合并后的清单可直接交给 Linux sha256sum。
    manifest.write_text("".join(checksums), encoding="utf-8", newline="\n")
    for artifact in artifacts:
        print(artifact)
    print(manifest)


if __name__ == "__main__":
    main()
