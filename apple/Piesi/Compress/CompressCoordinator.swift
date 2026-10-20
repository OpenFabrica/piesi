// CompressCoordinator.swift —— 压缩流程（设计 6.2）：收集源 → 对话框（预设 / 高级）→ 后台 createArchive + 进度 → 收尾。
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
    /// 保留 Finder 标签、xattr、资源分支（写成 __MACOSX/._ AppleDouble，和 ditto 一致）；只有 zip 支持
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
        await run(options, parent: parent)
    }

    /// 后台创建；失败或取消时 Rust 会删掉写了一半的输出。
    private func run(_ options: CompressOptions, parent: NSWindow?) async {
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
        } catch {
            if CoreBridge.isCancelled(error) {
                taskCenter.finish(item, state: .cancelled)
                Log.create.notice("create cancelled \(name, privacy: .public)")
                return
            }
            let message = CoreBridge.message(for: error)
            taskCenter.finish(item, state: .failed(message))
            Log.create.error("create failed \(name, privacy: .public): \(message, privacy: .public)")
            await Alerts.error(String(localized: "创建「\(name)」失败"), message, in: parent)
        }
    }
}
