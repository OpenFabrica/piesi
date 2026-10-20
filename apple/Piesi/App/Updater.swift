// Updater.swift —— 自动更新（Sparkle 2，设计 6.6）：菜单「检查更新…」和设置页的开关都走这里。
import AppKit
import Sparkle

/// 自动更新的入口，包一层 Sparkle 的标准更新器（自带检查、下载、安装、重启的界面和多语言文案）。
/// 更新清单 appcast.xml 和 DMG 放在 GitHub Release，地址是 Info.plist 的 SUFeedURL；
/// DMG 带 EdDSA 签名（公钥 SUPublicEDKey，私钥在发版机钥匙串 / CI Secret），签名不对 Sparkle 拒装。
@MainActor
final class Updater: NSObject, SPUUpdaterDelegate {
    /// startingUpdater: false —— 等 applicationDidFinishLaunching 再启动，不赶在首启提示前面检查
    private lazy var controller = SPUStandardUpdaterController(startingUpdater: false, updaterDelegate: self, userDriverDelegate: nil)

    /// 启动更新器：按 Info.plist 的 SUEnableAutomaticChecks 和用户设置定时检查。
    func start() {
        controller.startUpdater()
    }

    /// 手动检查（菜单、设置页按钮）；有窗口时 Sparkle 会把结果弹出来，没有更新也会告诉用户。
    func checkForUpdates() {
        controller.checkForUpdates(nil)
    }

    /// 正在检查或下载时为 false，菜单项据此变灰。
    var canCheckForUpdates: Bool {
        controller.updater.canCheckForUpdates
    }

    /// 是否定时自动检查（存在 Sparkle 自己的 UserDefaults 键里）。
    var automaticallyChecks: Bool {
        get { controller.updater.automaticallyChecksForUpdates }
        set { controller.updater.automaticallyChecksForUpdates = newValue }
    }

    /// 发现更新后是否在后台下载、退出时自动安装。
    var automaticallyDownloads: Bool {
        get { controller.updater.automaticallyDownloadsUpdates }
        set { controller.updater.automaticallyDownloadsUpdates = newValue }
    }

    /// 测试入口 `-PiesiFeedURL <地址>`：换掉清单地址，本地起服务器演练整个更新流程用。
    /// 正式包里留着也安全：换了清单，安装包仍要过 EdDSA 签名和代码签名同一团队的检查。
    nonisolated func feedURLString(for updater: SPUUpdater) -> String? {
        UserDefaults.standard.string(forKey: SettingsKey.feedURL)
    }
}
