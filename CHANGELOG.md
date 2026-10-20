# 更新日志

未发布的改动不写在这里；发版时按 `docs/release.md` 在顶部加一节。

## 0.1.1 · 2026-10-20 · beta（Windows / Linux CLI）

- 新增 Windows x64 命令行安装程序与便携 ZIP；支持当前用户安装、添加 PATH 与卸载清理
- 新增 Linux x86_64 的 DEB、RPM 安装包及 tar.gz 便携包，提供 SHA-256 校验清单
- Windows / Linux 与 macOS 共用压缩引擎，支持中文文件名、压缩、解压、加密与校验
- 修正 Windows 的 Unix 权限依赖与符号链接创建，保留安全解压边界
- 安装包经过 Windows、Ubuntu 和 Fedora 容器中的实际安装、命令执行与卸载验证
- 此次只发布命令行版；Windows 需要 Windows 10/11 x64，Linux 需要 glibc 2.35 或更新版本
- 已知限制：Windows 上 RAR4 符号链接会跳过并告警（上游 unrar 平台限制），普通文件和 RAR5 链接可正常处理

## 0.1.0 · 2026-10-20 · beta

- 双击压缩包直接解压到旁边；按住 ⌥ 双击先浏览内容
- 支持解压 zip、7z、rar、tar、gz、bz2、xz、zst，包括分卷和加密包
- 自动识别文件名编码（GBK、Big5、Shift_JIS 等），中文文件名不乱码；也可以手动切换
- 浏览窗口：搜索、空格预览、拖出解压、校验完整性
- 新建压缩包：「发给 Windows」「最小体积」「加密（AES-256）」「Mac 专用」四个预设
- 默认不写 __MACOSX 和 .DS_Store，在 Windows 上打开不乱码
- Finder 右键菜单直接「用 Piesi 解压到此处 / 查看 / 压缩」（要先在系统设置里打开 Piesi 扩展）
- 解压后在 Finder 里选中结果；同名文件夹已存在时，可选新建「名字-2」或解压进去
- 在 Finder 里按空格，能直接预览压缩包里的文件列表
- 快捷指令：「用 Piesi 解压」「用 Piesi 压缩」
- 自动更新
- 界面支持简体中文、繁体中文、English、日本語
