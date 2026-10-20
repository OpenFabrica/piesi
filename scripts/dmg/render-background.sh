#!/usr/bin/env bash
# 把 background.svg 渲染成 1x / 2x PNG，再合成 Finder 认的多分辨率 background.tiff（需要 brew install librsvg）。
set -euo pipefail
cd "$(dirname "$0")"
TMP=$(mktemp -d)
rsvg-convert -w 640 -h 400 background.svg -o "$TMP/bg.png"
rsvg-convert -w 1280 -h 800 background.svg -o "$TMP/bg@2x.png"
tiffutil -cathidpicheck "$TMP/bg.png" "$TMP/bg@2x.png" -out background.tiff
rm -rf "$TMP"
echo "已生成 $(pwd)/background.tiff"
