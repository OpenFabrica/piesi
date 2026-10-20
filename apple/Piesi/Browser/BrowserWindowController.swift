// BrowserWindowController.swift —— 浏览窗口（设计 6.1）：每个压缩包一个窗口，负责解压、校验、Quick Look、⌘↓ 打开、拖出。
import AppKit
import Quartz
import SwiftUI

/// Quick Look 的预览项：临时文件名是 <id>-<名字>，面板标题要显示原名。
final class PreviewItem: NSObject, QLPreviewItem, @unchecked Sendable {
    let previewItemURL: URL!
    let previewItemTitle: String!
    let rowID: UInt32

    init(url: URL, title: String, rowID: UInt32) {
        previewItemURL = url
        previewItemTitle = title
        self.rowID = rowID
    }
}

/// 浏览窗口控制器。控制器自己持有引用，关窗口时释放；也是 Quick Look 面板的数据源。
@MainActor
final class BrowserWindowController: NSWindowController, NSWindowDelegate, QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    private static var openControllers: [BrowserWindowController] = []
    let model: BrowserModel
    /// 预览专用的第二个句柄：Rust 解码期间持锁，不能占浏览用的那个，否则主线程的 listDir / search 会卡
    private var previewHandle: ArchiveHandle?
    /// 当前预览的取消句柄：换预览或关窗时取消，解码不会在后台一直跑
    private var previewCancel: CancelHandle?
    /// Quick Look 当前的预览项（临时文件由 Rust 写在 temp/piesi-preview-<pid>/）
    private var previewItem: PreviewItem?
    /// ⌘↓ 交给外部 app 打开过的临时文件：换预览时不能删，别的 app 可能还开着
    private var externallyOpened: Set<URL> = []
    /// forData 的包拖出时正在问密码：一次拖动多项只问一次
    private var isAskingPassword = false

    /// 开一个新窗口浏览 url。
    static func open(url: URL) {
        let controller = BrowserWindowController(url: url)
        openControllers.append(controller)
        controller.showWindow(nil)
        NSApp.activate()
        Task { @MainActor in
            await controller.model.load(parent: controller.window)
        }
    }

    private init(url: URL) {
        model = BrowserModel(url: url)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 820, height: 540),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = url.lastPathComponent
        window.representedURL = url
        window.isReleasedWhenClosed = false
        window.toolbarStyle = .unified
        super.init(window: window)
        // 闭包只弱持有自己：窗口 → hosting → 视图 → 控制器 会成环
        let actions = BrowserActions(
            extractAll: { [weak self] in self?.extract(all: true) },
            extractSelected: { [weak self] in self?.extract(all: false) },
            verify: { [weak self] in self?.verify() },
            outline: OutlineActions(
                preview: { [weak self] row in self?.preview(row) },
                openWithDefaultApp: { [weak self] row in self?.openWithDefaultApp(row) },
                dragExport: { [weak self] row in self?.promiseProvider(for: row) }
            )
        )
        let hosting = NSHostingController(rootView: BrowserView(model: model, actions: actions))
        // macOS 14：让 SwiftUI 的 .toolbar 填进 NSWindow 的工具栏
        hosting.sceneBridgingOptions = [.toolbars]
        // 不让 SwiftUI 的最小尺寸反过来决定窗口大小，否则窗口会被压到 minWidth
        hosting.sizingOptions = []
        window.contentViewController = hosting
        window.contentMinSize = NSSize(width: 640, height: 400)
        window.setContentSize(NSSize(width: 900, height: 560))
        window.center()
        window.setFrameAutosaveName("BrowserWindow")
        window.delegate = self
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("不用 nib")
    }

    /// 窗口关掉：停掉还在跑的预览解码、关 Quick Look、删临时文件、把自己从列表里拿掉（句柄随 model 释放）。
    func windowWillClose(_ notification: Notification) {
        previewCancel?.cancel()
        if let panel = QLPreviewPanel.sharedPreviewPanelExists() ? QLPreviewPanel.shared() : nil, panel.isVisible, panel.dataSource === self {
            panel.close()
        }
        discardPreviewFile()
        Self.openControllers.removeAll { $0 === self }
    }

    // MARK: 解压

    /// 全部 / 选中 → 选目录 → 走 ExtractCoordinator（它另开句柄，沿用这里的编码和密码）。
    private func extract(all: Bool) {
        let rows = all ? [] : model.selectedRows
        guard all || !rows.isEmpty, let window else { return }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = String(localized: "解压到此处")
        panel.message = all ? String(localized: "选择解压位置") : String(localized: "选择解压位置（\(rows.count) 项）")
        panel.directoryURL = model.url.deletingLastPathComponent()
        Task { @MainActor in
            guard await panel.beginSheetModal(for: window) == .OK, let dest = panel.url else { return }
            let ids = all ? [] : model.extractionIDs(for: rows)
            await AppDelegate.shared.extractor.extract(archive: model.url, ids: ids, dest: dest, presets: model.presets, parent: window)
        }
    }

    // MARK: 校验

    /// [测试]：另开一个句柄（校验期间持锁，不能占浏览用的那个），解码 + CRC 不写盘，结果用 NSAlert 显示。
    /// forData 的包先要密码，否则解码直接失败。
    private func verify() {
        guard model.isLoaded, let window else { return }
        let name = model.url.lastPathComponent
        Task { @MainActor in
            guard await model.ensurePassword(parent: window, wrongBefore: false) else { return }
            let presets = model.presets
            let handle: ArchiveHandle
            do {
                handle = try await CoreBridge.open(model.url)
                if let password = presets.password {
                    try await CoreBridge.setPassword(handle, password)
                }
                if let encoding = presets.encoding {
                    handle.setEncoding(encoding: encoding)
                }
            } catch {
                await Alerts.error(String(localized: "无法校验「\(name)」"), CoreBridge.message(for: error), in: window)
                return
            }
            let center = AppDelegate.shared.taskCenter
            let item = TaskItem(kind: .verify, title: name, totalBytes: model.totalBytes)
            center.add(item)
            AppDelegate.shared.tasksWindow.show()
            do {
                let result = try await CoreBridge.verify(handle, ids: [], listener: ProgressRelay(item: item), cancel: item.cancelHandle)
                center.finish(item, state: .done)
                Log.browser.notice("verify ok \(name, privacy: .public): entries=\(result.entries) skipped=\(result.skipped) warnings=\(result.warnings.count)")
                let alert = NSAlert()
                alert.messageText = String(localized: "校验通过：\(result.entries) 项")
                var details: [String] = []
                if result.skipped > 0 {
                    details.append(String(localized: "跳过 \(result.skipped) 项"))
                }
                details.append(contentsOf: result.warnings.prefix(5).map(\.localizedText))
                alert.informativeText = details.isEmpty ? String(localized: "所有条目解码和 CRC 都正常。") : details.joined(separator: "\n")
                alert.addButton(withTitle: String(localized: "好"))
                _ = await Alerts.present(alert, in: window)
            } catch {
                if CoreBridge.isCancelled(error) {
                    center.finish(item, state: .cancelled)
                    return
                }
                let message = CoreBridge.message(for: error)
                center.finish(item, state: .failed(message))
                Log.browser.error("verify failed \(name, privacy: .public): \(message, privacy: .public)")
                await Alerts.error(String(localized: "「\(name)」校验失败"), message, in: window)
            }
        }
    }

    // MARK: 预览与打开

    /// 懒开预览句柄，并把窗口当前的密码 / 编码同步过去（用户可能刚切了编码）。
    private func preparedPreviewHandle() async throws -> ArchiveHandle {
        let handle: ArchiveHandle
        if let existing = previewHandle {
            handle = existing
        } else {
            handle = try await CoreBridge.open(model.url)
            previewHandle = handle
        }
        let presets = model.presets
        if let password = presets.password {
            try await CoreBridge.setPassword(handle, password)
        }
        if let encoding = presets.encoding {
            handle.setEncoding(encoding: encoding)
        }
        return handle
    }

    /// 解单个条目到临时文件；forData 的包先要密码，密码错了带「错误」提示再要一次（最多三次）。
    private func temporaryFile(for row: Row) async -> URL? {
        guard model.isLoaded, row.kind == .file else { return nil }
        var wrong = false
        for _ in 0..<3 {
            guard await model.ensurePassword(parent: window, wrongBefore: wrong) else { return nil }
            do {
                let handle = try await preparedPreviewHandle()
                previewCancel?.cancel()
                let cancel = CancelHandle()
                previewCancel = cancel
                return try await CoreBridge.extractToTemp(handle, id: row.id, cancel: cancel)
            } catch {
                if CoreBridge.isCancelled(error) {
                    return nil
                }
                if CoreBridge.isWrongPassword(error) {
                    model.forgetPassword()
                    wrong = true
                    continue
                }
                await Alerts.error(String(localized: "无法预览「\(row.name)」"), CoreBridge.message(for: error), in: window)
                return nil
            }
        }
        return nil
    }

    /// 删掉上一个 Quick Look 临时文件（换预览或关窗时）；⌘↓ 打开过的留到退出时一起清。
    private func discardPreviewFile() {
        if let url = previewItem?.previewItemURL, !externallyOpened.contains(url) {
            try? FileManager.default.removeItem(at: url)
        }
        previewItem = nil
    }

    /// 双击 / 空格：同一条目且面板开着就关掉（空格的切换习惯），否则解到临时文件再打开面板。
    private func preview(_ row: Row) {
        if let panel = QLPreviewPanel.sharedPreviewPanelExists() ? QLPreviewPanel.shared() : nil, panel.isVisible,
           previewItem?.rowID == row.id {
            panel.close()
            return
        }
        Task { @MainActor in
            // 先删旧文件再解：同一条目关掉面板后再预览，新旧临时路径相同，后删会把刚解出来的文件删掉
            discardPreviewFile()
            guard let url = await temporaryFile(for: row) else { return }
            previewItem = PreviewItem(url: url, title: row.name, rowID: row.id)
            Log.browser.notice("preview \(row.path, privacy: .public)")
            let panel = QLPreviewPanel.shared()!
            if panel.isVisible {
                panel.reloadData()
            } else {
                panel.makeKeyAndOrderFront(nil)
            }
        }
    }

    /// ⌘↓：临时解压后用默认 app 打开。文件留到退出时一起清，别的 app 可能还开着它。
    private func openWithDefaultApp(_ row: Row) {
        Task { @MainActor in
            guard let url = await temporaryFile(for: row) else { return }
            externallyOpened.insert(url)
            Log.browser.notice("open with default app \(row.path, privacy: .public)")
            NSWorkspace.shared.open(url)
        }
    }

    /// 拖出：交给 NSFilePromiseProvider，在后台解。forData 的包没密码就先问一次（多选拖出也只问一次），这次拖动不给承诺。
    private func promiseProvider(for row: Row) -> NSPasteboardWriting? {
        guard model.isLoaded else { return nil }
        if model.passwordState == .forData, model.password == nil {
            if !isAskingPassword {
                isAskingPassword = true
                Task { @MainActor in
                    _ = await model.ensurePassword(parent: window, wrongBefore: false)
                    isAskingPassword = false
                }
            }
            return nil
        }
        return EntryPromiseDelegate.makeProvider(archiveURL: model.url, row: row, presets: model.presets)
    }

    // MARK: Quick Look（响应链找到这里当数据源）

    override func acceptsPreviewPanelControl(_ panel: QLPreviewPanel!) -> Bool {
        true
    }

    // 这两个是 NSObject 分类上的非隔离方法，Quick Look 在主线程调；assumeIsolated 后才能碰面板的主线程属性
    override func beginPreviewPanelControl(_ panel: QLPreviewPanel!) {
        MainActor.assumeIsolated {
            panel.dataSource = self
            panel.delegate = self
        }
    }

    override func endPreviewPanelControl(_ panel: QLPreviewPanel!) {
        MainActor.assumeIsolated {
            panel.dataSource = nil
            panel.delegate = nil
        }
    }

    nonisolated func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int {
        MainActor.assumeIsolated { previewItem == nil ? 0 : 1 }
    }

    nonisolated func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> (any QLPreviewItem)! {
        MainActor.assumeIsolated { previewItem }
    }
}
