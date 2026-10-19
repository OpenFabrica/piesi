// BrowserWindowController.swift —— 浏览窗口（设计 6.1）。M1 先把窗口和打开流程立起来，列表在 M2 用 NSOutlineView 接。
import AppKit
import SwiftUI

/// 浏览窗口控制器：每个压缩包一个窗口；控制器自己持有引用，关窗口时释放。
@MainActor
final class BrowserWindowController: NSWindowController, NSWindowDelegate {
    private static var openControllers: [BrowserWindowController] = []
    let model: BrowserModel

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
            contentRect: NSRect(x: 0, y: 0, width: 760, height: 520),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = url.lastPathComponent
        window.representedURL = url
        window.isReleasedWhenClosed = false
        window.contentViewController = NSHostingController(rootView: BrowserView(model: model))
        window.setFrameAutosaveName("BrowserWindow")
        window.center()
        super.init(window: window)
        window.delegate = self
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("不用 nib")
    }

    /// 窗口关掉就把自己从列表里拿掉，句柄随 model 释放。
    func windowWillClose(_ notification: Notification) {
        Self.openControllers.removeAll { $0 === self }
    }
}

/// 浏览窗口的状态：打开句柄、密码、编码。列表数据由 M2 的 outline 按层级向句柄拉取。
@MainActor
@Observable
final class BrowserModel {
    let url: URL
    private(set) var handle: ArchiveHandle?
    private(set) var formatName = ""
    private(set) var entryCount: UInt32 = 0
    private(set) var encodingInfo: EncodingInfo?
    private(set) var passwordState: PasswordState = .notNeeded
    private(set) var errorMessage: String?
    /// 用户输入过的密码，解压时沿用
    private(set) var password: String?

    init(url: URL) {
        self.url = url
    }

    /// 后台打开；头加密（forNames）的包先要密码，forData 留到解压时再要。
    func load(parent: NSWindow?) async {
        let name = url.lastPathComponent
        do {
            let opened = try await CoreBridge.open(url)
            if opened.passwordState() == .forNames {
                var wrong = false
                while true {
                    guard let entered = await PasswordDialog.ask(archiveName: name, wrongBefore: wrong, parent: parent) else {
                        errorMessage = "需要密码才能列出内容"
                        return
                    }
                    do {
                        try await CoreBridge.setPassword(opened, entered)
                        password = entered
                        break
                    } catch {
                        guard CoreBridge.isWrongPassword(error) else { throw error }
                        wrong = true
                    }
                }
            }
            handle = opened
            formatName = opened.formatName()
            entryCount = opened.entryCount()
            encodingInfo = opened.encodingInfo()
            passwordState = opened.passwordState()
            Log.browser.notice("opened \(self.url.path, privacy: .public) format=\(self.formatName, privacy: .public) entries=\(self.entryCount)")
        } catch {
            errorMessage = CoreBridge.message(for: error)
            Log.browser.error("open failed \(self.url.path, privacy: .public): \(self.errorMessage ?? "", privacy: .public)")
        }
    }

    /// 解压时沿用的编码 / 密码。
    var presets: ExtractPresets {
        ExtractPresets(encoding: encodingInfo?.encoding, password: password)
    }
}

/// 浏览窗口内容（M1：只有状态行；M2 加 NSOutlineView 列表）。
struct BrowserView: View {
    let model: BrowserModel

    var body: some View {
        VStack {
            if let error = model.errorMessage {
                ContentUnavailableView("无法打开", systemImage: "exclamationmark.triangle", description: Text(error))
            } else if model.handle == nil {
                ProgressView("正在打开…")
            } else {
                Text("\(model.formatName) · \(model.entryCount) 个条目")
                    .foregroundStyle(.secondary)
            }
        }
        .frame(minWidth: 600, minHeight: 400)
    }
}
