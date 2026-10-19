// DragExport.swift —— 从浏览窗口拖条目到 Finder：NSFilePromiseProvider 在后台队列里开自己的句柄，解到临时目录再搬到 Finder 给的位置。
import AppKit
import UniformTypeIdentifiers

/// 文件承诺的委托。每次拖动一个实例；写入在 provider 的后台队列上同步完成。
final class EntryPromiseDelegate: NSObject, NSFilePromiseProviderDelegate, @unchecked Sendable {
    private let archiveURL: URL
    private let row: Row
    private let presets: ExtractPresets
    private let queue: OperationQueue = {
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

    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, fileNameForType fileType: String) -> String {
        row.name
    }

    func operationQueue(for filePromiseProvider: NSFilePromiseProvider) -> OperationQueue {
        queue
    }

    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, writePromiseTo url: URL, completionHandler: @escaping (Error?) -> Void) {
        do {
            try DragExport.write(archive: archiveURL, row: row, presets: presets, to: url)
            completionHandler(nil)
        } catch {
            Log.browser.error("drag export failed \(self.row.path, privacy: .public): \(CoreBridge.message(for: error), privacy: .public)")
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

        let ids = row.id == UInt32.max ? BrowserModel.collectIDs(handle, dir: row.path) : [row.id]
        let request = ExtractRequest(dest: temp.path, smartFolder: false, conflict: .overwrite, skipJunk: true, ids: ids)
        let listener = SilentProgress()
        _ = try handle.extract(request: request, listener: listener, cancel: CancelHandle())

        let produced = temp.appendingPathComponent(row.path)
        guard fm.fileExists(atPath: produced.path) else {
            throw PiesiError.Other(message: "解出的文件不在预期位置：\(row.path)")
        }
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
