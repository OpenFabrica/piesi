// OpenQueue.swift —— 打开请求的串行队列：多文件按顺序一个个走完整流程，避免同时弹好几个密码框。
import Foundation

/// 一次「打开文件」请求。
struct OpenRequest: Sendable {
    let url: URL
    /// true = 浏览窗口，false = 直接解压
    let browse: Bool
}

/// 串行队列。start() 之前到的请求先攒着（Finder 的 open 事件比 didFinishLaunching 早）。
@MainActor
final class OpenQueue {
    private var pending: [OpenRequest] = []
    private var processing = false
    private var started = false
    private let handler: @MainActor (OpenRequest) async -> Void
    /// 队列清空时回调（自动退出判断用）
    var onIdle: (@MainActor () -> Void)?

    init(handler: @escaping @MainActor (OpenRequest) async -> Void) {
        self.handler = handler
    }

    var isIdle: Bool {
        pending.isEmpty && !processing
    }

    func enqueue(_ request: OpenRequest) {
        pending.append(request)
        if started {
            pump()
        }
    }

    /// 启动完成后才开始处理。
    func start() {
        started = true
        pump()
    }

    /// 取出一个处理完再取下一个；同一时间只有一条流程在走。
    private func pump() {
        guard !processing, !pending.isEmpty else { return }
        processing = true
        Task { @MainActor in
            while !pending.isEmpty {
                let request = pending.removeFirst()
                await handler(request)
            }
            processing = false
            onIdle?()
        }
    }
}
