// CoreBridge.swift —— Swift 侧唯一直接调用 uniffi 生成函数的地方，其它代码只认这里的 API。
// uniffi 把 Rust 的 snake_case 转成了 camelCase：open_archive → openArchive(path:locale:)，list_dir → listDir(dir:)。
// 耗时调用（打开、计划、解压、压缩）全部 Task.detached 放后台：Rust 侧持锁且阻塞，不能占主线程。
import Foundation

/// 对 Generated/piesi_ffi.swift 的薄封装：集中处理命名差异、线程切换和错误文案。
enum CoreBridge {
    /// Rust 引擎版本（Cargo.toml 的 version，如 0.1.0-dev）。
    /// 成员名刻意不和生成的全局函数同名，否则在这里面调用会被自己遮蔽。
    static var version: String {
        coreVersion()
    }

    /// 识别文件格式名；认不出返回 nil。后台读文件头。
    static func detectFormat(at url: URL) async -> String? {
        let path = url.path
        return await Task.detached(priority: .userInitiated) {
            detectFormatName(path: path)
        }.value
    }

    /// 打开压缩包并自动识别编码（locale 传 nil = 系统语言）。只读目录区，tar 系会建索引。
    static func open(_ url: URL) async throws -> ArchiveHandle {
        let path = url.path
        return try await Task.detached(priority: .userInitiated) {
            try openArchive(path: path, locale: nil)
        }.value
    }

    /// 设密码。7z 头加密时会去验证，所以也放后台。
    static func setPassword(_ handle: ArchiveHandle, _ password: String?) async throws {
        try await Task.detached(priority: .userInitiated) {
            try handle.setPassword(password: password)
        }.value
    }

    /// 只算解压计划不落盘。
    static func plan(_ handle: ArchiveHandle, _ request: ExtractRequest) async throws -> ExtractPlanInfo {
        try await Task.detached(priority: .userInitiated) {
            try handle.plan(request: request)
        }.value
    }

    /// 解压。进度经 listener 回调（Rust 已节流），取消经 cancel。
    static func extract(_ handle: ArchiveHandle, _ request: ExtractRequest,
                        listener: ProgressListener, cancel: CancelHandle) async throws -> ExtractResult {
        try await Task.detached(priority: .userInitiated) {
            try handle.extract(request: request, listener: listener, cancel: cancel)
        }.value
    }

    /// 单个条目解到临时文件（Quick Look / 用默认 app 打开用）。
    static func extractToTemp(_ handle: ArchiveHandle, id: UInt32, cancel: CancelHandle) async throws -> URL {
        let path = try await Task.detached(priority: .userInitiated) {
            try handle.extractEntryToTemp(id: id, cancel: cancel)
        }.value
        return URL(fileURLWithPath: path)
    }

    /// 创建压缩包。
    static func create(_ request: CreateRequest, listener: ProgressListener, cancel: CancelHandle) async throws -> CreateResult {
        try await Task.detached(priority: .userInitiated) {
            try createArchive(request: request, listener: listener, cancel: cancel)
        }.value
    }

    /// 错误转成给用户看的中文；Rust 的 #[error] 文案只有 Other 带过来，其余按 case 翻译。
    static func message(for error: Error) -> String {
        if let e = error as? PiesiError {
            switch e {
            case .UnknownFormat: return "无法识别的压缩格式"
            case .PasswordRequired: return "需要密码"
            case .WrongPassword: return "密码错误"
            case .Cancelled: return "已取消"
            case .Other(let message): return message
            }
        }
        return error.localizedDescription
    }

    /// 是否用户主动取消（取消不算失败，不弹错误）。
    static func isCancelled(_ error: Error) -> Bool {
        if case PiesiError.Cancelled = error { return true }
        return false
    }

    /// 是否密码错误。
    static func isWrongPassword(_ error: Error) -> Bool {
        if case PiesiError.WrongPassword = error { return true }
        return false
    }
}

/// 主 app 自身的版本信息，从 Info.plist 读（版本号来源是 Config/Version.xcconfig）。
enum AppInfo {
    /// 「Piesi 0.1.0-dev · core 0.1.0-dev」这一行
    static var versionLine: String {
        let info = Bundle.main.infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String ?? "?"
        let suffix = info["PiesiVersionSuffix"] as? String ?? ""
        return "Piesi \(version)\(suffix) · core \(CoreBridge.version)"
    }
}

/// 文件名编码的显示名和候选列表（uniffi 枚举没有 CaseIterable，这里补）。
extension Encoding: CaseIterable, Identifiable {
    public static var allCases: [Encoding] {
        [.utf8, .gb18030, .big5, .shiftJis, .eucKr, .cp1251, .cp866, .cp1252]
    }

    public var id: String { displayName }

    /// 选择器里显示的名字
    var displayName: String {
        switch self {
        case .utf8: "UTF-8"
        case .gb18030: "GB18030（简体中文）"
        case .big5: "Big5（繁体中文）"
        case .shiftJis: "Shift_JIS（日文）"
        case .eucKr: "EUC-KR（韩文）"
        case .cp1251: "CP1251（西里尔）"
        case .cp866: "CP866（DOS 西里尔）"
        case .cp1252: "CP1252（西欧）"
        }
    }

    /// 短名，顶栏「文件名编码：GBK（自动）」用
    var shortName: String {
        switch self {
        case .utf8: "UTF-8"
        case .gb18030: "GB18030"
        case .big5: "Big5"
        case .shiftJis: "Shift_JIS"
        case .eucKr: "EUC-KR"
        case .cp1251: "CP1251"
        case .cp866: "CP866"
        case .cp1252: "CP1252"
        }
    }
}

/// 字节数的统一显示（1.2 MB 这种）。用类方法而不是共享实例：ByteCountFormatter 不是 Sendable，static 实例过不了 Swift 6。
enum ByteText {
    static func string(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
    }

    static func string(_ bytes: Double) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }
}
