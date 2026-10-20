// DragExport.swift —— 从浏览窗口拖条目到 Finder：NSFilePromiseProvider 在后台队列里开自己的句柄，解到临时目录再搬到 Finder 给的位置。
import AppKit
import UniformTypeIdentifiers

/// 文件承诺的委托。每个被拖的条目一个实例；写入在共用的串行队列上同步完成。
final class EntryPromiseDelegate: NSObject, NSFilePromiseProviderDelegate, @unchecked Sendable {
    private let archiveURL: URL
    private let row: Row
    private let presets: ExtractPresets
    /// 所有拖出共用一条串行队列：多选拖 N 项不会并发开 N 个句柄同时解码。
    private static let sharedQueue: OperationQueue = {
        let q = OperationQueue()
        q.name = "piesi.drag-export"
        q.maxConcurrentOperationCount = 1
        return q
    }()

    init(archiveURL: URL, row: Row, presets: ExtractPresets) {
        self.archiveURL = archiveURL
        self.row = row
        self.presets = presets
    }

    /// 建 provider：类型按扩展名，目录是 public.folder。
    static func makeProvider(archiveURL: URL, row: Row, presets: ExtractPresets) -> NSFilePromiseProvider {
        let type: UTType
        switch row.kind {
        case .dir: type = .folder
        case .symlink: type = .symbolicLink
        case .file: type = UTType(filenameExtension: (row.name as NSString).pathExtension) ?? .data
        }
        let delegate = EntryPromiseDelegate(archiveURL: archiveURL, row: row, presets: presets)
        let provider = NSFilePromiseProvider(fileType: type.identifier, delegate: delegate)
        // provider 只弱持有 delegate，挂到 userInfo 上续命到拖动结束
        provider.userInfo = delegate
        return provider
    }

    /// Finder 问「落地叫什么名字」：用条目自己的名字。
    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, fileNameForType fileType: String) -> String {
        row.name
    }

    /// 写入在哪条队列上跑：共用的串行后台队列，不占主线程。
    func operationQueue(for filePromiseProvider: NSFilePromiseProvider) -> OperationQueue {
        Self.sharedQueue
    }

    /// Finder 给了目标位置：解出来搬过去；失败回主线程弹一次提示，并把错误交回 Finder。
    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, writePromiseTo url: URL, completionHandler: @escaping (Error?) -> Void) {
        do {
            try DragExport.write(archive: archiveURL, row: row, presets: presets, to: url)
            completionHandler(nil)
        } catch {
            let message = CoreBridge.message(for: error)
            let name = row.name
            Log.browser.error("drag export failed \(self.row.path, privacy: .public): \(message, privacy: .public)")
            Task { @MainActor in
                await Alerts.error(String(localized: "拖出「\(name)」失败"), message, in: nil)
            }
            completionHandler(error)
        }
    }
}

/// 实际的导出：阻塞式，只在后台队列调。
enum DragExport {
    /// 选中解压不会剥掉包内路径（a/b/c.txt 解出来还是 a/b/c.txt），所以先解到临时目录，再把对应路径搬到目标。
    static func write(archive: URL, row: Row, presets: ExtractPresets, to destination: URL) throws {
        let handle = try openArchive(path: archive.path, locale: nil)
        if let password = presets.password {
            try handle.setPassword(password: password)
        }
        if let encoding = presets.encoding {
            handle.setEncoding(encoding: encoding)
        }
        let fm = FileManager.default
        let temp = fm.temporaryDirectory.appendingPathComponent("piesi-drag-\(UUID().uuidString)", isDirectory: true)
        try fm.createDirectory(at: temp, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: temp) }

        // 合成目录（id == max）没有真实条目，用 idsUnder 一次拿到它下面的全部 id
        let ids = row.id == UInt32.max ? handle.idsUnder(dir: row.path) : [row.id]
        let request = ExtractRequest(dest: temp.path, smartFolder: false, conflict: .overwrite, skipJunk: true, ids: ids, mergeIntoExisting: false)
        let listener = SilentProgress()
        _ = try handle.extract(request: request, listener: listener, cancel: CancelHandle())

        let produced = temp.appendingPathComponent(row.path)
        guard fm.fileExists(atPath: produced.path) else {
            throw PiesiError.Other(message: String(localized: "解出的文件不在预期位置：\(row.path)"))
        }
        // 目标已存在时直接替换是安全的：Finder 给的 url 要么是它刚起的不冲突的名字，要么是用户在冲突提示里选了「替换」
        if fm.fileExists(atPath: destination.path) {
            try fm.removeItem(at: destination)
        }
        try fm.moveItem(at: produced, to: destination)
        Log.browser.notice("drag export \(row.path, privacy: .public) → \(destination.path, privacy: .public)")
    }
}

/// 不显示进度的监听器（拖出由 Finder 自己转圈）。
final class SilentProgress: ProgressListener, @unchecked Sendable {
    func onProgress(info: ProgressInfo) {}
}
