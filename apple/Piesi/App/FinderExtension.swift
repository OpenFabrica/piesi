// FinderExtension.swift —— Finder 右键菜单扩展（PiesiFinder）的开关状态与系统设置入口，给设置页用。
import FinderSync
import Foundation

/// Finder 右键菜单扩展的状态查询与开关入口。
enum FinderExtension {
    /// 扩展的 bundle id（project.yml：主 app id + .findersync）
    static var bundleID: String {
        (Bundle.main.bundleIdentifier ?? "com.citrolabs.piesi") + ".findersync"
    }

    /// 扩展是否已在系统设置里打开。
    /// 为什么不用 FIFinderSyncController.isExtensionEnabled：实测开关打开了它也返回 false；
    /// pluginkit 输出的行首 "+" 表示用户已启用，"-" 表示已关闭，和系统设置里的开关一致。
    static func isEnabled() async -> Bool {
        let id = bundleID
        return await Task.detached {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/pluginkit")
            process.arguments = ["-m", "-i", id]
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = FileHandle.nullDevice
            guard (try? process.run()) != nil else { return false }
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return String(decoding: data, as: UTF8.self).split(separator: "\n").contains { $0.hasPrefix("+") }
        }.value
    }

    /// 打开系统设置里 Finder 扩展的开关页（登录项与扩展 → 文件提供程序）。
    static func openSettings() {
        FIFinderSyncController.showExtensionManagementInterface()
    }
}
