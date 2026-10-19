// PiesiApp.swift —— 程序入口。用 AppKit 生命周期而不是 SwiftUI App：
// 因打开文件而启动时不能弹空主窗口、任务做完要自动退出、要多开浏览窗口，这些 SwiftUI 的 WindowGroup 在 macOS 14 上管不住。
import AppKit

/// 入口：建 NSApplication、挂 AppDelegate、装菜单、进主循环。
@main
@MainActor
enum PiesiApp {
    static func main() {
        let app = NSApplication.shared
        let delegate = AppDelegate()
        // NSApp.delegate 是弱引用，必须另外强持有
        AppDelegate.shared = delegate
        app.delegate = delegate
        app.setActivationPolicy(.regular)
        app.mainMenu = MainMenu.build()
        app.run()
    }
}
