// SettingsWindowController.swift —— 设置窗口（⌘,）：默认动作、完成后动作、自动退出、设为默认解压工具、Finder 右键菜单开关入口、界面语言入口、自动更新。存 UserDefaults。
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
        window.setContentSize(NSSize(width: 480, height: 800))
        window.title = String(localized: "设置")
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
    @AppStorage(SettingsKey.revealInFinder) private var revealInFinder = true
    @AppStorage(SettingsKey.moveToTrash) private var moveToTrash = false
    @AppStorage(SettingsKey.autoQuit) private var autoQuit = true
    @State private var settingDefault = false
    /// Finder 右键菜单扩展是否已在系统设置里打开；打开设置页、回到 app 时重读
    @State private var finderMenuEnabled = false
    /// 自动更新的两个开关，值存在 Sparkle 自己的键里，这里只做镜像
    @State private var autoCheckUpdates = AppDelegate.shared.updater.automaticallyChecks
    @State private var autoDownloadUpdates = AppDelegate.shared.updater.automaticallyDownloads

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
                // Finder Sync 扩展装上后要用户在系统设置里打开开关才生效，app 自己打不开它，只能带用户过去
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Finder 右键菜单")
                        Group {
                            if finderMenuEnabled {
                                Text("已开启。右键菜单里直接有「用 Piesi 解压到此处」等命令。")
                            } else {
                                Text("未开启。在系统设置里打开 Piesi 的开关后，右键菜单里直接有 Piesi 的命令。")
                            }
                        }
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("打开扩展设置…") {
                        FinderExtension.openSettings()
                    }
                }
                // 界面语言跟随系统，不在 app 里自己切：改 AppleLanguages 要重启才生效，
                // 而且 Quick Look 扩展不跟着变。这里只带用户去系统的「语言与地区」，在「应用程序」里单独给 Piesi 选语言。
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("界面语言")
                        Text("跟随系统。可在「语言与地区」的「应用程序」里单独为 Piesi 选择语言。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("打开语言设置…") {
                        SettingsView.openLanguageSettings()
                    }
                }
            }
            Section("更新") {
                Toggle("自动检查更新", isOn: $autoCheckUpdates)
                    .onChange(of: autoCheckUpdates) { _, value in
                        AppDelegate.shared.updater.automaticallyChecks = value
                    }
                Toggle("自动下载并在退出时安装", isOn: $autoDownloadUpdates)
                    .onChange(of: autoDownloadUpdates) { _, value in
                        AppDelegate.shared.updater.automaticallyDownloads = value
                    }
                    .disabled(!autoCheckUpdates)
                HStack {
                    Text("当前版本 \(AppInfo.version)")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("检查更新…") {
                        AppDelegate.shared.updater.checkForUpdates()
                    }
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 480, height: 800)
        .task {
            finderMenuEnabled = await FinderExtension.isEnabled()
        }
        // 用户去系统设置拨完开关切回来时刷新
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            Task { finderMenuEnabled = await FinderExtension.isEnabled() }
        }
    }

    /// 打开系统设置的「语言与地区」面板。
    static func openLanguageSettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.Localization-Settings.extension") else { return }
        NSWorkspace.shared.open(url)
    }
}
