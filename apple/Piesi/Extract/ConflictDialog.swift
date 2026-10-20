// ConflictDialog.swift —— 目标里已有同名项目时问冲突策略：改名（默认）/ 覆盖 / 跳过。
import AppKit

/// 冲突策略对话框。
@MainActor
enum ConflictDialog {
    /// 返回用户选的策略；取消返回 nil。
    static func ask(conflicts: [String], parent: NSWindow?) async -> Conflict? {
        Log.extract.notice("conflict dialog: \(conflicts.count) items")
        let alert = NSAlert()
        alert.messageText = String(localized: "目标位置已有 \(conflicts.count) 个同名项目")
        let preview = conflicts.prefix(6).joined(separator: "\n")
        let more = conflicts.count > 6 ? String(localized: "\n…还有 \(conflicts.count - 6) 个") : ""
        alert.informativeText = preview + more + "\n\n" + String(localized: "改名会给新解出的文件加上序号，已有文件不动。")
        alert.addButton(withTitle: String(localized: "改名"))
        alert.addButton(withTitle: String(localized: "覆盖"))
        alert.addButton(withTitle: String(localized: "跳过"))
        alert.addButton(withTitle: String(localized: "取消"))
        alert.buttons[1].hasDestructiveAction = true
        switch await Alerts.present(alert, in: parent) {
        case .alertFirstButtonReturn: return .rename
        case .alertSecondButtonReturn: return .overwrite
        case .alertThirdButtonReturn: return .skip
        default: return nil
        }
    }
}
