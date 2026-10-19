// AppDelegate.swift —— AppKit 委托：接 Finder「用 Piesi 打开」事件，判断是否按住 ⌥。
import AppKit
import OSLog

/// 接收 application(_:open:)。P0 只记录路径和 ⌥ 状态到 AppState 并写统一日志，解压/浏览动作在 P1 接上。
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// 主窗口共享状态，由 PiesiApp 注入 SwiftUI 环境
    let state = AppState()
    /// 打开事件日志：Finder 启动的 app 没有 stdout，用统一日志，`log show --predicate 'subsystem == "com.citrolabs.piesi"'` 可查
    private let logger = Logger(subsystem: "com.citrolabs.piesi", category: "open")

    /// Finder 双击 / 拖到 Dock / open 命令时系统调用，可能一次给多个文件。
    /// 设计 6.3：⌥ 双击 = 浏览，否则直接解压。open 事件本身不带按键信息，
    /// 所以读 NSEvent.modifierFlags（当前硬件修饰键状态）；它在事件派发的同一时刻取值，足够可靠。
    func application(_ application: NSApplication, open urls: [URL]) {
        let browse = NSEvent.modifierFlags.contains(.option)
        for url in urls {
            logger.notice("open \(url.path, privacy: .public) browse=\(browse)")
            state.record(url, source: .finder, browse: browse)
        }
    }
}
