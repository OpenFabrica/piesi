#!/usr/bin/env bash
# 打包：构建核心 → 生成工程 → Release 构建 → Developer ID 签名 → DMG → 公证 → staple → 验收。
# 签名身份与公证 profile 默认取自 ~/.claude/docs/apple-signing.md 的约定，可用环境变量覆盖：
#   APPLE_SIGN_IDENTITY   签名身份（默认 Developer ID Application: Jiangwei Lan (HQ537XMLJY)）
#   APPLE_NOTARY_PROFILE  notarytool 的 keychain profile（默认 aster-notary）
#   SKIP_NOTARIZE=1       只签名不公证（本地测试包）
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

scripts/build-core.sh
( cd apple && xcodegen generate )
rm -rf "$DIST" && mkdir -p "$DIST"
xcodebuild -project apple/Piesi.xcodeproj -scheme Piesi -configuration Release \
  -derivedDataPath apple/build \
  CODE_SIGN_IDENTITY="$IDENTITY" CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM=HQ537XMLJY \
  OTHER_CODE_SIGN_FLAGS="--timestamp" PIESI_GIT_HASH="$GIT_HASH" \
  build
APP=$(find apple/build/Build/Products/Release -maxdepth 1 -name "Piesi.app" | head -1)
[ -n "$APP" ] || { echo "找不到 Piesi.app" >&2; exit 1; }

# 深度签名：先扩展后主 app（嵌套的 appex 必须先签），都带 hardened runtime 与时间戳
find "$APP/Contents/PlugIns" -name "*.appex" -maxdepth 1 2>/dev/null | while read -r appex; do
  codesign --force --options runtime --timestamp --sign "$IDENTITY" "$appex"
done
codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

DMG="$DIST/Piesi-$PKG_VER.dmg"
STAGE="$DIST/dmg-stage"
mkdir -p "$STAGE" && cp -R "$APP" "$STAGE/" && ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Piesi" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"

if [ "${SKIP_NOTARIZE:-0}" != "1" ]; then
  xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl -a -t open --context context:primary-signature -vv "$DMG"
fi
echo "完成：$DMG"
