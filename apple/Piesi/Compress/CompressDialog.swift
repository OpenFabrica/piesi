// CompressDialog.swift —— 格式、文件名与位置优先；预设、密码及兼容选项收进可展开的高级区域。
import AppKit
import SwiftUI

/// 将不依赖引擎的格式标识转换为 UniFFI 格式。
extension CompressionFormat {
    var coreFormat: CreateFormat {
        switch self {
        case .zip: .zip
        case .sevenZ: .sevenZ
        case .tar: .tar
        case .tarGz: .tarGz
        case .tarBz2: .tarBz2
        case .tarXz: .tarXz
        case .tarZst: .tarZst
        case .gz: .gz
        case .bz2: .bz2
        case .xz: .xz
        case .zst: .zst
        }
    }
}

/// 创建能力与 Rust CreateFormat 保持一致，避免提交不支持的选项。
extension CreateFormat {
    var isSingleFile: Bool { [.gz, .bz2, .xz, .zst].contains(self) }
}

/// 可选的特殊用途预设，保留快捷指令兼容。
enum CompressPreset: String, CaseIterable, Identifiable, Sendable {
    case windows
    case smallest
    case encrypted
    case mac

    var id: Self { self }

    var title: String {
        switch self {
        case .windows: String(localized: "发给 Windows")
        case .smallest: String(localized: "最小体积")
        case .encrypted: String(localized: "加密")
        case .mac: String(localized: "Mac 专用")
        }
    }

    var format: CreateFormat {
        switch self {
        case .windows, .mac: .zip
        case .smallest, .encrypted: .sevenZ
        }
    }

    var level: Double {
        self == .smallest ? 7 : 5
    }

    var windowsSafe: Bool {
        self == .windows
    }

    var encryptNames: Bool {
        self == .encrypted
    }

    /// 只有「Mac 专用」写 __MACOSX/._ 元数据；发给 Windows 的包里它们只是垃圾
    var macMetadata: Bool {
        self == .mac
    }

    /// 该预设保留默认高级选项时的参数（skipJunk 默认开）；-PiesiCompress 入口和 App Intents 共用。
    /// 文件名加密和 submit() 一样只在 7z + 有密码时为真。
    func options(destination: URL, sources: [URL], password: String?) -> CompressOptions {
        CompressOptions(
            destination: destination,
            sources: sources,
            format: format,
            level: UInt8(level),
            password: password,
            encryptNames: encryptNames && format == .sevenZ && password != nil,
            skipJunk: true,
            windowsSafe: windowsSafe,
            macMetadata: macMetadata
        )
    }
}

/// 对话框入口。
@MainActor
enum CompressDialog {
    /// 返回确认后的参数；取消返回 nil。
    static func run(sources: [URL], parent: NSWindow?) async -> CompressOptions? {
        await DialogHost.present(title: String(localized: "新建压缩包"), parent: parent) { finish in
            CompressDialogView(sources: sources, finish: finish)
        }
    }
}

/// 对话框内容。
struct CompressDialogView: View {
    let sources: [URL]
    let finish: @MainActor (CompressOptions?) -> Void

    @State private var format: CreateFormat = .zip
    @State private var level: Double = 5
    @State private var password = ""
    @State private var passwordConfirm = ""
    @State private var encryptNames = false
    @State private var skipJunk = true
    @State private var windowsSafe = false
    @State private var macMetadata = false
    @State private var baseName: String
    @State private var directory: URL
    @State private var showAdvanced = false

    /// 默认文件名：单个源 → 源名；多个 → 它们所在目录名。输出到源所在目录。
    init(sources: [URL], finish: @escaping @MainActor (CompressOptions?) -> Void) {
        self.sources = sources
        self.finish = finish
        _directory = State(initialValue: sources[0].deletingLastPathComponent())
        _baseName = State(initialValue: ArchiveNaming.defaultBaseName(for: sources))
    }

