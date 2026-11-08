// FinderRequest.swift —— Finder 右键菜单扩展与主 app 的交接：扩展把请求写进 App Group 共享目录，再用 piesi:// 链接拉起主 app。
// 主 app 和 PiesiFinder 扩展都编译这个文件（project.yml）。
import Foundation

/// Finder 扩展与主应用共用的创建格式；不让轻量扩展链接 Rust 引擎。
enum CompressionFormat: String, CaseIterable, Codable, Sendable {
    case zip = "zip", sevenZ = "7z", tar = "tar", tarGz = "tar.gz", tarBz2 = "tar.bz2", tarXz = "tar.xz", tarZst = "tar.zst"
    case gz = "gz", bz2 = "bz2", xz = "xz", zst = "zst"

    var title: String { self == .zip ? "ZIP" : self == .tar ? "TAR" : rawValue }

    /// 裸压缩流只支持一个普通文件。
    var isSingleFile: Bool { [.gz, .bz2, .xz, .zst].contains(self) }
}

/// Finder 右键菜单发给主 app 的一次请求。
struct FinderRequest: Codable, Sendable {
    /// 菜单命令：解压到压缩包旁边 / 浏览 / 压缩
    enum Action: String, Codable, Sendable {
        case extract, browse, compress
    }

    let action: Action
    let paths: [String]
    /// nil 打开自定义对话框；有格式时按默认选项直接压缩。旧请求没有该字段，仍可解码。
    var format: CompressionFormat? = nil
}

/// 交接用的常量与读写。
/// 为什么不把路径直接放进链接：任何网页都能打开 piesi:// 链接。请求内容放在 App Group 目录里，
/// 只有本团队签名的程序写得进去；链接只带一个随机 id，伪造的链接找不到对应文件，什么也不做。
enum FinderHandoff {
    static let scheme = "piesi"
    static let host = "finder"
    /// 没被主 app 取走的请求（比如主 app 启动失败）放多久后清掉
    private static let staleAge: TimeInterval = 3600

    /// App Group id，写在两边 Info.plist 的 PiesiAppGroup（$(DEVELOPMENT_TEAM).com.citrolabs.piesi），与 entitlements 一致。
    static var groupID: String? {
        Bundle.main.object(forInfoDictionaryKey: "PiesiAppGroup") as? String
    }

    /// 共享目录里放请求文件的子目录。
    static func requestsDirectory() -> URL? {
        guard let groupID,
              let container = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: groupID) else { return nil }
        return container.appendingPathComponent("FinderRequests", isDirectory: true)
    }

    /// 扩展侧：写请求文件，返回拉起主 app 的链接。
    static func post(_ request: FinderRequest) throws -> URL {
        guard let dir = requestsDirectory() else { throw CocoaError(.fileNoSuchFile) }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let id = UUID().uuidString
        try JSONEncoder().encode(request).write(to: dir.appendingPathComponent("\(id).json"), options: .atomic)
        var components = URLComponents()
        components.scheme = scheme
        components.host = host
        components.queryItems = [URLQueryItem(name: "id", value: id)]
        guard let url = components.url else { throw CocoaError(.fileWriteUnknown) }
        return url
    }

    /// 主 app 侧：按链接里的 id 取出请求并删掉文件。id 必须是 UUID，挡住 ../ 之类的路径。
    static func take(_ url: URL) -> FinderRequest? {
        guard url.scheme == scheme, url.host == host,
              let value = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "id" })?.value,
              let id = UUID(uuidString: value),
              let dir = requestsDirectory() else { return nil }
        removeStale(in: dir)
        let file = dir.appendingPathComponent("\(id.uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        guard let data = try? Data(contentsOf: file) else { return nil }
        return try? JSONDecoder().decode(FinderRequest.self, from: data)
    }

    /// 清掉超过 staleAge 还没被取走的请求文件。
    private static func removeStale(in dir: URL) {
        let fm = FileManager.default
        guard let files = try? fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: [.contentModificationDateKey]) else { return }
        for file in files {
            let modified = (try? file.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate ?? .distantPast
            if Date().timeIntervalSince(modified) > staleAge {
                try? fm.removeItem(at: file)
            }
        }
    }
}
