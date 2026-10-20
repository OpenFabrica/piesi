// Intents/Intents.swift —— App Intents（设计 6.4）：快捷指令 / Finder 快速操作里的「用 Piesi 解压」「用 Piesi 压缩」。
// 两个意图都走 app 内和双击 / 对话框完全相同的协调器：密码、编码、冲突、空间检查照常弹框。
// openAppWhenRun = false：快捷指令在后台拉起 app，不抢前台；做完没有别的窗口就按自动退出规则退出。
import AppIntents
import AppKit
import UniformTypeIdentifiers

/// 「用 Piesi 解压」：每个压缩包解到它旁边（智能建文件夹），返回解出来的目录。
struct ExtractArchivesIntent: AppIntent {
    static let title: LocalizedStringResource = "用 Piesi 解压"
    static let description = IntentDescription("把压缩包解压到它旁边，和双击一样。")
    static let openAppWhenRun = false

    @Parameter(title: "压缩包")
    var archives: [IntentFile]

    /// 逐个解压；任何一个失败就整体报错（协调器已经弹过具体原因）。
    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<[IntentFile]> {
        let delegate = AppDelegate.shared!
        delegate.beginHeadlessWork()
        defer { delegate.endHeadlessWork() }
        var outputs: [IntentFile] = []
        for file in archives {
            guard let url = file.fileURL else { throw IntentFailure.noFileURL(file.filename) }
            Log.app.notice("intent extract \(url.path, privacy: .public)")
            guard let root = await delegate.extractor.extractAll(archive: url, parent: nil) else {
                throw IntentFailure.extractFailed(url.lastPathComponent)
            }
            outputs.append(IntentFile(fileURL: root))
        }
        return .result(value: outputs)
    }
}

/// 「用 Piesi 压缩」：按预设把文件打成一个包，放在第一个文件旁边，返回压缩包。
struct CompressFilesIntent: AppIntent {
    static let title: LocalizedStringResource = "用 Piesi 压缩"
    static let description = IntentDescription("把文件按预设打成一个压缩包，放在第一个文件旁边。")
    static let openAppWhenRun = false

    @Parameter(title: "文件")
    var files: [IntentFile]
    @Parameter(title: "预设", default: .windows)
    var preset: CompressPreset
    @Parameter(title: "密码")
    var password: String?

    /// 命名规则和压缩对话框的默认值一致：单个源用源名，多个用目录名，重名加序号。
    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<IntentFile> {
        let delegate = AppDelegate.shared!
        delegate.beginHeadlessWork()
        defer { delegate.endHeadlessWork() }
        var sources: [URL] = []
        for file in files {
            guard let url = file.fileURL else { throw IntentFailure.noFileURL(file.filename) }
            sources.append(url)
        }
        guard let first = sources.first else { throw IntentFailure.noFiles }
        // 空白密码当没有：快捷指令里留空的文本框会传空串
        let trimmed = password?.trimmingCharacters(in: .whitespaces) ?? ""
        let destination = ArchiveNaming.freeDestination(
            directory: first.deletingLastPathComponent(),
            baseName: ArchiveNaming.defaultBaseName(for: sources),
            format: preset.format
        )
        let options = preset.options(destination: destination, sources: sources, password: trimmed.isEmpty ? nil : trimmed)
        Log.app.notice("intent compress \(destination.path, privacy: .public) preset=\(preset.rawValue, privacy: .public) sources=\(sources.count)")
        guard let archive = await delegate.compressor.runHeadless(options) else {
            throw IntentFailure.compressFailed(destination.lastPathComponent)
        }
        return .result(value: IntentFile(fileURL: archive))
    }
}

/// 预设卡片在快捷指令里的样子（名字和压缩对话框一致）。
extension CompressPreset: AppEnum {
    static let typeDisplayRepresentation = TypeDisplayRepresentation(name: "压缩预设")
    static let caseDisplayRepresentations: [CompressPreset: DisplayRepresentation] = [
        .windows: DisplayRepresentation(title: "发给 Windows"),
        .smallest: DisplayRepresentation(title: "最小体积"),
        .encrypted: DisplayRepresentation(title: "加密"),
        .mac: DisplayRepresentation(title: "Mac 专用"),
    ]
}

/// 意图失败原因：快捷指令会把这段话显示给用户。
enum IntentFailure: Error, CustomLocalizedStringResourceConvertible {
    case noFiles
    case noFileURL(String)
    case extractFailed(String)
    case compressFailed(String)

    var localizedStringResource: LocalizedStringResource {
        switch self {
        case .noFiles: LocalizedStringResource("没有收到文件")
        case .noFileURL(let name): LocalizedStringResource("快捷指令没有给出「\(name)」的文件路径")
        case .extractFailed(let name): LocalizedStringResource("解压「\(name)」失败")
        case .compressFailed(let name): LocalizedStringResource("创建「\(name)」失败")
        }
    }
}

/// Siri / Spotlight 里的 App 快捷指令；短语的各语言版本在 AppShortcuts.xcstrings。
struct PiesiShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: ExtractArchivesIntent(),
            phrases: ["用 \(.applicationName) 解压", "\(.applicationName) 解压"], // strings-lint: ignore
            shortTitle: "解压",
            systemImageName: "arrow.down.doc"
        )
        AppShortcut(
            intent: CompressFilesIntent(),
            phrases: ["用 \(.applicationName) 压缩", "\(.applicationName) 压缩"], // strings-lint: ignore
            shortTitle: "压缩",
            systemImageName: "doc.zipper"
        )
    }
}
