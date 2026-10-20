// ArchiveOutlineView.swift —— NSOutlineView 列表（NSViewRepresentable）。子节点展开时才向 Rust 拉一层，
// 10 万条目也只持有展开过的部分；支持列排序、搜索平铺、双击 / 空格预览、⌘↓ 打开、拖出。
import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// 一个节点：一行 + 懒加载的子节点。NSOutlineView 用对象身份当 item，所以必须是 class。
@MainActor
final class ArchiveNode {
    let row: Row
    /// nil = 还没向 Rust 拉过
    var children: [ArchiveNode]?

    init(row: Row) {
        self.row = row
    }

    var isExpandable: Bool {
        row.childCount > 0
    }
}

/// 列表触发的动作，由窗口控制器提供。
struct OutlineActions {
    /// 双击文件 / 空格：Quick Look
    var preview: @MainActor (Row) -> Void
    /// ⌘↓：临时解压后用默认 app 打开
    var openWithDefaultApp: @MainActor (Row) -> Void
    /// 拖出：返回 NSFilePromiseProvider
    var dragExport: @MainActor (Row) -> NSPasteboardWriting?
}

/// SwiftUI 包装。reloadToken / searchQuery 作为普通属性传入，变了 SwiftUI 才会调 updateNSView。
struct ArchiveOutlineView: NSViewRepresentable {
    let model: BrowserModel
    let actions: OutlineActions
    let reloadToken: Int
    let searchQuery: String

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model, actions: actions)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let outline = KeyHandlingOutlineView()
        outline.identifier = NSUserInterfaceItemIdentifier("ArchiveOutline")
        outline.autosaveName = "ArchiveOutline"
        outline.autosaveTableColumns = true
        outline.allowsMultipleSelection = true
        outline.usesAlternatingRowBackgroundColors = true
        outline.style = .inset
        outline.rowSizeStyle = .default
        outline.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        for spec in ColumnSpec.all {
            let column = NSTableColumn(identifier: spec.id)
            column.title = spec.title
            column.width = spec.width
            column.minWidth = 60
            column.sortDescriptorPrototype = NSSortDescriptor(key: spec.id.rawValue, ascending: true)
            outline.addTableColumn(column)
        }
        outline.outlineTableColumn = outline.tableColumns.first
        outline.dataSource = context.coordinator
        outline.delegate = context.coordinator
        outline.target = context.coordinator
        outline.doubleAction = #selector(Coordinator.doubleClicked(_:))
        outline.setDraggingSourceOperationMask(.copy, forLocal: false)
        let coordinator = context.coordinator
        outline.onSpace = { coordinator.previewSelection() }
        outline.onCommandDown = { coordinator.openSelection() }
        coordinator.outlineView = outline

        let scrollView = NSScrollView()
        scrollView.documentView = outline
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        context.coordinator.sync(token: reloadToken, query: searchQuery)
    }

    /// 列定义。
    struct ColumnSpec {
        let id: NSUserInterfaceItemIdentifier
        let title: String
        let width: CGFloat

        static let name = NSUserInterfaceItemIdentifier("name")
        static let size = NSUserInterfaceItemIdentifier("size")
        static let packed = NSUserInterfaceItemIdentifier("packed")
        static let mtime = NSUserInterfaceItemIdentifier("mtime")
        static let kind = NSUserInterfaceItemIdentifier("kind")

        static let all: [ColumnSpec] = [
            ColumnSpec(id: name, title: String(localized: "名称"), width: 300),
            ColumnSpec(id: size, title: String(localized: "大小"), width: 80),
            ColumnSpec(id: packed, title: String(localized: "压缩后"), width: 80),
            ColumnSpec(id: mtime, title: String(localized: "修改时间"), width: 140),
            ColumnSpec(id: kind, title: String(localized: "类型"), width: 120),
        ]
    }

    /// 数据源 + 委托。缓存只到展开过的节点。
    @MainActor
    final class Coordinator: NSObject, NSOutlineViewDataSource, NSOutlineViewDelegate {
        private let model: BrowserModel
        private let actions: OutlineActions
        weak var outlineView: NSOutlineView?
        private var roots: [ArchiveNode]?
        private var searchResults: [ArchiveNode]?
        private var lastToken = -1
        private var lastQuery = ""
        private var sortDescriptor: NSSortDescriptor?
        /// 搜索平铺结果的上限，和底栏提示里的数字一致
        static let searchLimit: UInt32 = 200
        /// 已经排好但还没执行的重载（多次 sync 合并成一次）
        private var reloadScheduled = false
        /// 重载后要恢复展开的目录（切编码时记下）
        private var pendingExpansion: [ExpandedKey] = []
        private var iconCache: [String: NSImage] = [:]
        /// 「类型」列文字按扩展名缓存：UTType(filenameExtension:) 不便宜，按类型排序时每次比较都会调
        private var kindTextCache: [String: String] = [:]
        private let dateFormatter: DateFormatter = {
            let f = DateFormatter()
            f.dateStyle = .short
            f.timeStyle = .short
            return f
        }()

        init(model: BrowserModel, actions: OutlineActions) {
            self.model = model
            self.actions = actions
        }

        /// 编码或搜索词变了就丢缓存重载。重载推到下一轮主线程：updateNSView 跑在 SwiftUI 更新里，
        /// reloadData 会同步触发 selectionDidChange 回写 model.selectedRows，等于在视图更新中改状态。
        func sync(token: Int, query: String) {
            var needsReload = false
            if token != lastToken {
                // 切编码后节点对象全换，先记下展开过的目录，重载后按 id / 路径再展开
                pendingExpansion = expandedKeys()
                lastToken = token
                roots = nil
                searchResults = nil
                needsReload = true
            }
            if query != lastQuery {
                lastQuery = query
                searchResults = nil
                needsReload = true
            }
            if needsReload {
                scheduleReload()
            }
        }

        /// 把重载排到当前主线程工作结束之后，多次 sync 合并成一次。
        private func scheduleReload() {
            guard !reloadScheduled else { return }
            reloadScheduled = true
            Task { @MainActor [weak self] in
                guard let self else { return }
                reloadScheduled = false
                outlineView?.reloadData()
                let keys = pendingExpansion
                pendingExpansion = []
                restoreExpansion(keys)
            }
        }

        /// 展开状态的身份：真实目录用 id（切编码后 id 不变、名字会变），合成目录没有 id 只能按路径
        private struct ExpandedKey {
            let id: UInt32
            let path: String

            func matches(_ row: Row) -> Bool {
                id == UInt32.max ? row.path == path : row.id == id
            }
        }

        /// 当前展开的目录，按行序（父目录在前）。
        private func expandedKeys() -> [ExpandedKey] {
            guard let outlineView else { return [] }
            return (0..<outlineView.numberOfRows).compactMap { index in
                guard let node = outlineView.item(atRow: index) as? ArchiveNode, outlineView.isItemExpanded(node) else { return nil }
                return ExpandedKey(id: node.row.id, path: node.row.path)
            }
        }

        /// 按记下的顺序重新展开：父目录展开后子目录才会出现在行里，所以顺序不能乱。
        private func restoreExpansion(_ keys: [ExpandedKey]) {
            guard let outlineView, !keys.isEmpty, !isSearching else { return }
            for key in keys {
                for index in 0..<outlineView.numberOfRows {
                    guard let node = outlineView.item(atRow: index) as? ArchiveNode, key.matches(node.row) else { continue }
                    outlineView.expandItem(node)
                    break
                }
            }
        }

        private var isSearching: Bool {
            !lastQuery.isEmpty
        }

        /// 根层：搜索时是平铺结果（最多 200 条），否则是顶层目录。
        private func rootNodes() -> [ArchiveNode] {
            if isSearching {
                if searchResults == nil {
                    let rows = model.handle?.search(query: lastQuery, limit: Self.searchLimit) ?? []
                    searchResults = sorted(rows.map(ArchiveNode.init))
                    // 到了上限就让底栏提示还有没列出来的；这里可能在 AppKit 布局里被调，状态改动另起一轮
                    let hitLimit = rows.count >= Int(Self.searchLimit)
                    let model = self.model
                    Task { @MainActor in
                        if model.searchHitLimit != hitLimit { model.searchHitLimit = hitLimit }
                    }
                }
                return searchResults ?? []
            }
            if roots == nil {
                let rows = model.handle?.listDir(dir: "") ?? []
                roots = sorted(rows.map(ArchiveNode.init))
            }
            return roots ?? []
        }

        /// 子节点第一次要时才拉一层。
        private func children(of node: ArchiveNode) -> [ArchiveNode] {
            if node.children == nil {
                let rows = model.handle?.listDir(dir: node.row.path) ?? []
                node.children = sorted(rows.map(ArchiveNode.init))
            }
            return node.children ?? []
        }

        // MARK: 排序

        /// 目录永远在前，其余按当前列排序。
        private func sorted(_ nodes: [ArchiveNode]) -> [ArchiveNode] {
            let key = sortDescriptor?.key ?? ColumnSpec.name.rawValue
            let ascending = sortDescriptor?.ascending ?? true
            return nodes.sorted { a, b in
                let aDir = a.row.kind == .dir
                let bDir = b.row.kind == .dir
                if aDir != bDir { return aDir }
                let result: Bool
                switch key {
                case ColumnSpec.size.rawValue: result = a.row.size < b.row.size
                case ColumnSpec.packed.rawValue: result = (a.row.packedSize ?? 0) < (b.row.packedSize ?? 0)
                case ColumnSpec.mtime.rawValue: result = (a.row.mtime ?? 0) < (b.row.mtime ?? 0)
                case ColumnSpec.kind.rawValue: result = kindText(a.row).localizedStandardCompare(kindText(b.row)) == .orderedAscending
                default: result = a.row.name.localizedStandardCompare(b.row.name) == .orderedAscending
                }
                return ascending ? result : !result
            }
        }

        /// 排序变了：已经拉过的每一层都重排，节点对象不变所以展开状态保留。
        private func resort(_ nodes: inout [ArchiveNode]?) {
            guard var list = nodes else { return }
            list = sorted(list)
            for node in list {
                resort(&node.children)
            }
            nodes = list
        }

        func outlineView(_ outlineView: NSOutlineView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
            sortDescriptor = outlineView.sortDescriptors.first
            resort(&roots)
            resort(&searchResults)
            outlineView.reloadData()
        }

        // MARK: NSOutlineViewDataSource

        func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
            guard let node = item as? ArchiveNode else { return rootNodes().count }
            return children(of: node).count
        }

        func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
            guard let node = item as? ArchiveNode else { return rootNodes()[index] }
            return children(of: node)[index]
        }

        func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
            guard !isSearching, let node = item as? ArchiveNode else { return false }
            return node.isExpandable
        }

        func outlineView(_ outlineView: NSOutlineView, pasteboardWriterForItem item: Any) -> NSPasteboardWriting? {
            guard let node = item as? ArchiveNode else { return nil }
            return actions.dragExport(node.row)
        }

        // MARK: NSOutlineViewDelegate

        func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
            guard let column = tableColumn, let node = item as? ArchiveNode else { return nil }
            let row = node.row
            let cell = reusableCell(outlineView, id: column.identifier)
            switch column.identifier {
            case ColumnSpec.name:
                cell.imageView?.image = icon(for: row)
                // 搜索结果是平铺的，显示完整路径才知道在哪
                cell.textField?.stringValue = isSearching ? row.path : row.name
            case ColumnSpec.size:
                cell.textField?.stringValue = row.kind == .dir ? "—" : ByteText.string(row.size)
            case ColumnSpec.packed:
                cell.textField?.stringValue = row.kind == .dir ? "—" : (row.packedSize.map { ByteText.string($0) } ?? "—")
            case ColumnSpec.mtime:
                cell.textField?.stringValue = row.mtime.map { dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval($0))) } ?? "—"
            default:
                cell.textField?.stringValue = kindText(row)
            }
            return cell
        }

        func outlineViewSelectionDidChange(_ notification: Notification) {
            guard let outlineView else { return }
            model.selectedRows = outlineView.selectedRowIndexes.compactMap { (outlineView.item(atRow: $0) as? ArchiveNode)?.row }
        }

        // MARK: 动作

        /// 双击：目录展开 / 收起，文件预览。
        @objc func doubleClicked(_ sender: Any?) {
            guard let outlineView, outlineView.clickedRow >= 0,
                  let node = outlineView.item(atRow: outlineView.clickedRow) as? ArchiveNode else { return }
            if node.row.kind == .dir, !isSearching {
                if outlineView.isItemExpanded(node) {
                    outlineView.collapseItem(node)
                } else {
                    outlineView.expandItem(node)
                }
            } else {
                actions.preview(node.row)
            }
        }

        private var firstSelectedFile: Row? {
            model.selectedRows.first { $0.kind != .dir }
        }

        func previewSelection() {
            guard let row = firstSelectedFile else { return }
            actions.preview(row)
        }

        func openSelection() {
            guard let row = firstSelectedFile else { return }
            actions.openWithDefaultApp(row)
        }

        // MARK: 单元格

        /// 取可复用的单元格，没有就建一个：名称列带图标，其余只有文字。
        private func reusableCell(_ outlineView: NSOutlineView, id: NSUserInterfaceItemIdentifier) -> NSTableCellView {
            if let cell = outlineView.makeView(withIdentifier: id, owner: nil) as? NSTableCellView {
                return cell
            }
            let cell = NSTableCellView()
            cell.identifier = id
            let text = NSTextField(labelWithString: "")
            text.lineBreakMode = .byTruncatingMiddle
            text.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(text)
            cell.textField = text
            var leading: NSLayoutXAxisAnchor = cell.leadingAnchor
            var inset: CGFloat = 2
            if id == ColumnSpec.name {
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
                inset = 4
            } else if id == ColumnSpec.size || id == ColumnSpec.packed {
                text.alignment = .right
            }
            NSLayoutConstraint.activate([
                text.leadingAnchor.constraint(equalTo: leading, constant: inset),
                text.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
                text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            return cell
        }

        /// 按扩展名取系统图标，缓存起来。
        private func icon(for row: Row) -> NSImage {
            let key: String
            let type: UTType
            switch row.kind {
            case .dir:
                key = "/dir"
                type = .folder
            case .symlink:
                key = "/symlink"
                type = .symbolicLink
            case .file:
                let ext = (row.name as NSString).pathExtension.lowercased()
                key = ext
                type = UTType(filenameExtension: ext) ?? .data
            }
            if let cached = iconCache[key] { return cached }
            let image = NSWorkspace.shared.icon(for: type)
            image.size = NSSize(width: 16, height: 16)
            iconCache[key] = image
            return image
        }

        /// 「类型」列文字。
        private func kindText(_ row: Row) -> String {
            var text: String
            switch row.kind {
            case .dir: text = String(localized: "文件夹")
            case .symlink: text = String(localized: "符号链接")
            case .file:
                let ext = (row.name as NSString).pathExtension.lowercased()
                if let cached = kindTextCache[ext] {
                    text = cached
                } else {
                    text = ext.isEmpty ? String(localized: "文件") : (UTType(filenameExtension: ext)?.localizedDescription ?? String(localized: "\(ext.uppercased()) 文件"))
                    kindTextCache[ext] = text
                }
            }
            if row.encrypted {
                text += String(localized: "（加密）")
            }
            return text
        }
    }
}

/// 处理空格（Quick Look）和 ⌘↓（打开）的 outline。
@MainActor
final class KeyHandlingOutlineView: NSOutlineView {
    var onSpace: (() -> Void)?
    var onCommandDown: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        if event.charactersIgnoringModifiers == " " {
            onSpace?()
            return
        }
        if event.modifierFlags.contains(.command), event.specialKey == .downArrow {
            onCommandDown?()
            return
        }
        super.keyDown(with: event)
    }
}
