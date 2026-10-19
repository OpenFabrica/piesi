# Piesi 设计方案：现代原生 Mac 压缩/解压应用（核心可跨平台）

## 1. 背景

用户要做一个压缩/解压应用。用户给的调研指出：macOS 自带归档工具有坑（中文乱码、塞 `__MACOSX`/`.DS_Store`、格式少、不能加密分卷），第三方工具各有短板（Keka 不能浏览、BetterZip 贵、Unarchiver 只解压）。
本计划补充了 2026-10 的核实调研，经一轮独立架构评审后给出最终设计。目录 `/Users/mike/source/project/tools/Piesi` 目前为空，还不是 git 仓库。

## 2. 补充调研结论（2026-10-19 核实）

### 2.1 竞品现状

| 产品 | 版本 / 价格 | 现状要点 |
|---|---|---|
| Keka | 1.6.8（2026-09），官网免费 / MAS $4.99 | 靠 bundled `7zz`/`unrar`/`bsdtar` 子进程；**没有压缩包浏览**；ZIP 加密只有老 ZipCrypto；7z 用 AES-256 |
| BetterZip | 6.0（2026-07），$35 买断，30 天试用 | 已换 Liquid Glass 新界面、图标/分栏视图、Shortcuts/App Intents、Spotlight 索引包内文件、免费 Quick Look 扩展、「Windows 友好改名」选项。浏览/编辑这块的最强对手 |
| The Unarchiver | 4.3.9（2025-03），免费 | 只解压、停滞；编码识别好（Mozilla universalchardet）；曾出过「解压文件没打 quarantine」的 CVE-2023-46270 |
| MacZip（原 eZip） | 2.4（2024-02），免费 | 面向中文用户、能浏览编辑、可手动选编码；20 个月没更新，基本停更 |
| MAS 国产小工具 | ¥8–58 | 全能解压王 Pro、OmniZip Pro、易压、解压专家 Oka 等，质量参差 |
| Windows 11 自带 | 24H2 | 资源管理器已能创建 zip/7z/tar、解压 rar（libarchive）；**不能加密**；识别 ZIP 的 UTF-8 标志位 |
| macOS 26 自带 | 归档工具无变化 | 创建只有 zip；能展开 zip/tar/gz/bz2/xz/cpio，不支持 rar/7z；26.4 修了归档工具的沙盒逃逸 CVE-2026-28910 |

### 2.2 本机实证：乱码的真正来源

用本机已有的 ZIP 读了文件头（只读）：

| 生成方 | 文件名字节 | UTF-8 标志（bit 11） | 结果 |
|---|---|---|---|
| macOS Finder / ditto（版本号 3.0，Unix） | UTF-8 | **没设** | Windows 资源管理器按本地代码页读 → 乱码 |
| WinRAR 6.x（中文 Windows） | GBK | 没设，但带 0x7075 UTF-8 扩展字段 | macOS 归档工具按 UTF-8 读 → 乱码或报错 |
| 现代工具（7-Zip 新版等） | UTF-8 | 设了（0x808） | 两边都正常 |

结论：写 ZIP 时「UTF-8 + 设 bit 11 + NFC 归一化」就能让 Windows 正常显示；读 ZIP 时「bit 11 → 0x7075 → 合法 UTF-8 → 候选解码打分」四级判断能覆盖绝大多数来源。P0 要在 macOS 26 上用 `ditto` 再复核一次 Finder 仍不写 bit 11（本机样本可能来自旧系统）。

### 2.3 技术底座可用性（已核实）

