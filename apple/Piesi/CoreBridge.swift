// CoreBridge.swift —— Swift 侧唯一直接调用 uniffi 生成函数的地方，其它代码只认这里的 API。
// uniffi 把 Rust 的 snake_case 转成了 camelCase：core_version → coreVersion()，detect_format_name → detectFormatName(path:)。
import Foundation

/// 对 Generated/piesi_ffi.swift 的薄封装：集中处理命名差异和线程切换。
enum CoreBridge {
    /// Rust 引擎版本（Cargo.toml 的 version，如 0.1.0-dev）。
    /// 成员名刻意不和生成的全局函数同名，否则在这里面调用会被自己遮蔽。
    static var version: String {
        coreVersion()
    }

    /// 识别文件格式名；认不出返回 nil。
    /// Rust 侧要读文件头，放到后台线程跑，不卡主线程（Task.detached 不继承调用方的 actor）。
    static func detectFormat(at url: URL) async -> String? {
        let path = url.path
        return await Task.detached(priority: .userInitiated) {
            detectFormatName(path: path)
        }.value
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
