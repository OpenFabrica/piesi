// MainWindowController.swift —— 无参数启动时的主窗口：拖放区 + 打开压缩包 + 新建压缩包。
// 拖入压缩包按设置解压或浏览（按住 ⌥ 一律浏览），拖入普通文件进压缩对话框。
import AppKit
import SwiftUI

/// 主窗口控制器，窗口只建一次，关掉后可再显示。
@MainActor
final class MainWindowController: NSWindowController {
    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 360),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Piesi"
        window.isReleasedWhenClosed = false
        window.contentViewController = NSHostingController(rootView: MainView())
        window.setFrameAutosaveName("MainWindow")
        window.center()
        super.init(window: window)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("不用 nib")
    }
}

/// 主窗口内容。
struct MainView: View {
    @State private var isDropTargeted = false

    var body: some View {
        VStack(spacing: 18) {
            Text(AppInfo.versionLine)
                .font(.title3)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            dropZone
            HStack(spacing: 12) {
                Button("打开压缩包…") {
                    AppDelegate.shared.openDocument(nil)
                }
                Button("新建压缩包…") {
                    AppDelegate.shared.newArchive(nil)
                }
            }
        }
        .padding(24)
        .frame(minWidth: 480, minHeight: 320)
    }

    /// 虚线框拖放区。
    private var dropZone: some View {
        RoundedRectangle(cornerRadius: 12)
            .strokeBorder(style: StrokeStyle(lineWidth: 2, dash: [8]))
            .foregroundStyle(isDropTargeted ? Color.accentColor : Color.secondary)
            .frame(minHeight: 150)
            .overlay {
                VStack(spacing: 6) {
                    Image(systemName: "doc.zipper")
                        .font(.system(size: 34))
                    Text("把压缩包或文件拖到这里")
                    Text("压缩包按设置直接解压或先浏览（按住 ⌥ 一律浏览）；普通文件会新建压缩包")
                        .font(.caption)
                }
                .foregroundStyle(.secondary)
            }
            .dropDestination(for: URL.self) { urls, _ in
                let option = NSEvent.modifierFlags.contains(.option)
                AppDelegate.shared.open(urls: urls, browse: option ? true : nil)
                return !urls.isEmpty
            } isTargeted: { targeted in
                isDropTargeted = targeted
            }
    }
}
