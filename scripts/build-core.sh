#!/usr/bin/env bash
# scripts/build-core.sh —— 把 Rust 引擎（piesi-ffi）编成 Swift 能直接链接的 apple/PiesiCore.xcframework。
#
# 每一步为什么这样做：
# 1. cargo build piesi-ffi（crate-type 含 staticlib + cdylib），默认 arm64 + x86_64 两个架构：
#    直接分发（Developer ID）的 app 要通用二进制；staticlib 进 app，cdylib 只给 uniffi-bindgen 读导出元数据。
#    用独立的 CARGO_TARGET_DIR=target/apple，避免和 cargo test / 其他并行构建抢文件锁、互相清缓存。
# 2. uniffi-bindgen 用 --library 模式从 dylib 的符号里读导出表，生成 piesi_ffi.swift、piesi_ffiFFI.h、
#    piesi_ffiFFI.modulemap 到 apple/Generated（不入库）。用 workspace 里的 bin 而不是全局安装的 uniffi-bindgen，
#    因为 uniffi 要求 bindgen 和 crate 严格同版本。
# 3. 头文件和 modulemap 放进 headers 目录（modulemap 必须叫 module.modulemap，clang 只认这个名字）；
#    lipo 把两个架构的 .a 合成一个；xcodebuild -create-xcframework 产出 xcframework。
#    Xcode 链接 xcframework 时会自动把 headers 目录加进搜索路径，生成的 Swift 里 `import piesi_ffiFFI` 才找得到。
#
# 用法：scripts/build-core.sh [--arch all|native] [--profile release|debug]
#   --arch native     只编本机架构（开发时快），默认 all = arm64 + x86_64。
#   --profile debug   用 debug profile（省掉 LTO 时间），默认 release。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# 统一的报错出口：信息打到 stderr 并退出，让失败原因一眼可见
die() { echo "build-core: 错误：$*" >&2; exit 1; }
log() { echo "build-core: $*"; }

ARCH_MODE=all
PROFILE=release
while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch) [[ $# -ge 2 ]] || die "--arch 需要参数 all|native"; ARCH_MODE="$2"; shift 2 ;;
    --arch=*) ARCH_MODE="${1#*=}"; shift ;;
    --profile) [[ $# -ge 2 ]] || die "--profile 需要参数 release|debug"; PROFILE="$2"; shift 2 ;;
    --profile=*) PROFILE="${1#*=}"; shift ;;
    -h|--help) sed -n '2,17p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) die "未知参数：${1}（支持 --arch all|native、--profile release|debug）" ;;
  esac
done

# 优先用 rustup 管的工具链（~/.cargo/bin）：Homebrew 的 rust 只带本机架构的 std，编不了 x86_64-apple-darwin
if [[ -x "$HOME/.cargo/bin/cargo" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null || die "找不到 cargo，请先安装 Rust（https://rustup.rs）"
command -v xcodebuild >/dev/null || die "找不到 xcodebuild，请安装 Xcode 并执行 xcode-select"

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
case "$ARCH_MODE" in
  native) TRIPLES=("$HOST_TRIPLE") ;;
  all) TRIPLES=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  *) die "--arch 只能是 all 或 native，收到：${ARCH_MODE}" ;;
esac
case "$PROFILE" in
  release) CARGO_PROFILE_FLAG=(--release) ;;
  debug) CARGO_PROFILE_FLAG=() ;;
  *) die "--profile 只能是 release 或 debug，收到：${PROFILE}" ;;
esac

# 独立 target 目录，和 cargo test（target/）隔开；允许外部覆盖
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/apple}"
SYSROOT="$(rustc --print sysroot)"
for t in "${TRIPLES[@]}"; do
  [[ -d "$SYSROOT/lib/rustlib/$t/lib" ]] || die "当前工具链（${SYSROOT}）没有目标 ${t} 的 std，执行：rustup target add ${t}"
done

# ---------- 1. 编译 Rust ----------
# 注意：/bin/bash 是 3.2，双引号里 $var 后面紧跟中文会解析错，统一写 ${var}；空数组展开用 ${a[@]+"${a[@]}"}
for t in "${TRIPLES[@]}"; do
  log "编译 piesi-ffi（${t}，${PROFILE}）"
  cargo build -p piesi-ffi ${CARGO_PROFILE_FLAG[@]+"${CARGO_PROFILE_FLAG[@]}"} --target "$t"
done

# ---------- 2. 生成 Swift 绑定 ----------
# --library 模式只需要读元数据，任一架构的 dylib 都行；用本机架构的，和 cargo run 共用同一套编译产物
HOST_DYLIB="$CARGO_TARGET_DIR/$HOST_TRIPLE/$PROFILE/libpiesi_ffi.dylib"
[[ -f "$HOST_DYLIB" ]] || die "没找到 ${HOST_DYLIB}，确认 piesi-ffi 的 crate-type 含 cdylib"
GENERATED="$ROOT/apple/Generated"
rm -rf "$GENERATED"
mkdir -p "$GENERATED"
log "生成 Swift 绑定 → apple/Generated"
# --target 传本机三元组是为了复用上面已编好的依赖，否则 cargo run 会在 target/apple/<profile> 再编一遍整个依赖树
cargo run -p piesi-ffi ${CARGO_PROFILE_FLAG[@]+"${CARGO_PROFILE_FLAG[@]}"} --target "$HOST_TRIPLE" --bin uniffi-bindgen -- \
  generate --library "$HOST_DYLIB" --language swift --out-dir "$GENERATED" --no-format
for f in piesi_ffi.swift piesi_ffiFFI.h piesi_ffiFFI.modulemap; do
  [[ -f "$GENERATED/$f" ]] || die "uniffi-bindgen 没有产出 ${f}（检查 uniffi 版本是否和 crate 一致）"
done

# ---------- 3. 打 XCFramework ----------
STAGE="$CARGO_TARGET_DIR/xcframework"
rm -rf "$STAGE"
mkdir -p "$STAGE/headers"
cp "$GENERATED/piesi_ffiFFI.h" "$STAGE/headers/"
cp "$GENERATED/piesi_ffiFFI.modulemap" "$STAGE/headers/module.modulemap"

LIBS=()
for t in "${TRIPLES[@]}"; do
  lib="$CARGO_TARGET_DIR/$t/$PROFILE/libpiesi_ffi.a"
  [[ -f "$lib" ]] || die "没找到 ${lib}"
  LIBS+=("$lib")
done
if [[ ${#LIBS[@]} -gt 1 ]]; then
  log "lipo 合并 ${#LIBS[@]} 个架构"
  lipo -create "${LIBS[@]}" -output "$STAGE/libpiesi_ffi.a"
else
  cp "${LIBS[0]}" "$STAGE/libpiesi_ffi.a"
fi

OUT="$ROOT/apple/PiesiCore.xcframework"
# -create-xcframework 遇到已存在的输出会直接报错，先删掉保证脚本可重复执行
rm -rf "$OUT"
log "生成 ${OUT}"
xcodebuild -create-xcframework \
  -library "$STAGE/libpiesi_ffi.a" -headers "$STAGE/headers" \
  -output "$OUT"

log "完成。架构：$(lipo -archs "$STAGE/libpiesi_ffi.a")；Generated：$(ls "$GENERATED" | tr '\n' ' ')"
