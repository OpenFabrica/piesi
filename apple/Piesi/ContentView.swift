// ContentView.swift —— 主窗口：版本行 + 拖放识别区 + 打开记录。P0 只验证 Swift → Rust 链路，不是正式界面。
import SwiftUI

/// 主窗口内容。
struct ContentView: View {
    @Environment(AppState.self) private var state
    /// 拖放悬停中，用来高亮边框
    @State private var isDropTargeted = false

    var body: some View {
        VStack(spacing: 16) {
            Text(AppInfo.versionLine)
                .font(.title2)
                .textSelection(.enabled)
            dropZone
            requestList
        }
        .padding(24)
        .frame(minWidth: 520, minHeight: 400)
    }

    /// 虚线框拖放区：拖进文件后交给 AppState 识别格式。
    private var dropZone: some View {
        RoundedRectangle(cornerRadius: 12)
            .strokeBorder(style: StrokeStyle(lineWidth: 2, dash: [8]))
            .foregroundStyle(isDropTargeted ? Color.accentColor : Color.secondary)
            .frame(height: 140)
            .overlay {
                Text("把压缩包拖到这里识别格式")
                    .foregroundStyle(.secondary)
            }
            .dropDestination(for: URL.self) { urls, _ in
                for url in urls {
                    state.record(url, source: .drop, browse: false)
                }
                return !urls.isEmpty
            } isTargeted: { targeted in
                isDropTargeted = targeted
            }
    }

    /// 已打开/拖入的文件列表，每行显示来源、路径、识别结果。
    private var requestList: some View {
        List(state.openRequests) { request in
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(request.url.lastPathComponent)
                    Text(request.url.path)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
                if request.browse {
                    Text("⌥ 浏览")
                        .font(.caption)
                        .padding(.horizontal, 6)
                        .background(.quaternary, in: Capsule())
                }
                Text(request.source.rawValue)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(detectionText(request.detection))
                    .monospaced()
            }
        }
        .overlay {
            if state.openRequests.isEmpty {
                Text("还没有打开过文件")
                    .foregroundStyle(.tertiary)
            }
        }
    }

    /// 识别状态转成一列文字。
    private func detectionText(_ detection: OpenRequest.Detection) -> String {
        switch detection {
        case .pending: "识别中…"
        case .done(let name?): name
        case .done(nil): "未识别"
        }
    }
}
