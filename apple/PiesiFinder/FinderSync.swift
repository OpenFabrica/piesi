// FinderSync.swift —— Finder 右键菜单扩展：在右键菜单顶层放「用 Piesi 解压到此处 / 查看 / 压缩」，点了交给主 app 去做。
import Cocoa
import FinderSync
import os
import UniformTypeIdentifiers

/// Finder Sync 扩展的主类。
/// 为什么用 Finder Sync：「服务」和「快速操作」只能进二级菜单；要像 Gemini 那样放在右键菜单顶层、带图标，只有它能做到。
/// 它本来是给同步盘用的，只对 directoryURLs 下面的文件出菜单，所以监视根目录 "/" 和所有已挂载的卷，等于所有位置都有；
/// 不设角标，不碰文件。
final class FinderSync: FIFinderSync {
    private let log = Logger(subsystem: "com.citrolabs.piesi", category: "finder")

    /// 扩展所在的主 app（…/Piesi.app/Contents/PlugIns/PiesiFinder.appex 往上三层）
    private static let appURL = Bundle.main.bundleURL
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

    /// 出「解压」菜单的压缩包类型，写在本扩展 Info.plist 的 PiesiArchiveTypes。
    /// 为什么不读主 app 的 CFBundleDocumentTypes：沙盒不让扩展读外层 app 的 Info.plist；两份列表要保持一致（Quick Look 扩展同理）。
    private static let archiveTypes: [UTType] = {
        let list = Bundle.main.object(forInfoDictionaryKey: "PiesiArchiveTypes") as? [String] ?? []
        return list.compactMap { UTType($0) }
    }()

    override init() {
        super.init()
        updateDirectories()
        let center = NSWorkspace.shared.notificationCenter
        center.addObserver(self, selector: #selector(volumesChanged(_:)), name: NSWorkspace.didMountNotification, object: nil)
        center.addObserver(self, selector: #selector(volumesChanged(_:)), name: NSWorkspace.didUnmountNotification, object: nil)
        log.notice("finder extension started, \(Self.archiveTypes.count) archive types")
    }

    /// 监视 "/" 加上所有已挂载的卷。为什么要单独加卷：实测只监视 "/" 时，U 盘、磁盘映像、网络盘里没有菜单。
    private func updateDirectories() {
        var directories: Set<URL> = [URL(fileURLWithPath: "/")]
        let volumes = FileManager.default.mountedVolumeURLs(includingResourceValuesForKeys: nil, options: [.skipHiddenVolumes]) ?? []
        directories.formUnion(volumes)
        FIFinderSyncController.default().directoryURLs = directories
    }

    /// 插拔磁盘后重新设监视范围。
    @objc private func volumesChanged(_ notification: Notification) {
        updateDirectories()
    }

    /// 压缩菜单始终可用；全是压缩包时另提供解压与单包查看。
    override func menu(for menuKind: FIMenuKind) -> NSMenu? {
        guard menuKind == .contextualMenuForItems,
              let urls = FIFinderSyncController.default().selectedItemURLs(), !urls.isEmpty else { return nil }
        return Self.onMain {
            let menu = NSMenu(title: "")
            if urls.allSatisfy(Self.isArchive) {
                menu.addItem(Self.item(String(localized: "用 Piesi 解压到此处"), #selector(self.extract(_:))))
                if urls.count == 1 {
                    menu.addItem(Self.item(String(localized: "用 Piesi 查看"), #selector(self.browse(_:))))
                }
            }
            let compress = Self.item(String(localized: "用 Piesi 压缩"), #selector(self.compress(_:)))
            let singleFile = urls.count == 1 && (try? urls[0].resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])).map { $0.isRegularFile == true && $0.isSymbolicLink != true } == true
            compress.submenu = Self.compressionMenu(singleFile: singleFile)
            menu.addItem(compress)
            return menu
        }
    }

    /// 按输入能力构造真正的格式菜单，和请求交接测试共用。
    @MainActor
    static func compressionMenu(singleFile: Bool) -> NSMenu {
        let menu = NSMenu(title: "")
        for (index, format) in CompressionFormat.allCases.enumerated() where !format.isSingleFile || singleFile {
            let entry = NSMenuItem(title: format.title, action: #selector(compressFormat(_:)), keyEquivalent: "")
            // Finder 跨进程转发菜单只保留 tag，不传 representedObject。0 留给无格式的菜单项。
            entry.tag = index + 1
            menu.addItem(entry)
        }
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: String(localized: "自定义压缩…"), action: #selector(compress(_:)), keyEquivalent: ""))
        return menu
    }

    /// 从 Finder 回传的菜单 tag 恢复格式；无格式菜单项返回 nil。
    static func compressionFormat(menuTag: Int) -> CompressionFormat? {
        let formats = CompressionFormat.allCases
        guard menuTag > 0, menuTag <= formats.count else { return nil }
        return formats[menuTag - 1]
    }

    /// 在主线程上建菜单。为什么：Finder 从后台队列调 menu(for:)（实测），而 NSMenu 只能在主线程用；
    /// 扩展进程的主线程平时空闲，同步切过去不会卡住。NSMenu 不是 Sendable，借一个变量带出来。
    private static func onMain(_ build: @MainActor () -> NSMenu) -> NSMenu {
        nonisolated(unsafe) var result: NSMenu?
        if Thread.isMainThread {
            MainActor.assumeIsolated { result = build() }
        } else {
            DispatchQueue.main.sync {
                MainActor.assumeIsolated { result = build() }
            }
        }
        return result ?? NSMenu(title: "")
    }

    @objc func extract(_ sender: AnyObject?) {
        send(.extract)
    }

    @objc func browse(_ sender: AnyObject?) {
        send(.browse)
    }

    @objc func compress(_ sender: AnyObject?) {
        send(.compress)
    }

    /// 格式菜单直接创建，省去确认对话框。
    @objc func compressFormat(_ sender: NSMenuItem) {
        guard let format = Self.compressionFormat(menuTag: sender.tag) else {
            log.error("unknown compression menu tag: \(sender.tag)")
            return
        }
        send(.compress, format: format)
    }

    /// 把当前选中的文件连同命令交给主 app。指定用扩展所在的这份 app 打开链接，机器上有多份 Piesi 时不会跑错。
    private func send(_ action: FinderRequest.Action, format: CompressionFormat? = nil) {
        guard let urls = FIFinderSyncController.default().selectedItemURLs(), !urls.isEmpty else { return }
        do {
            let link = try FinderHandoff.post(FinderRequest(action: action, paths: urls.map(\.path), format: format))
            log.notice("finder \(action.rawValue, privacy: .public): \(urls.count) items")
            NSWorkspace.shared.open([link], withApplicationAt: Self.appURL, configuration: NSWorkspace.OpenConfiguration()) { [log] _, error in
                if let error {
                    log.error("open app failed: \(error.localizedDescription, privacy: .public)")
                }
            }
        } catch {
            log.error("post request failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// 按扩展名判断是不是压缩包（扩展在沙盒里，不读文件内容）；文件夹一律不算。
    private static func isArchive(_ url: URL) -> Bool {
        guard !url.hasDirectoryPath, let type = UTType(filenameExtension: url.pathExtension) else { return false }
        return archiveTypes.contains { type.conforms(to: $0) }
    }

    /// 带 Piesi 图标的菜单项（Finder 忽略 target，直接把 action 发给扩展实例）。
    @MainActor
    private static func item(_ title: String, _ action: Selector) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        let icon = NSWorkspace.shared.icon(forFile: appURL.path)
        icon.size = NSSize(width: 16, height: 16)
        item.image = icon
        return item
    }
}
