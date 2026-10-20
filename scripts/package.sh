#!/usr/bin/env bash
# 打包：构建核心 → 生成工程 → Release 构建 → Developer ID 签名 → DMG → 公证 → staple → 验收。
# 签名身份与公证 profile 默认取自 ~/.claude/docs/apple-signing.md 的约定，可用环境变量覆盖：
#   APPLE_SIGN_IDENTITY   签名身份（默认 Developer ID Application: Jiangwei Lan (HQ537XMLJY)）
#   APPLE_NOTARY_PROFILE  notarytool 的 keychain profile（默认 aster-notary）
#   SKIP_NOTARIZE=1       只签名不公证（本地测试包）
#   APPCAST=1             不发版也生成 dist/appcast.xml（演练自动更新；APPCAST_URL_PREFIX 可换下载地址前缀）
#   RELEASE=1             发版门禁（docs/release.md 第 6 条）：拒绝开发标记、版本与标签不一致、未提交改动
#   APPLE_NOTARY_KEY      CI 用 App Store Connect API Key 公证：.p8 路径，另需 APPLE_NOTARY_KEY_ID、APPLE_NOTARY_ISSUER；
#                         设了就不经钥匙串 profile
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
IDENTITY=${APPLE_SIGN_IDENTITY:-"Developer ID Application: Jiangwei Lan (HQ537XMLJY)"}
PROFILE=${APPLE_NOTARY_PROFILE:-aster-notary}
DIST="$ROOT/dist"

# 版本号：Rust 与 Xcode 两处必须一致（发版规范第 2 节）
RUST_VER=$(grep -E '^version = ' Cargo.toml | head -1 | sed -E 's/version = "(.*)"/\1/')
XC_VER=$(grep -E '^MARKETING_VERSION' apple/Config/Version.xcconfig | sed -E 's/.*= *//')
XC_SUFFIX=$(grep -E '^VERSION_SUFFIX' apple/Config/Version.xcconfig | sed -E 's/.*= *//' || true)
if [ "$RUST_VER" != "${XC_VER}${XC_SUFFIX}" ]; then
  echo "版本号不一致：Cargo.toml=$RUST_VER  Version.xcconfig=${XC_VER}${XC_SUFFIX}" >&2
  exit 1
fi
GIT_HASH=$(git rev-parse --short HEAD)
DIRTY=$(git status --porcelain --untracked-files=no | grep -q . && echo "+" || true)
case "$RUST_VER" in
  *-dev*) PKG_VER="${RUST_VER}-${GIT_HASH}${DIRTY}" ;;
  *) PKG_VER="$RUST_VER" ;;
esac
echo "打包版本：${PKG_VER}（签名：${IDENTITY}）"

# 发版门禁：任一不过就停。本地打测试包（不设 RELEASE）不走这里。
if [ "${RELEASE:-0}" = "1" ]; then
  case "$RUST_VER" in *-dev*) echo "发版包不能带开发标记：${RUST_VER}" >&2; exit 1 ;; esac
  TAG="${GITHUB_REF_NAME:-$(git describe --tags --exact-match 2>/dev/null || true)}"
  [ "$TAG" = "v$RUST_VER" ] || { echo "标签「${TAG}」与版本 v${RUST_VER} 不一致" >&2; exit 1; }
  [ -z "$DIRTY" ] || { echo "有未提交改动，不能发版" >&2; exit 1; }
  # 标签必须在主分支上（CI 里 origin/main 存在时才查）
  if git rev-parse --verify -q origin/main >/dev/null 2>&1; then
    git merge-base --is-ancestor "$TAG" origin/main || { echo "标签 ${TAG} 不在 origin/main 上" >&2; exit 1; }
  fi
fi

scripts/build-core.sh
( cd apple && xcodegen generate )
rm -rf "$DIST" && mkdir -p "$DIST"
# CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO：xcodebuild build 默认给产物注入调试用的 get-task-allow，公证会拒；
# 下面重签保留 entitlements，所以要在这里就不让它进去
xcodebuild -project apple/Piesi.xcodeproj -scheme Piesi -configuration Release \
  -derivedDataPath apple/build \
  CODE_SIGN_IDENTITY="$IDENTITY" CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM=HQ537XMLJY \
  CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO \
  OTHER_CODE_SIGN_FLAGS="--timestamp" PIESI_GIT_HASH="$GIT_HASH" \
  build
APP=$(find apple/build/Build/Products/Release -maxdepth 1 -name "Piesi.app" | head -1)
[ -n "$APP" ] || { echo "找不到 Piesi.app" >&2; exit 1; }

