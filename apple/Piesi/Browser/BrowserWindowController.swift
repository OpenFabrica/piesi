// BrowserWindowController.swift —— 浏览窗口（设计 6.1）：每个压缩包一个窗口，负责解压、Quick Look、⌘↓ 打开、拖出。
import AppKit
import Quartz
import SwiftUI

/// 浏览窗口控制器。控制器自己持有引用，关窗口时释放；也是 Quick Look 面板的数据源。
@MainActor
final class BrowserWindowController: NSWindowController, NSWindowDelegate, QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    private static var openControllers: [BrowserWindowController] = []
    let model: BrowserModel
    /// Quick Look 当前预览的临时文件（Rust 解到 temp/piesi-preview-<pid>/，退出时整个删掉）
    private var previewURL: URL?

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

    /// 窗口关掉就把自己从列表里拿掉，句柄随 model 释放。
    func windowWillClose(_ notification: Notification) {
        if let panel = QLPreviewPanel.sharedPreviewPanelExists() ? QLPreviewPanel.shared() : nil, panel.isVisible, panel.dataSource === self {
            panel.close()
        }
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
        panel.prompt = "解压到此处"
        panel.message = all ? "选择解压位置" : "选择解压位置（\(rows.count) 项）"
        panel.directoryURL = model.url.deletingLastPathComponent()
        Task { @MainActor in
            guard await panel.beginSheetModal(for: window) == .OK, let dest = panel.url else { return }
            let ids = all ? [] : model.extractionIDs(for: rows)
            await AppDelegate.shared.extractor.extract(archive: model.url, ids: ids, dest: dest, presets: model.presets, parent: window)
        }
    }

    // MARK: 校验

    /// [测试]：另开一个句柄（校验期间持锁，不能占浏览用的那个），解码 + CRC 不写盘，结果用 NSAlert 显示。
    private func verify() {
        guard model.isLoaded, let window else { return }
        let name = model.url.lastPathComponent
        let presets = model.presets
        Task { @MainActor in
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
                await Alerts.error("无法校验「\(name)」", CoreBridge.message(for: error), in: window)
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
                alert.messageText = "校验通过：\(result.entries) 项"
                var details: [String] = []
                if result.skipped > 0 {
                    details.append("跳过 \(result.skipped) 项")
                }
                details.append(contentsOf: result.warnings.prefix(5))
                alert.informativeText = details.isEmpty ? "所有条目解码和 CRC 都正常。" : details.joined(separator: "\n")
                alert.addButton(withTitle: "好")
                _ = await Alerts.present(alert, in: window)
            } catch {
                if CoreBridge.isCancelled(error) {
                    center.finish(item, state: .cancelled)
                    return
                }
                let message = CoreBridge.message(for: error)
                center.finish(item, state: .failed(message))
                Log.browser.error("verify failed \(name, privacy: .public): \(message, privacy: .public)")
                await Alerts.error("「\(name)」校验失败", message, in: window)
            }
        }
    }

    // MARK: 预览与打开

    /// 解单个条目到临时文件；forData 的包先要密码，密码错了再要一次。
    private func temporaryFile(for row: Row) async -> URL? {
        guard let handle = model.handle, row.kind == .file else { return nil }
        for _ in 0..<3 {
            guard await model.ensurePassword(parent: window) else { return nil }
            do {
                return try await CoreBridge.extractToTemp(handle, id: row.id, cancel: CancelHandle())
            } catch {
                if CoreBridge.isWrongPassword(error) {
                    model.forgetPassword()
                    continue
                }
                await Alerts.error("无法预览「\(row.name)」", CoreBridge.message(for: error), in: window)
                return nil
            }
        }
        return nil
    }

    /// 双击 / 空格：已经开着 Quick Look 就关掉（空格的切换习惯），否则解到临时文件再打开面板。
    private func preview(_ row: Row) {
        if let panel = QLPreviewPanel.sharedPreviewPanelExists() ? QLPreviewPanel.shared() : nil, panel.isVisible,
           previewURL?.lastPathComponent == row.name {
            panel.close()
            return
        }
        Task { @MainActor in
            guard let url = await temporaryFile(for: row) else { return }
            previewURL = url
            Log.browser.notice("preview \(row.path, privacy: .public)")
            let panel = QLPreviewPanel.shared()!
            if panel.isVisible {
                panel.reloadData()
            } else {
                panel.makeKeyAndOrderFront(nil)
            }
        }
    }

    /// ⌘↓：临时解压后用默认 app 打开。
    private func openWithDefaultApp(_ row: Row) {
        Task { @MainActor in
            guard let url = await temporaryFile(for: row) else { return }
            Log.browser.notice("open with default app \(row.path, privacy: .public)")
            NSWorkspace.shared.open(url)
        }
    }

    /// 拖出：交给 NSFilePromiseProvider，在后台解。
    private func promiseProvider(for row: Row) -> NSPasteboardWriting? {
        guard model.isLoaded else { return nil }
        // forData 的包没密码就拖不出来：先要一次，这里不能 await，只能提示
        if model.passwordState == .forData, model.password == nil {
            Task { @MainActor in _ = await model.ensurePassword(parent: window) }
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
        MainActor.assumeIsolated { previewURL == nil ? 0 : 1 }
    }

    nonisolated func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> (any QLPreviewItem)! {
        MainActor.assumeIsolated { previewURL as NSURL? }
    }
}
