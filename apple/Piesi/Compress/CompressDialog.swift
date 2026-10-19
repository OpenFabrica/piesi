// CompressDialog.swift —— 压缩对话框（设计 6.2）：四张预设卡片 + 文件名 / 位置 + 「高级」里的格式、级别、密码、垃圾过滤、Windows 安全名。
import AppKit
import SwiftUI

/// 预设卡片。选中即套一组默认值，之后还能在「高级」里改。
enum CompressPreset: CaseIterable, Identifiable {
    case windows
    case smallest
    case encrypted
    case mac

    var id: Self { self }

    var title: String {
        switch self {
        case .windows: "发给 Windows"
        case .smallest: "最小体积"
        case .encrypted: "加密"
        case .mac: "Mac 专用"
        }
    }

    var subtitle: String {
        switch self {
        case .windows: "ZIP · Windows 安全文件名"
        case .smallest: "7z · 级别 7"
        case .encrypted: "7z · 密码 + 文件名加密"
        case .mac: "ZIP · 级别 5"
        }
    }

    var icon: String {
        switch self {
        case .windows: "pc"
        case .smallest: "arrow.down.right.and.arrow.up.left"
        case .encrypted: "lock.fill"
        case .mac: "desktopcomputer"
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
}

/// 对话框入口。
@MainActor
enum CompressDialog {
    /// 返回确认后的参数；取消返回 nil。
    static func run(sources: [URL], parent: NSWindow?) async -> CompressOptions? {
        await DialogHost.present(title: "新建压缩包", parent: parent) { finish in
            CompressDialogView(sources: sources, finish: finish)
        }
    }
}

/// 对话框内容。
struct CompressDialogView: View {
    let sources: [URL]
    let finish: @MainActor (CompressOptions?) -> Void

    @State private var preset: CompressPreset? = .windows
    @State private var format: CreateFormat = .zip
    @State private var level: Double = 5
    @State private var password = ""
    @State private var passwordConfirm = ""
    @State private var encryptNames = false
    @State private var skipJunk = true
    @State private var windowsSafe = true
    @State private var baseName: String
    @State private var directory: URL
    @State private var showAdvanced = false

    /// 默认文件名：单个源 → 源名；多个 → 它们所在目录名。输出到源所在目录。
    init(sources: [URL], finish: @escaping @MainActor (CompressOptions?) -> Void) {
        self.sources = sources
        self.finish = finish
        let dir = sources[0].deletingLastPathComponent()
        _directory = State(initialValue: dir)
        let name = sources.count == 1 ? sources[0].lastPathComponent : dir.lastPathComponent
        _baseName = State(initialValue: name.isEmpty ? "压缩包" : name)
    }

    private var fileName: String {
        "\(baseName).\(formatExtension(format: format))"
    }

    /// 已存在就加「 2」「 3」…
    private var destination: URL {
        let fm = FileManager.default
        let ext = formatExtension(format: format)
        var candidate = directory.appendingPathComponent(fileName)
        var index = 2
        while fm.fileExists(atPath: candidate.path) {
            candidate = directory.appendingPathComponent("\(baseName) \(index).\(ext)")
            index += 1
        }
        return candidate
    }

    private var passwordMismatch: Bool {
        !password.isEmpty && password != passwordConfirm
    }

    private var canSubmit: Bool {
        guard !baseName.trimmingCharacters(in: .whitespaces).isEmpty, !passwordMismatch else { return false }
        if preset == .encrypted, password.isEmpty { return false }
        return true
    }

    private var sourceSummary: String {
        let names = sources.prefix(3).map(\.lastPathComponent).joined(separator: "、")
        return sources.count > 3 ? "\(names) 等 \(sources.count) 项" : names
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("压缩 \(sources.count) 个项目")
                .font(.headline)
            Text(sourceSummary)
                .font(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(2)
            presetCards
            nameAndLocation
            DisclosureGroup("高级", isExpanded: $showAdvanced) {
                advanced
                    .padding(.top, 6)
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
        .frame(width: 580)
        .onChange(of: format) { _, _ in
            // 非 7z 没有文件名加密
            if format != .sevenZ {
                encryptNames = false
            }
        }
    }

    /// 四张预设卡片。
    private var presetCards: some View {
        HStack(spacing: 10) {
            ForEach(CompressPreset.allCases) { candidate in
                Button {
                    apply(candidate)
                } label: {
                    VStack(spacing: 6) {
                        Image(systemName: candidate.icon)
                            .font(.title2)
                        Text(candidate.title)
                            .font(.callout.weight(.medium))
                        Text(candidate.subtitle)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity, minHeight: 84)
                    .padding(8)
                    .background(
                        RoundedRectangle(cornerRadius: 10)
                            .fill(preset == candidate ? Color.accentColor.opacity(0.15) : Color(nsColor: .controlBackgroundColor))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .strokeBorder(preset == candidate ? Color.accentColor : Color.secondary.opacity(0.3), lineWidth: preset == candidate ? 2 : 1)
                    )
                }
                .buttonStyle(.plain)
            }
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
                Text("格式")
                    .gridColumnAlignment(.trailing)
                Picker("", selection: $format) {
                    Text("ZIP").tag(CreateFormat.zip)
                    Text("7z").tag(CreateFormat.sevenZ)
                    Text("tar.gz").tag(CreateFormat.tarGz)
                    Text("tar.xz").tag(CreateFormat.tarXz)
                    Text("tar.zst").tag(CreateFormat.tarZst)
                }
                .labelsHidden()
                .frame(width: 140)
                .onChange(of: format) { _, _ in preset = nil }
            }
            GridRow {
                Text("级别")
                Slider(value: $level, in: 1...9, step: 1) {
                    EmptyView()
                } minimumValueLabel: {
                    Text("快")
                } maximumValueLabel: {
                    Text("小")
                }
                .frame(width: 220)
                .onChange(of: level) { _, _ in preset = nil }
                Text("\(Int(level))")
                    .monospacedDigit()
            }
            GridRow {
                Text("密码")
                SecureField("不加密", text: $password)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 220)
            }
            GridRow {
                Text("确认密码")
                SecureField("再输一次", text: $passwordConfirm)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 220)
                if passwordMismatch {
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
                    .onChange(of: windowsSafe) { _, _ in preset = nil }
            }
        }
    }

    /// 套预设：格式、级别、安全名、文件名加密；非加密预设清掉密码。
    private func apply(_ candidate: CompressPreset) {
        format = candidate.format
        level = candidate.level
        windowsSafe = candidate.windowsSafe
        encryptNames = candidate.encryptNames
        if candidate != .encrypted {
            password = ""
            passwordConfirm = ""
        } else {
            showAdvanced = true
        }
        // 上面几处 onChange 会把 preset 清成 nil，最后再设回来
        preset = candidate
    }

    private func chooseDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.directoryURL = directory
        panel.prompt = "选择"
        Task { @MainActor in
            if await panel.begin() == .OK, let url = panel.url {
                directory = url
            }
        }
    }

    private func submit() {
        let options = CompressOptions(
            destination: destination,
            sources: sources,
            format: format,
            level: UInt8(level),
            password: password.isEmpty ? nil : password,
            encryptNames: encryptNames && format == .sevenZ && !password.isEmpty,
            skipJunk: skipJunk,
            windowsSafe: windowsSafe
        )
        finish(options)
    }
}
