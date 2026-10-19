// AppSettings.swift —— 用户设置，存 UserDefaults；SwiftUI 设置页用 @AppStorage 读写同一批键。
import Foundation

/// 设置键名集中定义，避免两边拼错。
enum SettingsKey {
    /// 双击压缩包的默认动作："extract" 直接解压 / "browse" 先浏览
    static let defaultAction = "defaultAction"
    /// 解压/压缩完成后在 Finder 里显示
    static let revealInFinder = "revealInFinder"
    /// 解压完成后把压缩包移到废纸篓
    static let moveToTrash = "moveArchiveToTrash"
    /// 因打开文件而启动的任务全部完成后自动退出
    static let autoQuit = "autoQuitAfterTasks"
    /// 首次启动是否已经问过「设为默认解压工具」
    static let didAskDefaultHandler = "didAskDefaultHandler"
    /// 隐藏启动参数 `-PiesiBrowse YES`：强制走浏览窗口，自动化测试 ⌥ 路径用
    static let forceBrowse = "PiesiBrowse"
    /// 隐藏启动参数 `-PiesiShowSettings YES`：启动就开设置窗口，自动化测试用
    static let showSettings = "PiesiShowSettings"
}

/// 双击压缩包的默认动作。
enum DefaultAction: String {
    case extract
    case browse
}

/// 设置的薄封装：只读访问走这里，写入由设置页的 @AppStorage 完成。
@MainActor
final class AppSettings {
    private let defaults = UserDefaults.standard

    /// 注册默认值：自动退出默认开，其余默认关（设计 6.3）。
    init() {
        defaults.register(defaults: [
            SettingsKey.defaultAction: DefaultAction.extract.rawValue,
            SettingsKey.autoQuit: true,
        ])
    }

    var defaultAction: DefaultAction {
        DefaultAction(rawValue: defaults.string(forKey: SettingsKey.defaultAction) ?? "") ?? .extract
    }
    var revealInFinder: Bool { defaults.bool(forKey: SettingsKey.revealInFinder) }
    var moveToTrash: Bool { defaults.bool(forKey: SettingsKey.moveToTrash) }
    var autoQuit: Bool { defaults.bool(forKey: SettingsKey.autoQuit) }
    var didAskDefaultHandler: Bool {
        get { defaults.bool(forKey: SettingsKey.didAskDefaultHandler) }
        set { defaults.set(newValue, forKey: SettingsKey.didAskDefaultHandler) }
    }
    /// 命令行 `-PiesiBrowse YES` 会进 NSArgumentDomain，这里直接读得到
    var forceBrowse: Bool { defaults.bool(forKey: SettingsKey.forceBrowse) }
    var showSettingsAtLaunch: Bool { defaults.bool(forKey: SettingsKey.showSettings) }
}
