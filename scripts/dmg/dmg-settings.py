# dmgbuild 配置：DMG 窗口的大小、背景、图标位置和卷图标。由 scripts/package.sh 调用：
#   dmgbuild -s scripts/dmg/dmg-settings.py -D app=<Piesi.app> -D icon=<AppIcon.icns> -D background=<background.tiff> Piesi <输出.dmg>
# dmgbuild 直接写 .DS_Store，不经过 Finder / AppleScript，本机和 CI 上结果一样。
# 图标中心坐标要和 background.svg 里的箭头、文字对齐（窗口 640×400 点）。
import os.path

# defines 由 dmgbuild 注入（-D 参数）
app = defines["app"]  # noqa: F821
app_name = os.path.basename(app)

format = "ULFO"  # LZFSE 压缩，macOS 10.11+ 都能开，比 UDZO 小
filesystem = "APFS"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["icon"]  # noqa: F821  卷图标，用 app 自己的 icns
background = defines["background"]  # noqa: F821

window_rect = ((200, 140), (640, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
icon_size = 128
text_size = 13
# 不用 hide_extensions：它在 Piesi.app 上写 com.apple.FinderInfo，`codesign --verify --strict` 会判签名无效（v0.1.0 踩过）
icon_locations = {
    app_name: (160, 180),
    "Applications": (480, 180),
}
