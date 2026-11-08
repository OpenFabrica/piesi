// CompressCoordinator.swift —— 压缩流程：选定格式直接创建，或经对话框自定义后后台创建。
import AppKit

/// 对话框确认后的全部参数。
struct CompressOptions: Sendable {
    var destination: URL
    var sources: [URL]
    var format: CreateFormat
    var level: UInt8
    var password: String?
    var encryptNames: Bool
    var skipJunk: Bool
    var windowsSafe: Bool
    /// 保留 Finder 标签、xattr、资源分支；ZIP 与 TAR 系列支持
    var macMetadata: Bool
}

/// 压缩流程协调器。
@MainActor
final class CompressCoordinator {
    private let taskCenter: TaskCenter
    private let settings: AppSettings
    private let tasksWindow: TasksWindowController

    init(taskCenter: TaskCenter, settings: AppSettings, tasksWindow: TasksWindowController) {
        self.taskCenter = taskCenter
        self.settings = settings
        self.tasksWindow = tasksWindow
    }

    /// 入口：Services「用 Piesi 压缩」、主窗口「新建压缩包…」、拖普通文件到 Dock / 主窗口。做完才返回。
    func start(sources: [URL], parent: NSWindow?) async {
        guard !sources.isEmpty else { return }
        Log.create.notice("compress dialog for \(sources.count) sources")
        guard let options = await CompressDialog.run(sources: sources, parent: parent) else {
            Log.create.notice("compress cancelled in dialog")
            return
        }
        _ = await run(options, parent: parent)
    }

    /// Finder 格式菜单：在源目录创建，同名自动加序号，默认不改写文件名。
    func quickCreate(sources: [URL], format: CreateFormat) async {
        guard let first = sources.first else { return }
        let destination = ArchiveNaming.freeDestination(directory: first.deletingLastPathComponent(), baseName: ArchiveNaming.defaultBaseName(for: sources), format: format)
        _ = await runHeadless(CompressOptions(destination: destination, sources: sources, format: format, level: 5, password: nil, encryptNames: false, skipJunk: true, windowsSafe: false, macMetadata: false))
    }
    /// 无界面入口（-PiesiCompress 测试参数、App Intents）：不弹对话框，按给定参数直接压。成功返回输出路径。
    @discardableResult
    func runHeadless(_ options: CompressOptions) async -> URL? {
        Log.create.notice("headless compress \(options.destination.path, privacy: .public) sources=\(options.sources.count)")
        return await run(options, parent: nil) ? options.destination : nil
    }

    /// 后台创建；失败或取消时 Rust 会删掉写了一半的输出。返回是否成功。
    private func run(_ options: CompressOptions, parent: NSWindow?) async -> Bool {
        let name = options.destination.lastPathComponent
        let request = CreateRequest(
            dest: options.destination.path,
            sources: options.sources.map(\.path),
            format: options.format,
            level: options.level,
            password: options.password,
            encryptNames: options.encryptNames,
            skipJunk: options.skipJunk,
            windowsSafe: options.windowsSafe,
            macMetadata: options.macMetadata
        )
        let item = TaskItem(kind: .create, title: name, totalBytes: nil)
        taskCenter.add(item)
        tasksWindow.show()
        do {
            let result = try await CoreBridge.create(request, listener: ProgressRelay(item: item), cancel: item.cancelHandle)
            taskCenter.finish(item, state: .done)
            Log.create.notice("created \(options.destination.path, privacy: .public): entries=\(result.entries) in=\(result.inputBytes) out=\(result.outputBytes) junk=\(result.skippedJunk) warnings=\(result.warnings.count)")
            for warning in result.warnings {
                Log.create.warning("create \(name, privacy: .public): \(warning.localizedText, privacy: .public)")
            }
            if settings.revealInFinder {
                NSWorkspace.shared.activateFileViewerSelecting([options.destination])
            }
            return true
        } catch {
            if CoreBridge.isCancelled(error) {
                taskCenter.finish(item, state: .cancelled)
                Log.create.notice("create cancelled \(name, privacy: .public)")
                return false
            }
            let message = CoreBridge.message(for: error)
            taskCenter.finish(item, state: .failed(message))
            Log.create.error("create failed \(name, privacy: .public): \(message, privacy: .public)")
            await Alerts.error(String(localized: "创建「\(name)」失败"), message, in: parent)
            return false
        }
    }
}
