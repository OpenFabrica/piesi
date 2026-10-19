// PasswordDialog.swift —— 密码输入框：NSAlert + NSSecureTextField，系统原生样式。
import AppKit

/// 密码对话框。
@MainActor
enum PasswordDialog {
    /// 返回输入的密码；取消返回 nil。
    static func ask(archiveName: String, wrongBefore: Bool, parent: NSWindow?) async -> String? {
        let alert = NSAlert()
        alert.messageText = "「\(archiveName)」需要密码"
        alert.informativeText = wrongBefore ? "密码错误，请重新输入。" : "这个压缩包已加密，输入密码后继续。"
        let field = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.placeholderString = "密码"
        alert.accessoryView = field
        alert.addButton(withTitle: "确定")
        alert.addButton(withTitle: "取消")
        alert.window.initialFirstResponder = field
        let response = await Alerts.present(alert, in: parent)
        return response == .alertFirstButtonReturn ? field.stringValue : nil
    }
}
