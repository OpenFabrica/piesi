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
    /// 隐藏启动参数 `-PiesiCompress <预设> <输出路径> <源路径…>`：测试入口，不弹对话框直接压缩（见 compressTestRequest）
    static let compressTest = "PiesiCompress"
    /// 隐藏启动参数 `-PiesiPassword <密码>`：配合 -PiesiCompress 给「加密」预设传密码
    static let compressPassword = "PiesiPassword"
}

/// 双击压缩包的默认动作。
enum DefaultAction: String {
    case extract
    case browse
}

/// `-PiesiCompress` 解析出来的压缩请求。
struct CompressTestRequest: Sendable {
    let preset: CompressPreset
    let destination: URL
    let sources: [URL]
    let password: String?
}

/// 设置的薄封装：只读访问走这里，写入由设置页的 @AppStorage 完成。
@MainActor
final class AppSettings {
    private let defaults = UserDefaults.standard

    /// 注册默认值：自动退出、解压后在 Finder 选中结果默认开，其余默认关（设计 6.3）。
    init() {
        defaults.register(defaults: [
            SettingsKey.defaultAction: DefaultAction.extract.rawValue,
            SettingsKey.autoQuit: true,
            SettingsKey.revealInFinder: true,
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

    /// 命令行里有没有 `-PiesiCompress`（有但格式不对时 AppDelegate 会在日志里报用法）
    var hasCompressTestFlag: Bool { ProcessInfo.processInfo.arguments.contains("-" + SettingsKey.compressTest) }

    /// 测试入口 `-PiesiCompress <预设> <输出路径> <源路径…>`，预设名 windows / smallest / encrypted / mac，
    /// 密码用 `-PiesiPassword <密码>`。多值参数 UserDefaults 读不了，这里自己扫 argv：取到下一个 `-` 开头的参数或结尾为止，
    /// 所以别的参数（-didAskDefaultHandler 等）要放在它前面。
    var compressTestRequest: CompressTestRequest? {
        let arguments = ProcessInfo.processInfo.arguments
        guard let flag = arguments.firstIndex(of: "-" + SettingsKey.compressTest) else { return nil }
        let values = Array(arguments[(flag + 1)...].prefix { !$0.hasPrefix("-") })
        guard values.count >= 3, let preset = CompressPreset(rawValue: values[0]) else { return nil }
        var password: String?
        if let index = arguments.firstIndex(of: "-" + SettingsKey.compressPassword), index + 1 < arguments.count {
            password = arguments[index + 1]
        }
        return CompressTestRequest(
            preset: preset,
            destination: URL(fileURLWithPath: values[1]),
            sources: values[2...].map { URL(fileURLWithPath: $0) },
            password: password
        )
    }
}
