// AppDelegate.swift —— AppKit 委托：启动流程、Finder 打开事件与右键菜单请求、主窗口、自动退出。
import AppKit
import UniformTypeIdentifiers

/// 应用委托。Finder 的 open 事件在 didFinishLaunching 之前就会到，所以先进队列，启动完成后再按设置处理。
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// 全局入口（NSApp.delegate 是弱引用，这里是强引用）
    static var shared: AppDelegate!

    let settings = AppSettings()
    let taskCenter = TaskCenter()
    private(set) lazy var tasksWindow = TasksWindowController(center: taskCenter)
    private(set) lazy var extractor = ExtractCoordinator(taskCenter: taskCenter, settings: settings, tasksWindow: tasksWindow)
    private(set) lazy var compressor = CompressCoordinator(taskCenter: taskCenter, settings: settings, tasksWindow: tasksWindow)
    private lazy var settingsWindow = SettingsWindowController()
    private lazy var openQueue = OpenQueue { [weak self] request in
        await self?.handle(request)
    }
    /// 是否因为打开文件而启动：这种情况下不弹主窗口，做完自动退出（设计 6.3）
    private var launchedForFiles = false
    private var finishedLaunching = false
    /// 无参数启动时延后显示主窗口的任务（见 showMainWindowAfterLaunch）
    private var pendingMainWindow: Task<Void, Never>?
    /// 正在跑的 App Intents 数量：perform() 没返回前不能退出，否则快捷指令收不到结果
    private var headlessWorkCount = 0
    private var mainWindowController: MainWindowController?
    /// Finder 右键「服务」的处理器；NSApp.servicesProvider 是强引用，这里留一份便于测试
    private let servicesProvider = ServicesProvider()

    // MARK: 生命周期

    /// 服务处理器要在收到服务请求前装好，放在 willFinish。
    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.servicesProvider = servicesProvider
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        finishedLaunching = true
        cleanStalePreviewDirectories()
        taskCenter.onChange = { [weak self] in
            guard let self else { return }
            tasksWindow.sync()
            maybeAutoQuit()
        }
        openQueue.onIdle = { [weak self] in self?.maybeAutoQuit() }
        // 窗口关闭通知到达时窗口还没真正消失，下一轮 runloop 再判断是否该退出
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: nil, queue: .main) { _ in
            Task { @MainActor in AppDelegate.shared?.maybeAutoQuit() }
        }
        // 测试入口 -PiesiCompress 等同于「因文件启动」：不开主窗口，做完按自动退出规则退出
        if settings.hasCompressTestFlag {
            launchedForFiles = true
        } else if launchedForFiles {
            NSApp.activate()
        } else {
            showMainWindowAfterLaunch()
        }
        if settings.showSettingsAtLaunch {
            settingsWindow.show()
        }
        // 首次启动只问「设为默认解压工具」这一件事（设计 6.3），问完再处理排队的文件
        Task { @MainActor in
            await DefaultHandler.askOnFirstLaunch(settings: settings)
            openQueue.start()
            if settings.hasCompressTestFlag {
                await runCompressTestEntry()
            }
        }
    }

    /// 无参数启动时的主窗口延后 300 ms 再显示：快捷指令（App Intents）在后台拉起 app 时，perform() 要等启动完成才到，
    /// 没有这一拍延迟主窗口会先闪出来再被撤掉。普通点 Dock 启动多等 300 ms 感觉不到。
    private func showMainWindowAfterLaunch() {
        pendingMainWindow = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled, let self else { return }
            showMainWindow()
        }
    }

    /// App Intents 开始干活：撤掉还没显示的主窗口，并按「因文件启动」的规则在做完后自动退出（有别的窗口开着就不退）。
    func beginHeadlessWork() {
        pendingMainWindow?.cancel()
        pendingMainWindow = nil
        launchedForFiles = true
        headlessWorkCount += 1
    }

    /// 意图做完：默认等 2 秒让结果经 XPC 交回快捷指令，再按自动退出规则判断（测试入口传 .zero 立刻判断）。
    func endHeadlessWork(quitAfter delay: Duration = .seconds(2)) {
        headlessWorkCount -= 1
        Task { @MainActor [weak self] in
            if delay > .zero {
                try? await Task.sleep(for: delay)
            }
            self?.maybeAutoQuit()
        }
    }

    /// 隐藏启动参数 -PiesiCompress（测试入口，和 -PiesiBrowse 一样在 AppSettings 解析）：
    /// 不弹对话框，走和对话框相同的 CompressCoordinator.run()，结果看统一日志，然后按自动退出规则退出。
    private func runCompressTestEntry() async {
        // 和意图一样计数：不然 taskCenter.finish 一触发 maybeAutoQuit，进程会在 run() 返回前退出，收尾（日志、Finder 显示）做不完
        beginHeadlessWork()
        defer { endHeadlessWork(quitAfter: .zero) }
        if let request = settings.compressTestRequest {
            let options = request.preset.options(destination: request.destination, sources: request.sources, password: request.password)
            await compressor.runHeadless(options)
        } else {
            Log.app.error("compress test entry: 用法 -PiesiCompress <windows|smallest|encrypted|mac> <输出路径> <源路径…> [-PiesiPassword <密码>]")
        }
    }

    /// Finder 双击 / 拖到 Dock / open 命令。⌥ 双击 = 浏览（设计 6.3）；open 事件不带按键，读当前硬件修饰键。
    /// piesi:// 链接来自 Finder 右键菜单扩展，单独处理。
    func application(_ application: NSApplication, open urls: [URL]) {
        // 无效链接（伪造、过期）不算「因文件启动」：否则 app 会不开窗口、也没事可做，停在后台不退出
        let accepted = urls.filter { !$0.isFileURL }.filter(openFinderRequest)
        let urls = urls.filter(\.isFileURL)
        if !accepted.isEmpty || !urls.isEmpty {
            markLaunchedForFilesIfStarting()
        }
        guard !urls.isEmpty else { return }
        let option = NSEvent.modifierFlags.contains(.option)
        let browse = option || settings.forceBrowse || settings.defaultAction == .browse
        for url in urls {
            Log.open.notice("open \(url.path, privacy: .public) option=\(option) browse=\(browse)")
        }
        openQueue.enqueue(OpenRequest(urls: urls, browse: browse))
    }

    /// 打开请求到达时，判断这次启动算不算「因文件启动」（不弹主窗口、做完自动退出）。
    /// 文件的 open 事件一定早于 didFinishLaunching；piesi:// 链接的事件实测可能晚到一点，
    /// 这时主窗口还在 300 ms 延迟里没显示，同样撤掉它，按因文件启动处理（和 beginHeadlessWork 一个道理）。
    private func markLaunchedForFilesIfStarting() {
        if !finishedLaunching {
            launchedForFiles = true
        } else if mainWindowController == nil, let pending = pendingMainWindow, !pending.isCancelled {
            pending.cancel()
            pendingMainWindow = nil
            launchedForFiles = true
            // 和 didFinishLaunching 里「因文件启动」的分支一致：没有主窗口也要到前台，后面的对话框才不会藏在别的窗口后面
            NSApp.activate()
        }
    }

    /// Finder 右键菜单扩展发来的 piesi://finder?id=… ：按 id 取出请求，和双击一样排进打开队列（不看 ⌥ 和默认动作设置）。
    /// 返回是否收下了请求。
    private func openFinderRequest(_ link: URL) -> Bool {
        guard let request = FinderHandoff.take(link) else {
            Log.open.error("ignored link \(link.absoluteString, privacy: .public)")
            return false
        }
        let urls = request.paths.map { URL(fileURLWithPath: $0) }
        Log.open.notice("finder \(request.action.rawValue, privacy: .public): \(urls.count) files")
        switch request.action {
        case .extract:
            openQueue.enqueue(OpenRequest(urls: urls, browse: false))
        case .browse:
            openQueue.enqueue(OpenRequest(urls: urls, browse: true))
        case .compress:
            openQueue.enqueue(OpenRequest(urls: urls, browse: false, compressAll: true))
        }
        return true
    }

    /// 点 Dock 图标而没有窗口时，重新显示主窗口。
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag {
            showMainWindow()
        }
        return false
    }

    /// 退出时清掉 Quick Look 预览用的临时目录（Rust 写在 temp/piesi-preview-<pid>/）。
    func applicationWillTerminate(_ notification: Notification) {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("piesi-preview-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)
        try? FileManager.default.removeItem(at: dir)
    }

    /// 启动时清掉上次崩溃 / 被杀留下的 piesi-preview-<pid> 目录：只删进程已经不在的，后台做，不挡启动。
    private func cleanStalePreviewDirectories() {
        let temp = FileManager.default.temporaryDirectory
        Task.detached(priority: .utility) {
            let fm = FileManager.default
            guard let names = try? fm.contentsOfDirectory(atPath: temp.path) else { return }
            for name in names where name.hasPrefix("piesi-preview-") {
                guard let pid = pid_t(name.dropFirst("piesi-preview-".count)) else { continue }
                // kill(pid, 0) 不发信号只探测；ESRCH = 没有这个进程（EPERM 说明进程在但不是我们的，不能删）
                guard kill(pid, 0) != 0, errno == ESRCH else { continue }
                try? fm.removeItem(at: temp.appendingPathComponent(name, isDirectory: true))
                Log.app.notice("removed stale preview dir \(name, privacy: .public)")
            }
        }
    }

    /// 自动退出由 maybeAutoQuit 决定，不用系统的「最后一个窗口关闭就退出」。
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    /// 还有任务在跑时退出要确认，避免写一半的文件。
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard taskCenter.hasRunning else { return .terminateNow }
        let alert = NSAlert()
        alert.messageText = String(localized: "有 \(taskCenter.runningCount) 个任务正在进行")
        alert.informativeText = String(localized: "现在退出会中断这些任务，已写出的文件可能不完整。")
        alert.addButton(withTitle: String(localized: "取消"))
        alert.addButton(withTitle: String(localized: "中断并退出"))
        alert.buttons[1].hasDestructiveAction = true
        if alert.runModal() == .alertSecondButtonReturn {
            taskCenter.cancelAll()
            return .terminateNow
        }
        return .terminateCancel
    }

    // MARK: 打开入口

    /// 主窗口拖入、⌘O、Services 都走这里；browse 为 nil 时按设置决定。压缩包和普通文件混在一起也行，处理时再分。
    func open(urls: [URL], browse: Bool?) {
        let shouldBrowse = browse ?? (settings.defaultAction == .browse)
        for url in urls {
            Log.open.notice("open(in-app) \(url.path, privacy: .public) browse=\(shouldBrowse)")
        }
        openQueue.enqueue(OpenRequest(urls: urls, browse: shouldBrowse))
        openQueue.start()
    }

    /// 「用 Piesi 压缩」服务、主窗口「新建压缩包…」：直接进压缩对话框，不经过格式识别。
    func compress(sources: [URL]) {
        guard !sources.isEmpty else { return }
        Task { @MainActor in
            await compressor.start(sources: sources, parent: nil)
            maybeAutoQuit()
        }
    }

    /// ⌘N：选文件 / 文件夹后进压缩对话框。
    @objc func newArchive(_ sender: Any?) {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = true
        panel.canChooseFiles = true
        panel.message = String(localized: "选择要压缩的文件或文件夹")
        panel.prompt = String(localized: "压缩")
        Task { @MainActor in
            guard await panel.begin() == .OK else { return }
            compress(sources: panel.urls)
        }
    }

    /// ⌘O：选压缩包后进浏览窗口（设计 6.3：app 内打开 = 浏览）。
    @objc func openDocument(_ sender: Any?) {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.allowedContentTypes = ArchiveTypes.all
        panel.message = String(localized: "选择要浏览的压缩包")
        Task { @MainActor in
            guard await panel.begin() == .OK else { return }
            open(urls: panel.urls, browse: true)
        }
    }

    /// 窗口菜单：显示任务浮窗。
    @objc func showTasksWindow(_ sender: Any?) {
        tasksWindow.show()
    }

    /// 「关于 Piesi」：系统标准关于面板，版本一行显示 app 版本（带开发后缀）和 Rust 核心版本，
    /// 如「版本 0.1.0-dev (core 0.1.0-dev)」；下面放 GitHub 仓库、作者 X 账号，请用户点个 Star。主窗口不再放版本号。
    @objc func showAbout(_ sender: Any?) {
        NSApp.orderFrontStandardAboutPanel(options: [
            .applicationVersion: AppInfo.version,
            .version: "core \(CoreBridge.version)",
            .credits: aboutCredits(),
        ])
        NSApp.activate()
    }

    /// 关于面板的附加文字：一句求 Star，加两条可点的链接（GitHub 仓库、X 账号），居中小字。
    private func aboutCredits() -> NSAttributedString {
        let center = NSMutableParagraphStyle()
        center.alignment = .center
        center.paragraphSpacing = 4
        let base: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
            .foregroundColor: NSColor.secondaryLabelColor,
            .paragraphStyle: center,
        ]
        let text = NSMutableAttributedString(string: String(localized: "喜欢 Piesi？去 GitHub 给它点个 Star ⭐") + "\n", attributes: base)
        var link = base
        link[.link] = AppInfo.repoURL
        text.append(NSAttributedString(string: "GitHub: OpenFabrica/piesi", attributes: link))
        text.append(NSAttributedString(string: "\n", attributes: base))
        link[.link] = AppInfo.xURL
        text.append(NSAttributedString(string: "X: @daveylan2", attributes: link))
        return text
    }

    /// ⌘,：设置窗口。
    @objc func showSettingsWindow(_ sender: Any?) {
        settingsWindow.show()
    }

    // MARK: 处理队列

    /// 一批文件的完整处理：按文件头分成压缩包和普通文件。压缩包逐个浏览或解压（解压做完才轮到下一个）；
    /// 扩展名是压缩包但文件头认不出的（损坏、没下完）报「无法打开」，不能当普通文件去压缩；
    /// 剩下的普通文件（拖到 Dock 图标 / 主窗口）合成一个压缩请求。compressAll 时不分类，全部进压缩对话框。
    private func handle(_ request: OpenRequest) async {
        if request.compressAll {
            await compressor.start(sources: request.urls, parent: nil)
            return
        }
        var archives: [URL] = []
        var others: [URL] = []
        for url in request.urls {
            if await CoreBridge.detectFormat(at: url) != nil {
                archives.append(url)
            } else if ArchiveTypes.looksLikeArchive(url) {
                Log.open.error("unreadable archive \(url.path, privacy: .public)")
                await Alerts.error(String(localized: "无法打开「\(url.lastPathComponent)」"), String(localized: "扩展名是压缩包，但内容无法识别，文件可能已损坏或没有下载完整。"), in: nil)
            } else {
                others.append(url)
            }
        }
        for url in archives {
            if request.browse {
                BrowserWindowController.open(url: url)
            } else {
                await extractor.extractAll(archive: url, parent: nil)
            }
        }
        if !others.isEmpty {
            await compressor.start(sources: others, parent: nil)
        }
    }

    // MARK: 窗口与退出

    /// 显示主窗口（无参数启动、点 Dock 图标）。
    func showMainWindow() {
        if mainWindowController == nil {
            mainWindowController = MainWindowController()
        }
        mainWindowController?.showWindow(nil)
        NSApp.activate()
    }

    /// 因打开文件而启动、队列空、没有任务在跑、没有别的窗口 → 退出（设置项可关）。
    /// 任务浮窗和各种对话框都是 NSPanel，不算「别的窗口」。
    func maybeAutoQuit() {
        guard launchedForFiles, finishedLaunching, settings.autoQuit, headlessWorkCount == 0 else { return }
        guard openQueue.isIdle, !taskCenter.hasRunning else { return }
        let userWindows = NSApp.windows.filter { $0.isVisible && !($0 is NSPanel) }
        guard userWindows.isEmpty else { return }
        Log.app.notice("tasks finished and no window left, quitting")
        NSApp.terminate(nil)
    }
}

/// Info.plist 里声明的压缩包类型（CFBundleDocumentTypes），打开面板和「设为默认」共用，只存一处。
enum ArchiveTypes {
    /// 所有 LSItemContentTypes 的 UTI 字符串（含自定义导入的 7z / rar / zst）
    static var identifiers: [String] {
        let types = Bundle.main.infoDictionary?["CFBundleDocumentTypes"] as? [[String: Any]] ?? []
        return types.flatMap { $0["LSItemContentTypes"] as? [String] ?? [] }
    }

    /// 同上，转成 UTType（系统不认识的 UTI 会被丢掉）
    static var all: [UTType] {
        identifiers.compactMap { UTType($0) }
    }

    /// 按扩展名 / 系统标注的类型判断「看起来是压缩包」：给文件头识别失败时区分「损坏的包」和「普通文件」用。
    static func looksLikeArchive(_ url: URL) -> Bool {
        var type = UTType(filenameExtension: url.pathExtension)
        if type == nil {
            type = try? url.resourceValues(forKeys: [.contentTypeKey]).contentType
        }
        guard let type else { return false }
        return all.contains { type.conforms(to: $0) }
    }
}
