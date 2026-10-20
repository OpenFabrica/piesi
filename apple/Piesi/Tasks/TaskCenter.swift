// TaskCenter.swift —— 进行中 / 刚结束的任务列表，进度浮窗的数据源；ProgressRelay 把 Rust 回调转到主线程。
import Foundation
import Observation

/// 一个解压或压缩任务的可观察状态。
@MainActor
@Observable
final class TaskItem: Identifiable {
    enum Kind {
        case extract
        case create
        case verify
    }

    enum State: Equatable {
        case running
        case done
        case failed(String)
        case cancelled
    }

    let id = UUID()
    let kind: Kind
    /// 压缩包文件名
    let title: String
    /// UI 持有，点取消时调 cancel()
    @ObservationIgnored let cancelHandle = CancelHandle()
    let startedAt = Date()
    var state: State = .running
    var doneBytes: UInt64 = 0
    var totalBytes: UInt64?
    var doneEntries: UInt64 = 0
    var totalEntries: UInt64?
    /// 正在处理的条目名
    var current: String?
    /// 指数平滑后的速度（字节/秒）
    var bytesPerSecond: Double = 0
    @ObservationIgnored private var lastSampleTime = Date()
    @ObservationIgnored private var lastSampleBytes: UInt64 = 0

    init(kind: Kind, title: String, totalBytes: UInt64?) {
        self.kind = kind
        self.title = title
        self.totalBytes = totalBytes
    }

    /// 0…1 的完成比例；总量未知（tar 流式）时用条目数，再不行返回 nil 让进度条转圈。
    var fraction: Double? {
        if let total = totalBytes, total > 0 {
            return min(1, Double(doneBytes) / Double(total))
        }
        if let total = totalEntries, total > 0 {
            return min(1, Double(doneEntries) / Double(total))
        }
        return nil
    }

    var isFinished: Bool {
        state != .running
    }

    /// 浮窗里的图标
    var iconName: String {
        switch kind {
        case .extract: "arrow.down.doc"
        case .create: "doc.zipper"
        case .verify: "checkmark.shield"
        }
    }

    /// 收到 Rust 的进度快照（已节流到 ≥50 ms）。速度做指数平滑，否则数字每次都跳。
    func apply(_ info: ProgressInfo) {
        let now = Date()
        let dt = now.timeIntervalSince(lastSampleTime)
        if dt > 0.2, info.doneBytes >= lastSampleBytes {
            let instant = Double(info.doneBytes - lastSampleBytes) / dt
            bytesPerSecond = bytesPerSecond == 0 ? instant : bytesPerSecond * 0.7 + instant * 0.3
            lastSampleTime = now
            lastSampleBytes = info.doneBytes
        }
        doneBytes = info.doneBytes
        if let total = info.totalBytes {
            totalBytes = total
        }
        doneEntries = info.doneEntries
        totalEntries = info.totalEntries ?? totalEntries
        current = info.current
    }

    func cancel() {
        cancelHandle.cancel()
    }

    /// 右侧状态文字：百分比 + 速度，或结束状态。
    var statusText: String {
        switch state {
        case .running:
            var parts: [String] = []
            if let fraction {
                parts.append("\(Int(fraction * 100))%")
            }
            if bytesPerSecond > 0 {
                parts.append("\(ByteText.string(bytesPerSecond))/s")
            }
            return parts.isEmpty ? String(localized: "准备中…") : parts.joined(separator: " · ")
        case .done: return String(localized: "完成")
        case .failed: return String(localized: "失败")
        case .cancelled: return String(localized: "已取消")
        }
    }
}

/// 把 Rust 的进度回调（任意线程）转到主线程更新 TaskItem。
final class ProgressRelay: ProgressListener, @unchecked Sendable {
    private let item: TaskItem

    init(item: TaskItem) {
        self.item = item
    }

    func onProgress(info: ProgressInfo) {
        Task { @MainActor in
            self.item.apply(info)
        }
    }
}

/// 任务列表。
@MainActor
@Observable
final class TaskCenter {
    private(set) var tasks: [TaskItem] = []
    /// 增删或状态变化时回调（浮窗显示/隐藏、自动退出判断）
    @ObservationIgnored var onChange: (@MainActor () -> Void)?

    var hasRunning: Bool {
        tasks.contains { $0.state == .running }
    }

    var runningCount: Int {
        tasks.filter { $0.state == .running }.count
    }

    func add(_ item: TaskItem) {
        tasks.append(item)
        onChange?()
    }

    /// 标记结束；行留在浮窗里一会儿再移除，让用户看得到「完成」。
    func finish(_ item: TaskItem, state: TaskItem.State) {
        item.state = state
        onChange?()
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(1.2))
            self.remove(item)
        }
    }

    func remove(_ item: TaskItem) {
        tasks.removeAll { $0.id == item.id }
        onChange?()
    }

    func cancelAll() {
        for task in tasks where task.state == .running {
            task.cancel()
        }
    }
}