# Sparkle：框架里的辅助程序（XPC 服务、Autoupdate、Updater.app）要由里到外逐个用 Developer ID + hardened runtime 重签。
# Xcode 嵌入时只签框架外层，里面还是 Sparkle 官方的签名，公证会拒（Sparkle 文档「Distributing your app」）
SPARKLE_FW="$APP/Contents/Frameworks/Sparkle.framework"
for item in "$SPARKLE_FW"/Versions/B/XPCServices/*.xpc "$SPARKLE_FW/Versions/B/Autoupdate" "$SPARKLE_FW/Versions/B/Updater.app" "$SPARKLE_FW"; do
  [ -e "$item" ] || continue
  codesign --force --options runtime --timestamp --preserve-metadata=entitlements --sign "$IDENTITY" "$item"
done

# 深度签名：先扩展后主 app（嵌套的 appex 必须先签），都带 hardened runtime 与时间戳。
# 必须保留 Xcode 签进去的 entitlements：重签不带它会被清空，appex 没了沙盒系统就不加载，App Group 交接也会断
find "$APP/Contents/PlugIns" -name "*.appex" -maxdepth 1 2>/dev/null | while read -r appex; do
  codesign --force --options runtime --timestamp --preserve-metadata=entitlements --sign "$IDENTITY" "$appex"
done
codesign --force --options runtime --timestamp --preserve-metadata=entitlements --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

DMG="$DIST/Piesi-$PKG_VER.dmg"
# DMG 窗口（背景、图标位置、卷图标）由 dmgbuild 直接写 .DS_Store，配置在 scripts/dmg/；
# dmgbuild 及其依赖锁版本，装在 target/ 下的 venv 里，不碰系统 Python
VENV="$ROOT/target/dmgbuild-venv"
if [ ! -x "$VENV/bin/dmgbuild" ]; then
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install -q "dmgbuild==1.6.7" "ds_store==1.3.3" "mac_alias==2.2.3"
fi
"$VENV/bin/dmgbuild" -s scripts/dmg/dmg-settings.py \
  -D app="$APP" -D icon="$APP/Contents/Resources/AppIcon.icns" -D background="$ROOT/scripts/dmg/background.tiff" \
  "Piesi" "$DMG"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"
# DMG 里的 app 再做一次严格签名校验：dmgbuild 之类的工具可能给 app 加扩展属性（FinderInfo 等），
# 普通校验和 Gatekeeper 都放行，但 --strict 不过，Sparkle 装更新时也可能卡住
DMG_MNT=$(mktemp -d)
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$DMG_MNT" "$DMG"
codesign --verify --deep --strict "$DMG_MNT/Piesi.app" || { hdiutil detach -quiet "$DMG_MNT"; echo "DMG 里的 Piesi.app 严格签名校验没过" >&2; exit 1; }
hdiutil detach -quiet "$DMG_MNT"

if [ "${SKIP_NOTARIZE:-0}" != "1" ]; then
  if [ -n "${APPLE_NOTARY_KEY:-}" ]; then
    # CI：App Store Connect API Key（.p8 文件 + key id + issuer），runner 上没有钥匙串 profile
    xcrun notarytool submit "$DMG" --key "$APPLE_NOTARY_KEY" --key-id "${APPLE_NOTARY_KEY_ID:?缺 APPLE_NOTARY_KEY_ID}" \
      --issuer "${APPLE_NOTARY_ISSUER:?缺 APPLE_NOTARY_ISSUER}" --wait
  else
    xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait
  fi
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl -a -t open --context context:primary-signature -vv "$DMG"
fi

# 自动更新清单（设计 6.6）：发版（RELEASE=1）或 APPCAST=1 时，用 Sparkle 的 generate_appcast 给最终的 DMG
# 做 EdDSA 签名，写 dist/appcast.xml（只含这一版，Sparkle 只要最新一条）。必须在公证 + staple 之后，签的是最终字节。
# 私钥：CI 用 SPARKLE_ED_KEY_FILE（Secret 写出的文件）；本机用登录钥匙串里账户 piesi 的那把（generate_keys --account piesi）。
# 本版更新说明取 CHANGELOG.md 顶部小节，写成与 DMG 同名的 .md，嵌进 appcast
if [ "${RELEASE:-0}" = "1" ] || [ "${APPCAST:-0}" = "1" ]; then
  SPARKLE_BIN="$ROOT/apple/build/SourcePackages/artifacts/sparkle/Sparkle/bin"
  if [ -f CHANGELOG.md ]; then
    awk 'BEGIN{n=0} /^## /{n++} n==1{print}' CHANGELOG.md > "${DMG%.dmg}.md"
    [ -s "${DMG%.dmg}.md" ] || rm -f "${DMG%.dmg}.md"
  fi
  APPCAST_ARGS=(--download-url-prefix "${APPCAST_URL_PREFIX:-https://github.com/OpenFabrica/piesi/releases/download/v$RUST_VER/}" --embed-release-notes -o "$DIST/appcast.xml")
  if [ -n "${SPARKLE_ED_KEY_FILE:-}" ]; then
    APPCAST_ARGS+=(--ed-key-file "$SPARKLE_ED_KEY_FILE")
  else
    APPCAST_ARGS+=(--account piesi)
  fi
  "$SPARKLE_BIN/generate_appcast" "${APPCAST_ARGS[@]}" "$DIST"
  echo "appcast：$DIST/appcast.xml"
fi
echo "完成：$DMG"