- Rust `zip` 9.0.5（MIT）：AES 读写、zip64、流式、追加写入，提供 `name_raw()`、`flags()`、`extra_data_fields()`。**不支持 PKWARE 多盘分卷（`.z01`）**；两年内大版本 2→9，append/zip64 出过回归 → 只用它**写**，锁死版本。
- Rust `sevenz-rust2` 0.24.1（Apache-2.0）：7z 读写、AES-256、**全部 BCJ 过滤器（x86/ARM/ARM64/RISC-V/PPC/SPARC/IA64）读写 + BCJ2 读**、Delta、PPMd。实现时核实：`Lzma2Options::from_level_mt(level, threads, chunk_size)` 已支持多线程分块压缩，评审说的「没有多线程」只看了首页。
- Rust `zesven` 3.2.0（2026-10，MIT/Apache）：多线程、AES、多卷，但**文档里完全没有 BCJ/BCJ2**。7-Zip 给 exe/dll 默认加 BCJ、高压缩级别用 BCJ2，国内软件包 .7z 大量命中 → 不能当主引擎，只做基准参照。
- `liblzma` crate（0BSD，可静态链接）：`lzma_raw_encoder/decoder` + LZMA2 过滤器输出的就是 7z packed stream 位流；tar.xz 本来就要它。LZMA SDK 只有编解码器没有 7z 容器写入器，不选。
- Rust `unrar` 0.5.8：封装 RARLAB unrar C++ 库，只能列出/解压，**顺序处理模型**（选择性解压 = 遍历 + 跳过）；许可允许商用，禁止用来做 RAR 压缩（全世界只有 WinRAR 能创建 RAR）。
- `chardetng` 1.0（Mozilla）：面向网页文档，短输入下 GBK/Big5 互相「合法」会误判 → 只当一票，主判断用候选解码 + 字频打分（见 5.2）。
- `cap-std`（bytecodealliance）：能力式文件系统 `Dir`，系统调用层拦截 `..`、绝对路径、symlink 穿越。
- libarchive：macOS SDK 不带头文件，自建 3.8 要拖 zlib/bz2/lzma/zstd/lz4/libxml2/iconv（Keka 回退 bsdtar 3.8.8 就是这种折腾）；cab/iso/lha 对目标用户 <1% → **MVP 不碰**，v1.x 用系统自带 `/usr/bin/bsdtar` 子进程只读兜底。
- UniFFI 0.3x：Rust → Swift 绑定，支持 `with_foreign` trait 回调；`cargo-swift` 产出 XCFramework；`uniffi-bindgen-cs` 产出 C# 绑定（给以后的 Windows 版）。
- SwiftUI `Table` 在 macOS 上一两千行就能卡十几秒（Apple 论坛多处报告，15.5 和 26 更差）→ 列表用 `NSOutlineView`。
- 用户已有 Rust + Apple 签名公证经验（MailFabrica 用 `cargo xtask package`），Rust 核心方案可行。

## 3. 产品定位

一句话：**默认干净、中文不乱码、能像 Finder 一样浏览压缩包的现代原生 Mac 压缩工具，核心引擎可跨平台，开源。**

主要用户：和 Windows 用户互传文件的中文 Mac 用户（设计、开发、办公）。

和对手的差异：
- 比 Keka：能浏览、能编辑、ZIP 用 AES、编码能自动识别也能手动切、界面现代。
- 比 BetterZip：免费开源、中文编码是一等功能（不是改名选项）、默认就干净、有 Windows 版计划。
- 比 Unarchiver：能压缩、能浏览、开源可审计。
- 比 MacZip：在维护、安全（quarantine、zip-slip 防护）、现代界面。

## 4. 功能范围

### v1.0（MVP + 打磨）
- 解压：zip（含 `.zip.001` 字节分卷、`.z01` PKWARE 分卷）、7z（含 BCJ/BCJ2/ARM64/Delta、solid、头加密、`.001` 分卷）、rar（v4/v5、分卷）、tar 及 tar.gz/.bz2/.xz/.zst、单文件 gz/bz2/xz/zst。
- 压缩：zip（deflate；可选 store）、7z（LZMA2，多线程）、tar.gz / tar.xz / tar.zst。
- 浏览：zip/7z/rar 打开即看（只读目录区），tar.* 打开即显示索引进度；列表/大纲、搜索、排序、面包屑、按条目 Quick Look 预览、选择性解压、拖出即解压。
- 编码：自动识别 + 顶栏常驻选择器（带 5 个样本名实时预览），切换只重解码。
- 干净压缩：默认排除 `__MACOSX`、`.DS_Store`、`._*`、`Thumbs.db`、`desktop.ini`；写 UTF-8 + bit 11 + NFC。
- 加密：ZIP AES-256（打开 ZipCrypto 包时提示不安全）；7z AES-256 + 可选加密文件名。
- 安全：cap-std 输出、zip-slip、符号链接逃逸、压缩炸弹预警、磁盘空间预检、quarantine 传播、255 字节文件名截断、大小写冲突预检。
- 系统集成：设为默认解压工具、Finder 右键 Services（压缩 / 解压到此处 / 用 Piesi 查看）、拖到 Dock 图标、Shortcuts（App Intents）、Quick Look 扩展（只读索引）。
- 冲突处理：覆盖 / 跳过 / 重命名 / 合并；智能建文件夹（包内多个顶层项才建）。
- CLI `piesi`（供自动化与语料回归测试）。
- 本地化：zh-Hans、zh-Hant、en、ja。

