// AppState.swift —— 主窗口共享状态（macOS 14 的 @Observable）：记录每次打开/拖入的文件和识别结果。
import Foundation
import Observation
import OSLog

/// 一次「打开文件」请求：来源、路径、是否要求浏览（⌥）、Rust 识别出的格式名。
struct OpenRequest: Identifiable {
    /// 请求来源：Finder 打开事件，或拖进窗口
    enum Source: String {
        case finder = "Finder"
        case drop = "拖入"
    }

    /// 识别进度：等待 Rust 返回 / 已返回（nil 表示认不出）
    enum Detection: Equatable {
        case pending
        case done(String?)
    }

    let id = UUID()
    let url: URL
    let source: Source
    let browse: Bool
    var detection: Detection = .pending
}

/// 主窗口要显示的全部状态；只在主线程读写，识别本身丢到后台再回来更新。
@MainActor
@Observable
final class AppState {
    /// 按时间顺序记录的请求，最新的在最后
    private(set) var openRequests: [OpenRequest] = []
    /// 识别结果日志，和 AppDelegate 同一个 subsystem
    @ObservationIgnored private let logger = Logger(subsystem: "com.citrolabs.piesi", category: "detect")

    /// 记录一次请求，并异步调 Rust 识别格式；结果回到主线程写回对应条目。
    func record(_ url: URL, source: OpenRequest.Source, browse: Bool) {
        let request = OpenRequest(url: url, source: source, browse: browse)
        openRequests.append(request)
        let id = request.id
        Task {
            let name = await CoreBridge.detectFormat(at: url)
            self.logger.notice("detect \(url.path, privacy: .public) → \(name ?? "未识别", privacy: .public)")
            self.update(id: id, detection: .done(name))
        }
    }

    /// 按 id 回填识别结果；条目可能已被清掉，找不到就忽略。
    private func update(id: UUID, detection: OpenRequest.Detection) {
        guard let index = openRequests.firstIndex(where: { $0.id == id }) else { return }
        openRequests[index].detection = detection
    }
}
