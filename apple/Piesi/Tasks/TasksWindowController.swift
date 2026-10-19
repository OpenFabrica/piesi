// TasksWindowController.swift —— 进度浮窗：一个浮动小面板，每行一个任务；有任务时出现，全部结束后自动收起。
import AppKit
import SwiftUI

/// 浮窗控制器。NSPanel 不抢焦点、不算「用户窗口」，自动退出判断会忽略它。
@MainActor
final class TasksWindowController: NSWindowController {
    private let center: TaskCenter

    init(center: TaskCenter) {
        self.center = center
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 440, height: 120),
            styleMask: [.titled, .closable, .utilityWindow],
            backing: .buffered,
            defer: false
        )
        panel.title = "任务"
        panel.isFloatingPanel = true
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.contentViewController = NSHostingController(rootView: TasksView(center: center))
        super.init(window: panel)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("不用 nib")
    }

    /// 显示但不抢键盘焦点。
    func show() {
        guard let window else { return }
        if !window.isVisible {
            window.center()
        }
        window.orderFront(nil)
    }

    /// 任务列表变了：有任务就显示，空了就收起。
    func sync() {
        if center.tasks.isEmpty {
            window?.orderOut(nil)
        } else {
            show()
        }
    }
}

/// 浮窗内容：任务行列表。
struct TasksView: View {
    let center: TaskCenter

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if center.tasks.isEmpty {
                Text("没有进行中的任务")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 60)
            }
            ForEach(center.tasks) { task in
                TaskRowView(task: task)
            }
        }
        .padding(14)
        .frame(width: 440)
    }
}

/// 一行任务：图标、文件名、进度条、状态、取消。
struct TaskRowView: View {
    let task: TaskItem

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: task.kind == .extract ? "arrow.down.doc" : "doc.zipper")
                .font(.title2)
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 3) {
                HStack {
                    Text(task.title)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Text(task.statusText)
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
                if task.state == .running {
                    if let fraction = task.fraction {
                        ProgressView(value: fraction)
                    } else {
                        ProgressView()
                            .progressViewStyle(.linear)
                    }
                    Text(task.current ?? " ")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                } else if case .failed(let message) = task.state {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .lineLimit(2)
                }
            }
            Button {
                task.cancel()
            } label: {
                Image(systemName: "xmark.circle.fill")
            }
            .buttonStyle(.borderless)
            .disabled(task.state != .running)
            .help("取消")
        }
    }
}
