// PreviewViewController.swift —— Quick Look 预览扩展：显示压缩包的条目树。
// 设计 6.4：appex 沙盒且内存、时间受限，只读索引（zip/7z/rar 读目录区，tar.* 扫一遍建索引），绝不解压。
import Cocoa
import OSLog
import Quartz
import UniformTypeIdentifiers

/// 大纲树的一个节点：对应 Rust 返回的一行；子节点在第一次展开时才向 Rust 要，不持有整棵树。
@MainActor
final class ArchiveNode {
    let row: Row
    /// nil = 还没向 Rust 拉过
    var children: [ArchiveNode]?

    init(row: Row) {
        self.row = row
    }

    /// 只有带子项的目录能展开；空目录显示成叶子
    var isExpandable: Bool {
        row.kind == .dir && row.childCount > 0
    }
}

/// 后台线程打开压缩包后一次性带回主线程的数据（都是 Sendable）。
struct LoadedArchive: Sendable {
    let archive: ArchiveHandle
    let format: String
    let entryCount: UInt32
    let totalSize: UInt64
    let encoding: EncodingInfo
    let password: PasswordState
    /// 顶层条目；文件名加密时为空
    let topLevel: [Row]
}

/// Quick Look 预览控制器：顶部一行摘要（格式、条目数、总大小、文件名编码），下面是可展开的条目树。
/// 由 Info.plist 的 NSExtensionPrincipalClass 指定，系统直接实例化，没有 nib。
@MainActor
final class PreviewViewController: NSViewController, QLPreviewingController, NSOutlineViewDataSource, NSOutlineViewDelegate {
    /// 摘要行（可换行、可选中复制）
    private let header = NSTextField(wrappingLabelWithString: "")
    private let outline = NSOutlineView()
    private let scroll = NSScrollView()
    /// 打开后的句柄；展开目录时还要用它拉子项
    private var archive: ArchiveHandle?
    private var roots: [ArchiveNode] = []
    private let sizeFormatter = ByteCountFormatter()
    private let dateFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd HH:mm"
        return f
    }()
    /// appex 没有 stdout，用统一日志：`log stream --predicate 'subsystem == "com.citrolabs.piesi"'`
    private let logger = Logger(subsystem: "com.citrolabs.piesi", category: "quicklook")

    /// 没有 nib，手动建视图：摘要行在上，大纲表填满剩余空间。
    override func loadView() {
        let container = NSView(frame: NSRect(x: 0, y: 0, width: 680, height: 440))
        preferredContentSize = container.frame.size

        header.font = .systemFont(ofSize: 13, weight: .medium)
        header.textColor = .secondaryLabelColor
        header.stringValue = String(localized: "正在读取…")
        header.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(header)

        for (id, title, width, alignment) in [("name", String(localized: "名称"), 360.0, NSTextAlignment.left), ("size", String(localized: "大小"), 90.0, .right), ("mtime", String(localized: "修改时间"), 130.0, .left)] {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(id))
            column.title = title
            column.width = width
            column.headerCell.alignment = alignment
            outline.addTableColumn(column)
        }
        // 第一列是树形列，展开三角画在它上面
        outline.outlineTableColumn = outline.tableColumns[0]
        outline.dataSource = self
        outline.delegate = self
        outline.rowHeight = 20
        outline.usesAlternatingRowBackgroundColors = true
        outline.allowsColumnReordering = false
        outline.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        outline.autoresizesOutlineColumn = true
        scroll.documentView = outline
        scroll.hasVerticalScroller = true
        scroll.borderType = .noBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(scroll)

        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: container.topAnchor, constant: 10),
            header.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 14),
            container.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: 14),
            scroll.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 8),
            scroll.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: container.bottomAnchor),
        ])
        view = container
    }

    /// 系统要预览某个文件时调用。打开和顶层列表放后台线程（Task.detached 不继承 main actor），结果回主线程填表。
    /// 打不开时自己显示原因而不抛错：抛错会让 Quick Look 退回通用图标，用户反而看不到是什么问题。
    func preparePreviewOfFile(at url: URL) async throws {
        let path = url.path
        do {
            let loaded = try await Task.detached(priority: .userInitiated) { () -> LoadedArchive in
                let archive = try openArchive(path: path, locale: nil)
                let password = archive.passwordState()
                // 文件名也加密的包（7z 头加密）拿不到名字，appex 里不能要密码，只显示提示
                let topLevel = password == .forNames ? [] : archive.listDir(dir: "")
                return LoadedArchive(
                    archive: archive,
                    format: archive.formatName(),
                    entryCount: archive.entryCount(),
                    totalSize: archive.totalSize(),
                    encoding: archive.encodingInfo(),
                    password: password,
                    topLevel: topLevel
                )
            }.value
            archive = loaded.archive
            roots = loaded.topLevel.map(ArchiveNode.init)
            header.stringValue = summary(of: loaded)
            outline.reloadData()
            // 只有一个顶层文件夹（ditto、7zz 打包目录的常见形态）时自动展开它，省一次点击；
            // 多个顶层项不展开，免得大包逐个目录拉子项拖慢首屏。
            if roots.count == 1, let only = roots.first, only.isExpandable {
                outline.expandItem(only)
            }
            logger.notice("preview \(url.lastPathComponent, privacy: .public): \(loaded.format, privacy: .public) \(loaded.entryCount) 项")
        } catch {
            header.stringValue = String(localized: "无法读取压缩包：\(error.localizedDescription)")
            logger.error("preview \(url.lastPathComponent, privacy: .public) failed: \(String(describing: error), privacy: .public)")
        }
    }

    /// 摘要行：「ZIP · 1,234 项 · 56.7 MB · 文件名编码 GBK（自动）」。
    private func summary(of loaded: LoadedArchive) -> String {
        var parts = [loaded.format, String(localized: "\(loaded.entryCount.formatted()) 项"), sizeFormatter.string(fromByteCount: Int64(loaded.totalSize))]
        switch loaded.password {
        case .forNames:
            parts.append(String(localized: "文件名已加密，需要用 Piesi 打开并输入密码"))
        case .forData:
            parts.append(String(localized: "内容已加密"))
        case .notNeeded:
            break
        }
        let enc = loaded.encoding
        var label = String(localized: "文件名编码 \(encodingLabel(enc.encoding))")
        if enc.source != "manual" {
            label += enc.ambiguous ? String(localized: "（自动，不确定）") : String(localized: "（自动）")
        }
        parts.append(label)
        return parts.joined(separator: " · ")
    }

    /// 编码枚举转人看的名字。
    private func encodingLabel(_ encoding: Encoding) -> String {
        switch encoding {
        case .utf8: "UTF-8"
        case .gb18030: "GBK / GB18030"
        case .big5: "Big5"
        case .shiftJis: "Shift_JIS"
        case .eucKr: "EUC-KR"
        case .cp1251: "CP1251"
        case .cp866: "CP866"
        case .cp1252: "CP1252"
        }
    }

    // MARK: - NSOutlineViewDataSource

    /// 子节点惰性拉取：第一次展开时才调 Rust 的 listDir，之后缓存在节点上。
    private func children(of node: ArchiveNode) -> [ArchiveNode] {
        if let cached = node.children {
            return cached
        }
        let rows = archive?.listDir(dir: node.row.path) ?? []
        let nodes = rows.map(ArchiveNode.init)
        node.children = nodes
        return nodes
    }

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        guard let node = item as? ArchiveNode else { return roots.count }
        return children(of: node).count
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        guard let node = item as? ArchiveNode else { return roots[index] }
        return children(of: node)[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        (item as? ArchiveNode)?.isExpandable ?? false
    }

    // MARK: - NSOutlineViewDelegate

    /// 视图型单元格：名称列带图标，大小右对齐，目录不显示大小。
    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        guard let node = item as? ArchiveNode, let column = tableColumn else { return nil }
        let cell = outlineView.makeView(withIdentifier: column.identifier, owner: self) as? NSTableCellView ?? makeCell(column.identifier)
        let row = node.row
        switch column.identifier.rawValue {
        case "name":
            cell.textField?.stringValue = row.name + (row.encrypted ? "  " + String(localized: "[加密]") : "")
            cell.imageView?.image = icon(for: row)
        case "size":
            cell.textField?.stringValue = row.kind == .file ? sizeText(row.size) : ""
            cell.textField?.alignment = .right
        case "mtime":
            cell.textField?.stringValue = row.mtime.map { dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval($0))) } ?? ""
        default:
            cell.textField?.stringValue = ""
        }
        return cell
    }

    /// 大小列文字：ByteCountFormatter 对 0 会显示「0 KB」，这里统一成「0 字节」。
    private func sizeText(_ bytes: UInt64) -> String {
        bytes == 0 ? String(localized: "0 字节") : sizeFormatter.string(fromByteCount: Int64(bytes))
    }

    /// 新建一个带图标 + 文字的单元格；makeView 复用时就不再走这里。
    private func makeCell(_ identifier: NSUserInterfaceItemIdentifier) -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = identifier
        let text = NSTextField(labelWithString: "")
        text.lineBreakMode = .byTruncatingMiddle
        text.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(text)
        cell.textField = text
        var leading = cell.leadingAnchor
        if identifier.rawValue == "name" {
            let image = NSImageView()
            image.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(image)
            cell.imageView = image
            NSLayoutConstraint.activate([
                image.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
                image.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                image.widthAnchor.constraint(equalToConstant: 16),
                image.heightAnchor.constraint(equalToConstant: 16),
            ])
            leading = image.trailingAnchor
        }
        NSLayoutConstraint.activate([
            text.leadingAnchor.constraint(equalTo: leading, constant: 4),
            cell.trailingAnchor.constraint(equalTo: text.trailingAnchor, constant: 2),
            text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    /// 按类型取系统图标：目录用文件夹，文件按扩展名，链接用别名图标。
    private func icon(for row: Row) -> NSImage {
        switch row.kind {
        case .dir:
            return NSWorkspace.shared.icon(for: .folder)
        case .symlink:
            return NSWorkspace.shared.icon(for: .aliasFile)
        case .file:
            let ext = (row.name as NSString).pathExtension
            let type = ext.isEmpty ? UTType.data : (UTType(filenameExtension: ext) ?? .data)
            return NSWorkspace.shared.icon(for: type)
        }
    }
}