### v1.x
- 编辑压缩包：Overlay 暂存模型（增、删、改名先落内存，Save 时写临时文件 → fsync → 原子替换）；zip 追加走 `new_append`，删除/改名重写时用 `raw_copy_file` 免重压；7z 编辑 = 整包重写；RAR 编辑 = 「转存为 zip/7z」。顺带得到撤销。
- 压缩预设与自动化（预设卡片、Shortcuts 动作、压缩完自动跑 Shortcut）。
- 测试压缩包、修复 zip（扫描本地文件头重建中央目录）。
- 「修复文件名编码并保存」：GBK 旧包 → UTF-8 + bit 11 + NFC，条目数据原样拷贝零重压（评审提出，顺着 Overlay 重写路径免费得到）。
- 双击直接解压后若编码置信度低，完成通知里给「用其他编码重新解压…」入口，并记录本次用的编码。
- 任务队列窗口、批量操作。
- 长尾格式（iso/cab/xar/cpio/lha）经系统 `/usr/bin/bsdtar` 只读兜底。
- Mac App Store 版（沙盒，只需在 App 层加 security-scoped bookmark）。

### v2
- Windows 版：同一 Rust 核心 + WinUI 3（C#，`uniffi-bindgen-cs`），资源管理器右键（IExplorerCommand，稀疏 MSIX）。

## 5. 架构

```
┌───────────────────────────────┐   ┌──────────────────────────┐
│ macOS app（SwiftUI + AppKit）  │   │ Windows app（WinUI 3）v2  │
│ Quick Look 扩展 / Services     │   │ Explorer 右键扩展          │
└───────────────┬───────────────┘   └─────────────┬────────────┘
                │ UniFFI（Swift）                  │ UniFFI（C#）
┌───────────────┴─────────────────────────────────┴────────────┐
│ piesi-ffi：只放导出与 record/enum，无逻辑（句柄 + 回调 + 取消令牌） │
├──────────────────────────────────────────────────────────────┤
│ piesi-core（Rust）                                            │
│  archive   ArchiveReader/Writer trait、Entry、ArchiveIndex      │
│            （arena + u32 id，状态 Instant/Building/Complete）    │
│  fmt/zip   自写读取器（CD、local header、AES、分卷、修复扫描）      │
│            写入用 zip crate（锁版本）                            │
│  fmt/sevenz  sevenz-rust2 容器 + Codec 接入                     │
│  fmt/rar   unrar，只读、顺序                                    │
│  fmt/tar   tar + gz/bz2/xz/zst，后台建索引、全部解压走流式          │
│  codec     Codec trait：liblzma（默认）/ lzma-rust2（feature）、   │
│            deflate/deflate64/bzip2/zstd/ppmd；LZMA2 分块并行       │
│  encoding  候选解码 + 字频打分 + locale 先验 + 写入 NFC            │
│  names     垃圾文件过滤、Windows 安全名、255 字节截断、大小写冲突    │
│  plan      ExtractPlan（按 solid folder 分组、冲突）/ CreatePlan   │
│  fs        cap-std sink、quarantine xattr、原子替换、ChainedReader │
│  progress  Progress trait（节流）、CancelToken                   │
│  overlay   v1.x 暂存编辑                                        │
└──────────────────────────────────────────────────────────────┘
                 piesi-cli（clap）直接用 piesi-core
```

