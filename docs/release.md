# 版本号与发版规范

做法参照 Firefox Nightly / Rust nightly：两次发版之间，源码里的版本号一直带开发标记；不带标记的版本号一定是发出去的包。

## 1. 通用规则

1. **平时版本号带开发标记。** Rust 侧写 `0.1.0-dev`，Xcode 侧 `VERSION_SUFFIX = -dev`。本地构建和 CI 中间构建一眼能认出来。
2. **能取到 git 信息时，开发版接短哈希。** 例如 `0.1.0-dev-1a2b3c4`；工作区有未提交改动时再加 `+`。
3. **开发版不打标签，不建 Release。**
4. **版本号只存一处。** 安装包文件名、About 页、只认数字的字段在构建时读它，不手抄。
   Rust 与 macOS app 同号发版；若将来分开，标签带平台前缀（`macos-v0.1.1`），不同平台不要撞号。
5. **发版步骤：**
   1. 去掉开发标记（`0.1.0-dev` → `0.1.0`；`VERSION_SUFFIX` 清空）。
   2. `CHANGELOG.md` 顶上加 `## <版本> · <日期> · <渠道>`，渠道是 `alpha` / `beta` / `rc` / `stable`。
      更新日志手写，面向用户，一行一条。先按上个标签以来的 `git log` 起草，给用户看过再定稿。
   3. 提交 `chore(release): <版本>`，打注释标签 `v<版本>`，推送分支和标签。
      标签推上 GitHub 后，`.github/workflows/release.yml` 自动跑 `scripts/package.sh`（门禁 → 通用二进制 → Developer ID 签名 → 公证 → 用 Sparkle 私钥生成 `appcast.xml` → DMG 与 appcast 一起传到 Release）；需要的 Secrets 写在该文件开头的注释里。
      自动更新按 `CFBundleVersion`（= `MARKETING_VERSION`）比新旧，所以每次发版版本号必须比上一版大；老用户靠「latest」Release 里的 `appcast.xml` 发现新版，预发布不会推给他们。
   4. **标签推出去后，马上再做一个提交，把版本号推到下一个开发版，并推送。**
      默认补丁号加一（`0.1.0` → `0.1.1-dev`）；预发布串把最后一个数字加一（`0.1.0-alpha.2` → `0.1.0-alpha.3-dev`）。
      用户说了要升次版本号或主版本号，才按用户说的改。提交信息 `chore(release): 开始 <下一个版本>`，不打标签。
   5. 日常 push 不改版本号。只有发版标签推出去后才推进。
6. **门禁**（发版脚本或 CI 第一步检查，任一不过就停）：带开发标记的拒绝；版本号与标签一致；
   标签在主分支上（`git merge-base --is-ancestor <tag> origin/main`）；已跟踪的文件没有未提交改动（`git status --porcelain --untracked-files=no` 为空）。
7. **发出去就不改。** 已推送的标签不移动、不重打；已发布的 Release 不改回草稿。发错了就发下一个版本。
8. **打发版标签并推送前，先问用户。** 用户同意后，第 5.4 步的推进提交和推送一起做，不再问。

## 2. 本项目用到的语言

| 语言 / 构建 | 版本号存哪里 | 开发版写法 | 带哈希的写法 | 注意 |
|---|---|---|---|---|
| Rust | 根 `Cargo.toml` 的 `[workspace.package] version`（当前 `0.1.0-dev`） | `0.1.1-dev` | `scripts/package.sh` 接 `-<短哈希>` | 改完跑一次 `cargo check`，让 `Cargo.lock` 跟着更新并一起提交 |
| Swift / Xcode | `apple/Config/Version.xcconfig` 的 `MARKETING_VERSION`，另加 `VERSION_SUFFIX` | `MARKETING_VERSION = 0.1.1`，`VERSION_SUFFIX = -dev` | 构建脚本把短哈希写进 Info.plist 自定义键 `PiesiGitHash` | `CFBundleShortVersionString` 只认数字点号，开发标记不能进它；发版提交把 `VERSION_SUFFIX` 清空 |

只认数字的字段（Info.plist、DMG 文件名里的版本）：在构建脚本里去掉后缀再传，不改源码里的版本号。
Rust 与 Xcode 两处版本号必须一致，`scripts/package.sh` 开头校验。
