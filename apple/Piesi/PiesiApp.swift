// PiesiApp.swift —— SwiftUI 入口。文件打开事件走 AppDelegate，共享状态通过环境注入主窗口。
import SwiftUI

/// App 入口：挂 AppDelegate 接 Finder 事件，开一个主窗口，把 AppState 注入 SwiftUI 环境。
@main
struct PiesiApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environment(appDelegate.state)
        }
    }
}
