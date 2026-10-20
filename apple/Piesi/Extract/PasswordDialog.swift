// PasswordDialog.swift —— 密码输入框：NSAlert + NSSecureTextField，系统原生样式；ZipCrypto 包在输入框下多一行灰字提醒不安全。
import AppKit

/// 密码对话框。
@MainActor
enum PasswordDialog {
    /// 返回输入的密码；取消返回 nil。weakEncryption = 包用的是老式 ZipCrypto（核心的 hasWeakEncryption），在输入框下方提醒。
    static func ask(archiveName: String, wrongBefore: Bool, weakEncryption: Bool, parent: NSWindow?) async -> String? {
        // 对话框本身没法自动化点，日志留一条让脚本核对「弱加密提示有没有出现」
        Log.app.notice("password dialog \(archiveName, privacy: .public) wrongBefore=\(wrongBefore) weak=\(weakEncryption)")
        let alert = NSAlert()
        alert.messageText = String(localized: "「\(archiveName)」需要密码")
        alert.informativeText = wrongBefore ? String(localized: "密码错误，请重新输入。") : String(localized: "这个压缩包已加密，输入密码后继续。")
        let field = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.placeholderString = String(localized: "密码")
        alert.accessoryView = weakEncryption ? withWeakHint(field) : field
        alert.addButton(withTitle: String(localized: "确定"))
        alert.addButton(withTitle: String(localized: "取消"))
        alert.window.initialFirstResponder = field
        let response = await Alerts.present(alert, in: parent)
        return response == .alertFirstButtonReturn ? field.stringValue : nil
    }

    /// 输入框下面加一行灰色小字：ZipCrypto 已知可破解（设计 4「加密」：打开 ZipCrypto 包时提示不安全）。
    private static func withWeakHint(_ field: NSSecureTextField) -> NSView {
        let width = field.frame.width
        let hint = NSTextField(wrappingLabelWithString: String(localized: "这个包用的是老式 ZipCrypto 加密，已知可被破解。能解开，但建议用 AES 重新打包。"))
        hint.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        hint.textColor = .secondaryLabelColor
        hint.preferredMaxLayoutWidth = width
        let hintHeight = hint.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: width, height: 10_000)).height ?? 32
        hint.frame = NSRect(x: 0, y: 0, width: width, height: hintHeight)
        // NSAlert 的 accessoryView 按 frame 布局、不走自动布局：手动摆，输入框在上、提示在下（AppKit 坐标原点在左下）
        let spacing: CGFloat = 6
        let container = NSView(frame: NSRect(x: 0, y: 0, width: width, height: field.frame.height + spacing + hintHeight))
        field.frame.origin = NSPoint(x: 0, y: hintHeight + spacing)
        container.addSubview(field)
        container.addSubview(hint)
        return container
    }
}
