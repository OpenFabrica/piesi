// DiskSpace.swift —— 解压前的磁盘空间预检。
import Foundation

/// 查目标所在卷的可用空间。
enum DiskSpace {
    /// 目标目录可能还没建（智能文件夹），向上找到第一个存在的祖先再问卷容量；找不到返回 nil（不拦）。
    static func available(forPath path: String) -> UInt64? {
        var url = URL(fileURLWithPath: path)
        let fm = FileManager.default
        while !fm.fileExists(atPath: url.path) {
            let parent = url.deletingLastPathComponent()
            if parent.path == url.path { return nil }
            url = parent
        }
        guard let capacity = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
            .volumeAvailableCapacityForImportantUsage else { return nil }
        return UInt64(max(0, capacity))
    }
}
