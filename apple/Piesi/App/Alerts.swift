// Alerts.swift —— NSAlert 的 async 封装：有父窗口就挂 sheet，没有就独立模态。
import AppKit

/// 提示框工具。
@MainActor
enum Alerts {
    /// 显示并等用户点按钮。
    static func present(_ alert: NSAlert, in parent: NSWindow?) async -> NSApplication.ModalResponse {
        if let parent, parent.isVisible {
            return await alert.beginSheetModal(for: parent)
        }
        NSApp.activate()
        return alert.runModal()
    }

    /// 错误提示，只有一个「好」。
    static func error(_ title: String, _ message: String, in parent: NSWindow?) async {
        // 弹过的错误也进统一日志：没有 UI 自动化时靠它核对文案（含本地化）
        Log.app.error("alert: \(title, privacy: .public) — \(message, privacy: .public)")
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        alert.addButton(withTitle: String(localized: "好"))
        _ = await present(alert, in: parent)
    }
}
