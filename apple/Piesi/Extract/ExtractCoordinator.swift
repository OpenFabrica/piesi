// ExtractCoordinator.swift —— 解压流程（设计 6.3）：打开 → 密码 → 编码 → 计划 → 冲突 / 空间 / 炸弹 → 解压 → 收尾。
// 每个任务用自己的 openArchive 句柄；浏览窗口把它已确定的编码和密码作为 presets 传进来，避免再问一遍。
import AppKit

/// 浏览窗口已经确定的东西，解压时直接沿用。
struct ExtractPresets: Sendable {
    var encoding: Encoding?
    var password: String?
}

/// 解压流程协调器。
@MainActor
final class ExtractCoordinator {
    private let taskCenter: TaskCenter
    private let settings: AppSettings
    private let tasksWindow: TasksWindowController
    /// 编码置信度低于这个值就先弹选择条
    private let encodingConfidenceThreshold: Float = 0.8
    /// 展开比超过这个值且装不下，按压缩炸弹提示（设计 5.3）
    private let bombRatio: Double = 1000

    init(taskCenter: TaskCenter, settings: AppSettings, tasksWindow: TasksWindowController) {
        self.taskCenter = taskCenter
        self.settings = settings
        self.tasksWindow = tasksWindow
    }

    /// 整包解压到压缩包旁边（双击路径）。成功返回解出来的根目录，失败 / 取消返回 nil。
    /// 旁边写不进去（别的 app 的容器目录、只读卷）时改解到「下载」，并强制在 Finder 里选中结果，免得用户找不到。
    @discardableResult
    func extractAll(archive url: URL, parent: NSWindow?) async -> URL? {
        let folder = url.deletingLastPathComponent()
        guard !Self.canWrite(in: folder),
              let downloads = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
        else {
            return await run(archive: url, ids: [], dest: "", presets: ExtractPresets(), parent: parent)
        }
        Log.extract.notice("not writable \(folder.path, privacy: .public), fallback → \(downloads.path, privacy: .public)")
        return await run(archive: url, ids: [], dest: downloads.path, presets: ExtractPresets(), parent: parent, forceReveal: true)
    }