### 5.1 选型与理由（一句话）
- **核心用 Rust**：解析不可信压缩包是经典攻击面（libarchive、7-Zip 年年出 CVE），内存安全值回票价；一套引擎服务 Mac、Windows、CLI。
- **UI 用 SwiftUI + AppKit**：Mac 原生感是卖点，Finder/Quick Look/Services 都必须原生写；不用 Tauri/Flutter。
- **7z = sevenz-rust2 容器 + 它自带的多线程 LZMA2（`from_level_mt`，4×dict 分块）**：sevenz-rust2 有全部 BCJ/BCJ2，兼容性优先。P0 基准若比 `7zz` 慢超过 2 倍，再在 Codec 层接 liblzma 的 raw LZMA2 encoder 做分块并行。不选 7-Zip 的 `7z.so`（COM 风格无文档 API、约 1.5k 行 C++ shim、它的 zip handler 先解码文件名再给你，和编码层打架）。
- **ZIP 读取器自写**：中央目录 + local header 约 800 行，编解码和 AES（aes + hmac + pbkdf2）用 crates。原始文件名、编码重解、损坏 CD 的扫描修复、`.z01` 分卷全靠它；写入继续用 `zip` crate。
- **不 bundle `7zz`/`unrar` 子进程**（Keka 路线）：进度、取消、选择性解压、编码覆盖都难做，也无法移植。
- **直接分发（Developer ID + 公证）优先**，不先上 MAS：沙盒会卡「解压到压缩包旁边」这类基本操作；core 保持路径无关，日后 MAS 只改 App 层。

### 5.2 编码处理（核心卖点）

读取，每个条目取原始字节，**读取时保留原字节，不做 NFC**：
1. ZIP bit 11 置位 → UTF-8。
2. 有 0x7075 扩展字段且 CRC 对得上 → 用字段里的 UTF-8 名（覆盖 WinRAR 产物）。
3. 7z / rar 本身是 Unicode，跳过；tar 先按 UTF-8。
4. 所有名字都是合法 UTF-8 → UTF-8（覆盖 Finder/ditto 产物）。
5. 否则把全部名字字节拼起来，候选集 {UTF-8, GB18030, Big5, CP932, CP949, CP1251, CP1252} 全部用 `encoding_rs` 解码，非法即弃；按字频表（GB2312 一级字、Big5 常用字、JIS 第一水準、KS X 1001）打分；`version made by` 是 MS-DOS 和系统地区做 tie-breaker；chardetng 只算一票。
6. 顶栏选择器**常驻**显示「文件名编码：GBK（自动）」并带 5 个样本名实时预览；切换只重解码，不重读包；置信度低时解压前先弹这个选择条。
7. 解不出的字节用 U+FFFD 替换并标记；手动选择按包记忆（key = 文件大小 + 头 64 KB 哈希）。

写入：
- 一律 UTF-8 + bit 11 + NFC 归一化；可选同时写 0x7075 给老工具。
- 「发给 Windows」预设再做：替换 `\ / : * ? " < > |`、去尾部空格和点、提示保留名（CON、PRN、NUL…）、路径超过 240 字符提示。
- 不写 `__MACOSX` 和 AppleDouble；「Mac 专用」预设才保留 xattr/资源分支（写成 `._` AppleDouble，与 ditto 兼容）。

