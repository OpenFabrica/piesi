// ConflictDialog.swift —— 解压前的两个询问：顶层文件夹已存在时另建还是解进去；目标里已有同名文件时改名（默认）/ 覆盖 / 跳过。
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

/// 顶层文件夹（包里自带的或按包名建的）在目标里已存在时的询问。
@MainActor
enum ExistingFolderDialog {
    /// 用户的选择：另建新文件夹 / 解进已有文件夹
    enum Choice {
        case newFolder, merge
    }

    /// 默认按钮是另建「名字-2」，已有文件不受影响；取消返回 nil。
    static func ask(existing: String, newFolder: String, parent: NSWindow?) async -> Choice? {
        Log.extract.notice("existing folder dialog: \(existing, privacy: .public) → \(newFolder, privacy: .public)")
        let alert = NSAlert()
        alert.messageText = String(localized: "已有名为「\(existing)」的文件夹")
        alert.informativeText = String(localized: "可以新建「\(newFolder)」放解出的文件，也可以直接解压到已有的「\(existing)」里（遇到同名文件会再问）。")
        alert.addButton(withTitle: String(localized: "新建「\(newFolder)」"))
        alert.addButton(withTitle: String(localized: "解压到「\(existing)」"))
        alert.addButton(withTitle: String(localized: "取消"))
        switch await Alerts.present(alert, in: parent) {
        case .alertFirstButtonReturn: return .newFolder
        case .alertSecondButtonReturn: return .merge
        default: return nil
        }
    }
}
