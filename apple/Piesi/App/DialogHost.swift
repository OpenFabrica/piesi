// DialogHost.swift —— 把 SwiftUI 视图放进窗口当对话框：有父窗口作 sheet，没有就居中浮窗；用 continuation 等结果，不嵌套 runloop。
import AppKit
import SwiftUI

/// 持有对话框窗口和「已经回答过」标记，防止 continuation 被 resume 两次。
@MainActor
private final class DialogBox {
    var window: NSWindow?
    var finished = false
}

/// 对话框宿主。
@MainActor
enum DialogHost {
    /// 显示 content 构造的视图，视图通过传入的 finish 闭包交回结果。
    static func present<T: Sendable, V: View>(
        title: String,
        parent: NSWindow?,
        @ViewBuilder content: (@escaping @MainActor (T) -> Void) -> V
    ) async -> T {
        await withCheckedContinuation { (continuation: CheckedContinuation<T, Never>) in
            let box = DialogBox()
            let finish: @MainActor (T) -> Void = { result in
                guard !box.finished else { return }
                box.finished = true
                if let parent, parent.isVisible, let window = box.window {
                    parent.endSheet(window)
                } else {
                    box.window?.close()
                }
                continuation.resume(returning: result)
            }
            let hosting = NSHostingController(rootView: content(finish))
            let window = NSWindow(contentViewController: hosting)
            window.title = title
            // 只留标题栏：关闭按钮会绕过 finish，结果就丢了；取消一律走视图里的按钮
            window.styleMask = [.titled]
            window.isReleasedWhenClosed = false
            box.window = window
            if let parent, parent.isVisible {
                parent.beginSheet(window)
            } else {
                window.center()
                window.level = .floating
                NSApp.activate()
                window.makeKeyAndOrderFront(nil)
            }
        }
    }
}