### 5.3 安全与文件系统规则
- 所有输出走 `cap-std` 的 `Dir`：系统调用层拦截 `..`、绝对路径、穿越 symlink；symlink 最后创建；拒绝 Windows 盘符与 `\\?\`。
- 压缩炸弹：声明展开大小 / 包大小 > 1000 且展开总量 > 可用磁盘 → 先提示。
- quarantine：App 与每个 appex 都设 `LSFileQuarantineEnabled = YES`（默认拒绝）；源压缩包没有 `com.apple.quarantine` 时，解压后对输出去掉它，避免比归档工具更烦的过度隔离；CLI 没有 Info.plist，必须手动拷 xattr。**不提供「去隔离」开关**；解压 .app 被 Gatekeeper 拦时弹说明（原因是应用没签名/公证，不是包坏了），不自动绕过。
- APFS 文件名上限 255 **字节**：Windows 来的 ≥86 个汉字文件名会 `ENAMETOOLONG` → 按 grapheme 截断 + 哈希后缀并报告。
- 大小写冲突（`README` / `readme`）在 plan 阶段检出并走冲突策略，不要解到一半。
- 解析器全部 Rust；`cargo-fuzz` 跑 zip/7z/rar 入口。

### 5.4 性能与内存
- `ArchiveIndex` 三态：zip/7z/rar 是 Instant（只读目录区，目标 10 万条目 < 200 ms）；tar.* 是 Building（窗口立即打开显示进度，「全部解压」走流式不等索引）；索引按 (path, size, mtime) 缓存。
- 索引只在 Rust；Swift 一次只取一个目录层级 `list_dir(path, sort, filter) -> [Row]`（id 用 u32 条目下标），搜索是 Rust 端平铺查询限量返回。
- 进度用 `#[uniffi::export(with_foreign)]` trait，Rust 侧节流（≥50 ms 或 ≥4 MB 才回调）；取消用 `CancelToken`（`Arc<AtomicBool>`），每 64 KB 检查一次。
- 条目内容不跨 FFI 返回 `Vec<u8>`：解到调用方容器内的临时文件，把 URL 交给 Quick Look。
- solid 7z 选择性解压：`ExtractPlan` 按 solid folder 分组，每组按归档顺序解一次、跳过未选条目（代价是时间，内存只占 dict）；RAR 同理顺序跳过。
- 7z 创建默认 solid 块 ≤ 128 MB 并按扩展名分组，这样选择性解压和后续编辑不用整包重压。
- zip 多文件并行压缩（每文件独立 deflate，worker = CPU 核数）；LZMA2 多线程在 Codec 层分块；字典按内存自动，默认上限 min(可用内存/4, 256 MB)；「极限」档先弹内存估算。
- 全程流式，永不整包进内存；解压前预检磁盘空间。

## 6. macOS 应用设计

### 6.1 窗口
- 单窗口：工具栏 [解压 ▾] [添加] [删除] [测试] | 搜索 | [编码：自动（UTF-8）▾ 常驻]。
- 主区：`NSOutlineView`（经 `NSViewRepresentable`，子节点按层级从 Rust 惰性拉取），列：名称 / 大小 / 压缩后 / 修改时间 / 类型；面包屑；右侧检查器（文件信息 + Quick Look 预览）。工具栏、检查器、对话框、设置用 SwiftUI。
- 底栏：条目数、总大小、压缩率、格式、是否加密、solid 提示。
- 任务窗口：所有进行中的任务，速度、剩余时间、可取消。
- 拖入 = 添加到包；拖出 = 解压到 Finder（`NSFilePromiseProvider`）；双击条目 = Quick Look；⌘↓ = 临时解压后用默认 app 打开。

### 6.2 压缩对话框（Finder 右键或拖到 Dock 触发）
预设卡片：[发给 Windows（ZIP）] [最小体积（7z）] [加密（7z）] [Mac 专用（ZIP）]，下方「高级」：格式、级别、密码、分卷大小、排除规则。

### 6.3 默认行为（用户已定：双击直接解压）
- 双击压缩包：**直接解压到压缩包旁边**，只显示一个小进度浮窗。例外：包加密 → 先要密码；编码置信度低 → 先弹带样本名的编码选择条；目标已存在 → 冲突策略。
- 要浏览时：按住 ⌥ 双击（app 在 `application(_:open:)` 里读 `NSEvent.modifierFlags`）、Finder 右键「用 Piesi 查看」、或在 app 内 ⌘O 打开。设置里可把默认改成「先浏览」。
- 评审意见（记录，不改决定）：评审建议默认先浏览，理由是 WinRAR/360/Bandizip 用户的肌肉记忆是双击开窗，且乱码要在解压前看到。已用「低置信度先弹编码条」覆盖后一点。
- 解压到压缩包所在目录；智能建文件夹；完成后可选在 Finder 显示、可选移压缩包到废纸篓（默认关）。
- 首次启动只问一件事：「设为默认解压工具」。

### 6.4 系统集成
- 默认处理器：注册 zip/7z/rar/tar/gz/bz2/xz/zst 等 UTI；7z（`org.7-zip.7-zip-archive`）、rar（`com.rarlab.rar-archive`）、zst 系统没声明，放进 `UTImportedTypeDeclarations`；「设为默认」用 `NSWorkspace.setDefaultApplication(at:toOpen:)` 逐个 UTI 设置。
- TCC：非沙盒 app 写 Desktop/Documents/Downloads 要在 Info.plist 写三个 `NS*FolderUsageDescription`，否则授权弹窗没有文字甚至静默拒绝。
- CLI `piesi` 随 app 打包在 `Contents/MacOS/`，同 hardened runtime 签名、一起公证。
- Finder 右键：`NSServices`（「用 Piesi 压缩」「用 Piesi 解压到此处」「用 Piesi 查看」）+ App Intents（可做 Quick Action）。
- Quick Look 扩展：空格预览包内文件列表；appex 永远沙盒且内存受限，**只做索引不解压**，列表最多 1 万行；头加密的 7z 无法预览（不能要密码）。
- 不用 FinderSync（为同步盘设计，不合适）。

