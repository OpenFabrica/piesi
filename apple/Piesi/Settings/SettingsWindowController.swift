// SettingsWindowController.swift —— 设置窗口（⌘,）：默认动作、完成后动作、自动退出、设为默认解压工具。存 UserDefaults。
import AppKit
import SwiftUI

/// 设置窗口控制器，窗口只建一次。
@MainActor
final class SettingsWindowController: NSWindowController {
    init() {
        let hosting = NSHostingController(rootView: SettingsView())
        // grouped 风格的 Form 没有固有高度，窗口尺寸由这里定，不让 SwiftUI 反推
        hosting.sizingOptions = []
        let window = NSWindow(contentViewController: hosting)
        window.setContentSize(NSSize(width: 480, height: 470))
        window.title = "设置"
        window.styleMask = [.titled, .closable]
        window.isReleasedWhenClosed = false
        window.setFrameAutosaveName("SettingsWindow")
        super.init(window: window)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("不用 nib")
    }

    func show() {
        guard let window else { return }
        if !window.isVisible {
            window.center()
        }
        showWindow(nil)
        NSApp.activate()
    }
}

/// 设置页。@AppStorage 直接读写 AppSettings 注册过默认值的那些键。
struct SettingsView: View {
    @AppStorage(SettingsKey.defaultAction) private var defaultAction = DefaultAction.extract.rawValue
    @AppStorage(SettingsKey.revealInFinder) private var revealInFinder = false
    @AppStorage(SettingsKey.moveToTrash) private var moveToTrash = false
    @AppStorage(SettingsKey.autoQuit) private var autoQuit = true
    @State private var settingDefault = false

    var body: some View {
        Form {
            Section("双击压缩包") {
                Picker("默认动作", selection: $defaultAction) {
                    Text("直接解压到压缩包旁边").tag(DefaultAction.extract.rawValue)
                    Text("先浏览内容").tag(DefaultAction.browse.rawValue)
                }
                .pickerStyle(.radioGroup)
                .labelsHidden()
                Text("按住 ⌥ 双击总是先浏览。")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("完成后") {
                Toggle("在 Finder 中显示结果", isOn: $revealInFinder)
                Toggle("解压完成后把压缩包移到废纸篓", isOn: $moveToTrash)
                Toggle("因打开文件而启动时，任务全部完成后自动退出", isOn: $autoQuit)
            }
            Section("系统") {
                HStack {
                    Text("让 zip、7z、rar、tar 等压缩包默认用 Piesi 打开")
                    Spacer()
                    Button("设为默认解压工具") {
                        settingDefault = true
                        Task { @MainActor in
                            await DefaultHandler.setAsDefault(reportTo: NSApp.keyWindow)
                            settingDefault = false
                        }
                    }
                    .disabled(settingDefault)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 480, height: 470)
    }
}
