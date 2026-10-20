// DefaultHandler.swift —— 「设为默认解压工具」：对 Info.plist 里声明的每个压缩包类型调 NSWorkspace.setDefaultApplication。
import AppKit
import UniformTypeIdentifiers

/// 默认处理器设置。
@MainActor
enum DefaultHandler {
    /// 首次启动只问这一件事（设计 6.3）；问过就记住，不再问。
    static func askOnFirstLaunch(settings: AppSettings) async {
        guard !settings.didAskDefaultHandler else { return }
        settings.didAskDefaultHandler = true
        Log.app.notice("default handler: asking on first launch")
        let alert = NSAlert()
        alert.messageText = String(localized: "把 Piesi 设为默认的解压工具？")
        alert.informativeText = String(localized: "之后双击 zip、7z、rar、tar 等压缩包会直接用 Piesi 解压。随时可以在设置里改。")
        alert.addButton(withTitle: String(localized: "设为默认"))
        alert.addButton(withTitle: String(localized: "以后再说"))
        guard await Alerts.present(alert, in: nil) == .alertFirstButtonReturn else {
            Log.app.notice("default handler: user declined on first launch")
            return
        }
        await setAsDefault(reportTo: nil)
    }

    /// 逐个类型设置。reportTo 非空（设置页按钮）时弹结果；失败的类型总会提示。
    @discardableResult
    static func setAsDefault(reportTo window: NSWindow?) async -> (ok: Int, failed: [String]) {
        var ok = 0
        var failed: [String] = []
        for type in ArchiveTypes.all {
            do {
                try await NSWorkspace.shared.setDefaultApplication(at: Bundle.main.bundleURL, toOpen: type)
                ok += 1
            } catch {
                failed.append(type.localizedDescription ?? type.identifier)
                Log.app.warning("set default for \(type.identifier, privacy: .public) failed: \(error.localizedDescription, privacy: .public)")
            }
        }
        Log.app.notice("default handler: ok=\(ok) failed=\(failed.count)")
        if window != nil || !failed.isEmpty {
            let alert = NSAlert()
            if failed.isEmpty {
                alert.messageText = String(localized: "已设为默认解压工具")
                alert.informativeText = String(localized: "\(ok) 种压缩包类型现在默认用 Piesi 打开。")
            } else {
                alert.alertStyle = .warning
                alert.messageText = String(localized: "部分类型没有设置成功")
                alert.informativeText = String(localized: "成功 \(ok) 种，失败：\(failed.joined(separator: "、"))")
            }
            alert.addButton(withTitle: String(localized: "好"))
            _ = await Alerts.present(alert, in: window)
        }
        return (ok, failed)
    }
}