### 6.5 分发
- 最低 macOS 14（SDK 26 编译，在 26 上自动用 Liquid Glass 外观）。
- Developer ID 签名 + 公证 + staple，按 `~/.claude/docs/apple-signing.md`（profile `aster-notary`）。
- 发版规范按 `~/.claude/docs/release-versioning.md` 写进 `docs/release.md`（只写 Rust + Swift/Xcode 两行）。

## 7. 开源与分发（用户已定：开源）

- 代码开源，放 GitHub。许可：`piesi-core` 与 app 用 **MIT OR Apache-2.0**（和 Rust 生态一致，方便别人嵌入核心）；rar 解压部分遵循 unRAR 许可（允许商用，不得用于创建 RAR），在 `LICENSE-THIRD-PARTY` 里单独列出。
- 分发走 Keka 模式：GitHub Releases 免费下载（签名公证的 DMG）+ 之后上 Mac App Store 卖同款支持开发 + GitHub Sponsors。功能不分版本。
- 开源本身是卖点：Unarchiver 被 MacPaw 收购后闭源，用户有信任顾虑；压缩工具处理的是私人文件，可审计更可信。
- 仓库里不放任何签名凭据；公证走本机 keychain profile。

## 8. 目录结构（MVP）

```
Piesi/
  CLAUDE.md                     项目规则（引用 docs/release.md）
  docs/release.md               发版规范
  LICENSE-MIT / LICENSE-APACHE / LICENSE-THIRD-PARTY
  Cargo.toml                    workspace（版本从 0.1.0-dev 起）
  crates/
    piesi-core/src/
      lib.rs                    open / index / plan / extract / create
      archive.rs                ArchiveReader/Writer trait、Entry、ArchiveIndex（arena、u32 id、三态）
      plan.rs                   ExtractPlan（solid 分组、冲突）/ CreatePlan + presets
      encoding.rs               候选解码 + 字频打分 + locale 先验 + 写入 NFC
      names.rs                  垃圾文件过滤、Windows 安全名、255 字节截断、大小写冲突
      fs.rs                     cap-std sink、quarantine xattr、原子替换、ChainedReader(.001)
      codec/                    Codec trait；lzma_c.rs(liblzma) lzma_rs.rs(feature) deflate/bz2/zstd/ppmd
      fmt/zip/                  自写 reader（cd.rs local.rs aes.rs split.rs repair.rs）；writer 用 zip crate
      fmt/sevenz.rs             sevenz-rust2 + codec 接入
      fmt/rar.rs                unrar，只读
      fmt/tar.rs                tar + gz/bz2/xz/zst，后台建索引
      progress.rs               Progress trait、CancelToken、节流
      overlay.rs                v1.x 暂存编辑
    piesi-ffi/                  只放 uniffi 导出与 record/enum，无逻辑
    piesi-cli/                  clap；同时是语料回归测试入口
  tests/corpus/                 7zz / zip / WinRAR / Bandizip / Finder 产出的夹具 + manifest.json（预期名单）
  apple/
    project.yml                 xcodegen 定义（.xcodeproj 不入库）
    Config/Version.xcconfig     MARKETING_VERSION + VERSION_SUFFIX（按发版规范）
    Piesi/                      SwiftUI app（Browser、ExtractSheet、EncodingPicker、Compress、Tasks、Settings、Services、Intents）
    PiesiQuickLook/             QLPreviewingController，只读索引
    PiesiCore.xcframework       scripts/build-core.sh 产物（不入库）
    scripts/build-core.sh       编译 Rust（arm64 + x86_64）→ uniffi-bindgen + lipo → XCFramework
    scripts/package.sh          签名、公证、staple、出 DMG
  .github/workflows/            ci.yml（cargo test + 语料回归 + xcodebuild）、release.yml（打标签出 DMG）
  windows/                      v2 再建
```