    /// 单文件压缩流不能打包目录或多个文件。
    private var isSingleFile: Bool {
        sources.count == 1 && (try? sources[0].resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])).map { $0.isRegularFile == true && $0.isSymbolicLink != true } == true
    }

    private var supportsPassword: Bool { format == .zip || format == .sevenZ }
    private var supportsMetadata: Bool { format != .sevenZ && !format.isSingleFile }

    /// 完整文件名：基础名 + 当前格式的扩展名
    private var fileName: String {
        "\(baseName).\(formatExtension(format: format))"
    }

    /// 已存在就加「 2」「 3」…
    private var destination: URL {
        ArchiveNaming.freeDestination(directory: directory, baseName: baseName, format: format)
    }

    /// 两次密码不一致（只在填了密码时算）
    private var passwordMismatch: Bool {
        !password.isEmpty && password != passwordConfirm
    }

    /// 能否按「压缩」：名字非空、密码一致；要加密文件名就必须有密码，否则会得到没加密的 7z。
    private var canSubmit: Bool {
        guard !baseName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !baseName.contains("/"), baseName != ".", baseName != "..",
              !supportsPassword || !passwordMismatch else { return false }
        if format == .sevenZ, encryptNames, password.isEmpty { return false }
        return true
    }

    /// 顶部的来源摘要：最多列 3 个名字
    private var sourceSummary: String {
        let names = sources.prefix(3).map(\.lastPathComponent).joined(separator: String(localized: "、"))
        return sources.count > 3 ? String(localized: "\(names) 等 \(sources.count) 项") : names
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("压缩 \(sources.count) 个项目")
                .font(.headline)
            Text(sourceSummary)
                .font(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(2)
            HStack {
                Text("格式")
                    .frame(width: 52, alignment: .trailing)
                Picker("格式", selection: $format) {
                    ForEach(CompressionFormat.allCases.filter { !$0.isSingleFile || isSingleFile }, id: \.self) { candidate in
                        Text(candidate.title).tag(candidate.coreFormat)
                    }
                }
                .labelsHidden()
                .frame(width: 170)
                Spacer()
            }
            nameAndLocation
            Button {
                showAdvanced.toggle()
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: showAdvanced ? "chevron.down" : "chevron.right")
                        .font(.caption.weight(.semibold))
                    Text("高级")
                    Spacer()
                }
                .padding(.vertical, 6)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityValue(showAdvanced ? String(localized: "已展开") : String(localized: "已折叠"))
            if showAdvanced {
                ScrollView {
                    advanced
                        .padding(.vertical, 4)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(height: 260)
            }
            HStack {
                Spacer()
                Button("取消") {
                    finish(nil)
                }
                .keyboardShortcut(.cancelAction)
                Button("压缩") {
                    submit()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!canSubmit)
            }
        }
        .padding(20)
        .frame(width: 520)
        .fixedSize(horizontal: false, vertical: true)
        .onChange(of: format) { _, _ in
            // 非 7z 没有文件名加密
            if format != .sevenZ { encryptNames = false }
            if !supportsPassword {
                password = ""
                passwordConfirm = ""
            }
            if !supportsMetadata { macMetadata = false }
        }
    }

    /// 文件名 + 位置。
    private var nameAndLocation: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("文件名")
                    .frame(width: 52, alignment: .trailing)
                TextField("", text: $baseName)
                    .textFieldStyle(.roundedBorder)
                Text(".\(formatExtension(format: format))")
                    .foregroundStyle(.secondary)
                    .monospaced()
            }
            HStack {
                Text("位置")
                    .frame(width: 52, alignment: .trailing)
                Text(directory.path)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .foregroundStyle(.secondary)
                Spacer()
                Button("选择…") {
                    chooseDirectory()
                }
            }
            if destination.lastPathComponent != fileName {
                Text("同名文件已存在，将保存为「\(destination.lastPathComponent)」")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.leading, 58)
            }
        }
    }

    /// 「高级」里的全部选项。
    private var advanced: some View {
        Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 10) {
            GridRow {
                Text("预设")
                    .gridColumnAlignment(.trailing)
                Menu("选择预设…") {
                    ForEach(CompressPreset.allCases) { candidate in
                        Button(candidate.title) { apply(candidate) }
                    }
                }
                .frame(width: 170)
            }
            GridRow {
                Text("级别")
                Slider(value: $level, in: 0...9, step: 1) {
                    EmptyView()
                } minimumValueLabel: {
                    Text("快")
                } maximumValueLabel: {
                    Text("小")
                }
                .frame(width: 220)
                .disabled(format == .tar)
                Text("\(Int(level))")
                    .monospacedDigit()
            }
            GridRow {
                Text("密码")
                SecureField("不加密", text: $password)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 220)
                    .disabled(!supportsPassword)
            }
            GridRow {
                Text("确认密码")
                SecureField("再输一次", text: $passwordConfirm)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 220)
                    .disabled(!supportsPassword)
                if supportsPassword && passwordMismatch {
                    Text("两次输入不一致")
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
            GridRow {
                Text("")
                Toggle("连文件名一起加密（仅 7z）", isOn: $encryptNames)
                    .disabled(format != .sevenZ || password.isEmpty)
            }
            GridRow {
                Text("")
                Toggle("跳过 .DS_Store、__MACOSX 等垃圾文件", isOn: $skipJunk)
            }
            GridRow {
                Text("")
                Toggle("文件名改成 Windows 安全名", isOn: $windowsSafe)
            }
            GridRow {
                Text("")
                // zip 写成 __MACOSX/._，tar.* 写 ._ + PAX xattr；7z 没有对应的存放方式
                Toggle("保留 Mac 元数据（Finder 标签、资源分支）", isOn: $macMetadata)
                    .disabled(!supportsMetadata)
            }
        }
    }

    /// 套预设：格式、级别、安全名、文件名加密、Mac 元数据；非加密预设清掉密码，加密预设展开「高级」让用户填密码。
    private func apply(_ candidate: CompressPreset) {
        format = candidate.format
        level = candidate.level
        windowsSafe = candidate.windowsSafe
        encryptNames = candidate.encryptNames
        macMetadata = candidate.macMetadata
        if candidate != .encrypted {
            password = ""
            passwordConfirm = ""
        } else {
            showAdvanced = true
        }
    }

    /// 「选择…」：选输出目录。面板挂在对话框窗口上当 sheet，没有键窗口时退回独立面板。
    private func chooseDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.directoryURL = directory
        panel.prompt = String(localized: "选择")
        Task { @MainActor in
            let response: NSApplication.ModalResponse
            if let window = NSApp.keyWindow {
                response = await panel.beginSheetModal(for: window)
            } else {
                response = await panel.begin()
            }
            if response == .OK, let url = panel.url {
                directory = url
            }
        }
    }

    /// 「压缩」：把界面状态收成 CompressOptions 交回。
    /// encryptNames 再推一遍而不是直接用开关值：开关可能在禁用状态下还留着 true（先选加密再改成 zip、或删掉密码），
    /// Rust 收到「加密文件名但没密码」会报错，这里保证只有 7z + 有密码时才为真。
    private func submit() {
        let options = CompressOptions(
            destination: destination,
            sources: sources,
            format: format,
            level: UInt8(level),
            password: supportsPassword && !password.isEmpty ? password : nil,
            encryptNames: encryptNames && format == .sevenZ && !password.isEmpty,
            skipJunk: skipJunk,
            windowsSafe: windowsSafe,
            // 开关在 7z 下是禁用的，但值可能还留着 true（先选 Mac 专用再改成 7z）；Rust 对 7z 会忽略，这里也不传
            macMetadata: macMetadata && supportsMetadata
        )
        finish(options)
    }
}

/// 输出文件的默认命名，对话框和无界面入口（-PiesiCompress、App Intents）共用。
enum ArchiveNaming {
    /// 默认基础名：单个源 → 源名；多个 → 它们所在目录名；都拿不到就叫「压缩包」。
    static func defaultBaseName(for sources: [URL]) -> String {
        let name = sources.count == 1 ? sources[0].lastPathComponent : sources[0].deletingLastPathComponent().lastPathComponent
        return name.isEmpty ? String(localized: "压缩包") : name
    }

    /// directory/baseName.ext；已存在就加「 2」「 3」…
    static func freeDestination(directory: URL, baseName: String, format: CreateFormat) -> URL {
        let fm = FileManager.default
        let ext = formatExtension(format: format)
        var candidate = directory.appendingPathComponent("\(baseName).\(ext)")
        var index = 2
        while fm.fileExists(atPath: candidate.path) {
            candidate = directory.appendingPathComponent("\(baseName) \(index).\(ext)")
            index += 1
        }
        return candidate
    }
}