    /// 真建一个临时文件来试目录能不能写。
    /// 为什么不用 FileManager.isWritableFile：它走 access(2)，看不到 macOS 的「App 数据保护」（微信等 app 的
    /// ~/Library/Containers/…），也看不到沙盒 / TCC 拦截，会说「能写」，真写时才 EPERM。
    private static func canWrite(in folder: URL) -> Bool {
        let probe = folder.appendingPathComponent(".piesi-probe-\(UUID().uuidString)")
        let fd = open(probe.path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        guard fd >= 0 else { return false }
        close(fd)
        unlink(probe.path)
        return true
    }

    /// 浏览窗口触发：指定目标目录和条目（空 = 全部），沿用窗口已定的编码 / 密码。
    @discardableResult
    func extract(archive url: URL, ids: [UInt32], dest: URL, presets: ExtractPresets, parent: NSWindow?) async -> Bool {
        await run(archive: url, ids: ids, dest: dest.path, presets: presets, parent: parent) != nil
    }

    // MARK: 流程

    /// 成功返回实际落盘的根目录。forceReveal：不管设置，都在 Finder 里选中结果（目标不是压缩包旁边时用）。
    private func run(archive url: URL, ids: [UInt32], dest: String, presets: ExtractPresets, parent: NSWindow?, forceReveal: Bool = false) async -> URL? {
        let name = url.lastPathComponent

        // 1. 打开
        let handle: ArchiveHandle
        do {
            handle = try await CoreBridge.open(url)
        } catch {
            Log.extract.error("open failed \(url.path, privacy: .public): \(CoreBridge.message(for: error), privacy: .public)")
            await Alerts.error(String(localized: "无法打开「\(name)」"), CoreBridge.message(for: error), in: parent)
            return nil
        }

        // 2. 密码：浏览窗口传了就直接用；否则按需要弹框，错了重试，取消就退出
        var password = presets.password
        if let password {
            do {
                try await CoreBridge.setPassword(handle, password)
            } catch {
                await Alerts.error(String(localized: "无法打开「\(name)」"), CoreBridge.message(for: error), in: parent)
                return nil
            }
        } else if handle.passwordState() != .notNeeded {
            guard let entered = await askPassword(handle, name: name, parent: parent, wrongBefore: false) else { return nil }
            password = entered
        }

        // 3. 编码：不确定就先让用户看样本选
        if let encoding = presets.encoding {
            handle.setEncoding(encoding: encoding)
        } else {
            let info = handle.encodingInfo()
            if info.ambiguous || info.confidence < encodingConfidenceThreshold {
                Log.extract.notice("encoding uncertain \(name, privacy: .public): \(info.encoding.shortName, privacy: .public) \(info.confidence) ambiguous=\(info.ambiguous)")
                guard await EncodingDialog.run(handle: handle, archiveName: name, parent: parent) else { return nil }
            }
        }

        // 4. 计划（不落盘）：智能文件夹只在整包解压时有意义
        var request = ExtractRequest(dest: dest, smartFolder: ids.isEmpty, conflict: .rename, skipJunk: true, ids: ids, mergeIntoExisting: false)
        guard var plan = await makePlan(handle, request, name: name, parent: parent) else { return nil }

        // 4b. 顶层文件夹已存在：问另建「名字-2」还是解进已有的文件夹；选解进去就按合并重算计划（冲突在下一步再问）
        if let existing = plan.existingFolder, let newFolder = plan.newFolder {
            switch await ExistingFolderDialog.ask(existing: existing, newFolder: newFolder, parent: parent) {
            case .newFolder:
                break
            case .merge:
                request.mergeIntoExisting = true
                guard let merged = await makePlan(handle, request, name: name, parent: parent) else { return nil }
                plan = merged
            case nil:
                return nil
            }
        }
        for warning in plan.warnings {
            Log.extract.warning("plan \(name, privacy: .public): \(warning.localizedText, privacy: .public)")
        }

        // 5. 冲突：目标里已有同名项目，问策略（默认改名）
        if !plan.conflicts.isEmpty {
            guard let policy = await ConflictDialog.ask(conflicts: plan.conflicts, parent: parent) else { return nil }
            request.conflict = policy
        }

        // 6. 磁盘空间 / 压缩炸弹
        if let available = DiskSpace.available(forPath: plan.root), plan.totalBytes > available {
            let ratio = plan.archiveBytes > 0 ? Double(plan.totalBytes) / Double(plan.archiveBytes) : 0
            if ratio > bombRatio {
                await Alerts.error(
                    String(localized: "「\(name)」可能是压缩炸弹"),
                    String(localized: "包只有 \(ByteText.string(plan.archiveBytes))，展开却要 \(ByteText.string(plan.totalBytes))，超过磁盘剩余的 \(ByteText.string(available))。已停止。"),
                    in: parent
                )
            } else {
                await Alerts.error(
                    String(localized: "磁盘空间不足"),
                    String(localized: "解压「\(name)」需要 \(ByteText.string(plan.totalBytes))，目标卷只剩 \(ByteText.string(available))。"),
                    in: parent
                )
            }
            return nil
        }

        // 7. 解压。forData 的密码只有真正解数据时才验得出来，错了就重新要密码再来一次
        while true {
            let item = TaskItem(kind: .extract, title: name, totalBytes: plan.totalBytes)
            taskCenter.add(item)
            tasksWindow.show()
            let relay = ProgressRelay(item: item)
            do {
                let result = try await CoreBridge.extract(handle, request, listener: relay, cancel: item.cancelHandle)
                taskCenter.finish(item, state: .done)
                Log.extract.notice("done \(url.path, privacy: .public) → \(result.root, privacy: .public) extracted=\(result.extracted) skipped=\(result.skipped) junk=\(result.skippedJunk) warnings=\(result.warnings.count)")
                for warning in result.warnings {
                    Log.extract.warning("extract \(name, privacy: .public): \(warning.localizedText, privacy: .public)")
                }
                finish(result: result, archive: url, wholeArchive: ids.isEmpty, forceReveal: forceReveal)
                return URL(fileURLWithPath: result.root)
            } catch {
                if CoreBridge.isCancelled(error) {
                    taskCenter.finish(item, state: .cancelled)
                    Log.extract.notice("cancelled \(name, privacy: .public)")
                    return nil
                }
                if CoreBridge.isWrongPassword(error), presets.password == nil {
                    taskCenter.remove(item)
                    guard await askPassword(handle, name: name, parent: parent, wrongBefore: true) != nil else { return nil }
                    continue
                }
                let message = CoreBridge.message(for: error)
                taskCenter.finish(item, state: .failed(message))
                Log.extract.error("failed \(name, privacy: .public): \(message, privacy: .public)")
                await Alerts.error(String(localized: "解压「\(name)」失败"), message, in: parent)
                return nil
            }
        }
    }

    /// 算解压计划；失败时弹错误并返回 nil。
    private func makePlan(_ handle: ArchiveHandle, _ request: ExtractRequest, name: String, parent: NSWindow?) async -> ExtractPlanInfo? {
        do {
            return try await CoreBridge.plan(handle, request)
        } catch {
            await Alerts.error(String(localized: "无法解压「\(name)」"), CoreBridge.message(for: error), in: parent)
            return nil
        }
    }

    /// 反复要密码直到 setPassword 通过或用户取消。返回通过的密码。
    private func askPassword(_ handle: ArchiveHandle, name: String, parent: NSWindow?, wrongBefore: Bool) async -> String? {
        var wrong = wrongBefore
        while true {
            guard let password = await PasswordDialog.ask(archiveName: name, wrongBefore: wrong, weakEncryption: handle.hasWeakEncryption(), parent: parent) else { return nil }
            do {
                try await CoreBridge.setPassword(handle, password)
                return password
            } catch {
                if CoreBridge.isWrongPassword(error) {
                    wrong = true
                    continue
                }
                await Alerts.error(String(localized: "无法打开「\(name)」"), CoreBridge.message(for: error), in: parent)
                return nil
            }
        }
    }

    /// 收尾：按设置（或 forceReveal）在 Finder 里选中解出来的顶层项目（没有就退回根目录）、把压缩包移到废纸篓（只在整包解压时）。
    private func finish(result: ExtractResult, archive: URL, wholeArchive: Bool, forceReveal: Bool) {
        if settings.revealInFinder || forceReveal {
            let paths = result.topLevel.isEmpty ? [result.root] : result.topLevel
            NSWorkspace.shared.activateFileViewerSelecting(paths.map { URL(fileURLWithPath: $0) })
        }
        if settings.moveToTrash, wholeArchive {
            do {
                try FileManager.default.trashItem(at: archive, resultingItemURL: nil)
            } catch {
                Log.extract.warning("trash failed \(archive.path, privacy: .public): \(error.localizedDescription, privacy: .public)")
            }
        }
    }
}