工程约定：bundle id `com.citrolabs.piesi`（沿用 ego lite 的组织前缀，可改）；Xcode 用 `Version.xcconfig` 单一来源；CI 公证用 App Store Connect API Key 放 GitHub Secrets，本机用 `aster-notary`。

## 9. 实施步骤

0. **仓库初始化**（当前目录不是 git 仓库）：
   ```sh
   cd /Users/mike/source/project/tools/Piesi && git init -b main && git commit --allow-empty -m "chore: init"
   git worktree add ../Piesi-mvp -b feat/mvp
   ```
   之后所有编辑、构建、测试都在 `../Piesi-mvp` 里做。写 `CLAUDE.md`、`docs/release.md`、`.gitignore`、许可文件。
1. **P0 验证（约 1 周）**，两个硬门槛：
   - **兼容语料先过，再谈速度**：收集 7-Zip 9.20/16/23/24、WinRAR 6/7、Bandizip、Finder/ditto、Info-ZIP 产出的包：BCJ/BCJ2/ARM64/Delta/PPMd、头加密、空文件/anti-item、>4 GB、10 万条目、symlink/unix 属性、solid/非 solid、`.001` 与 `.z01` 分卷、GBK/Big5/Shift_JIS/EUC-KR/CP1251 文件名。`piesi-cli` 跑 manifest 全过。
   - **基准**：1 GB 混合目录 + 单个 2 GB 文件，liblzma 分块并行 vs `7zz` vs lzma-rust2，**压缩和解压都测**，记耗时与峰值内存（`/usr/bin/time -l`）。
   - UniFFI 打通到一个空 SwiftUI 窗口：进度回调节流 + 取消令牌 + 单层目录拉取。
   - macOS 26 上复核 Finder/ditto 不写 bit 11（`ditto -c -k` 后用 Python 读 `flag_bits`）。
2. **P1 MVP（4–6 周）**：zip（自写 reader）/7z/rar/tar 解压、zip/7z 创建、浏览窗口（NSOutlineView）、编码识别 + 常驻选择器、干净默认、cap-std 与 quarantine、255 字节名、分卷、默认处理器、Services、CLI、签名公证、内测。
3. **P2 打磨（约 4 周）**：加密、Quick Look 扩展、App Intents、预设、任务队列、本地化、CI 与 Release 流程，发 1.0。
4. **P3**：Overlay 编辑、修复、bsdtar 长尾兜底、MAS 版、Windows 版。
5. 结束时 `git worktree remove ../Piesi-mvp`。

## 10. 验证方法

- **语料回归**：`tests/corpus` 全部包 + 预期名单 JSON，`cargo test` 和 CI 每次跑；对 `7zz` 与 Info-ZIP 做 round-trip（我们写的包它们能开，它们写的包我们能开）。
- **安全语料**：zip-slip 样本（绝对路径、`..`、symlink 逃逸、盘符）、炸弹样本、损坏样本、255 字节超长名、大小写冲突；`cargo fuzz` 跑 24 小时无崩溃。
- **quarantine**：源包有/无 `com.apple.quarantine` 两种情况，`xattr -l` 验证输出一致；CLI 同样验证。
- **性能**：和 `7zz`、Keka 对比同一数据集，`/usr/bin/time -l` 记录耗时与峰值内存；10 万条目包打开 < 200 ms，列表滚动不掉帧。
- **端到端**：Finder 右键用 Piesi 压缩 → 拷到 Windows 11 资源管理器解压，文件名正确、没有 `__MACOSX`；WinRAR 生成的 GBK zip → Piesi 打开名字正确；`spctl -a -vv -t exec` 显示 Notarized Developer ID。
- **UI**：按 ui-loop 流程截图比对。

## 11. 已定事项与评审分歧

- 开源（MIT OR Apache-2.0），Keka 式分发。
- 双击直接解压，⌥ 双击或右键才浏览（评审建议先浏览，已记录在 6.3，默认可在设置里切）。
- 7z 引擎：sevenz-rust2 容器 + liblzma 编解码；zesven 弃用；不用 7z.so / LZMA SDK / libarchive。
