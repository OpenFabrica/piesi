// CompressionMenuTests.swift —— 直接运行 Finder 的菜单构造与 Codable 请求兼容回归，不加载扩展实例。
import AppKit

/// 编译方式见 docs/compression.md；不向 Finder 注册扩展、不更改系统设置。
@main
struct CompressionMenuTests {
    @MainActor
    static func main() throws {
        for singleFile in [false, true] {
            let menu = FinderSync.compressionMenu(singleFile: singleFile)
            let formats = menu.items.compactMap { FinderSync.compressionFormat(menuTag: $0.tag) }
            let expected = CompressionFormat.allCases.filter { !$0.isSingleFile || singleFile }
            precondition(formats == expected)
            precondition(menu.items.dropLast(2).allSatisfy { $0.representedObject == nil })
            precondition(formats.count == (singleFile ? 11 : 7))
            precondition(menu.items.dropLast(2).allSatisfy { $0.action == #selector(FinderSync.compressFormat(_:)) })
            precondition(menu.items[menu.items.count - 2].isSeparatorItem)
            precondition(menu.items.last?.action == #selector(FinderSync.compress(_:)))
        }
        precondition(FinderSync.compressionFormat(menuTag: 0) == nil)
        precondition(FinderSync.compressionFormat(menuTag: -1) == nil)
        precondition(FinderSync.compressionFormat(menuTag: 1000) == nil)
        let old = Data(#"{"action":"compress","paths":["/tmp/中文.txt"]}"#.utf8)
        let decoded = try JSONDecoder().decode(FinderRequest.self, from: old)
        precondition(decoded.format == nil)
        for format in CompressionFormat.allCases {
            let request = FinderRequest(action: .compress, paths: decoded.paths, format: format)
            let copy = try JSONDecoder().decode(FinderRequest.self, from: JSONEncoder().encode(request))
            precondition(copy.format == format && copy.paths == decoded.paths)
        }
        print("通过：7/11 格式菜单、选择器及参数、旧请求兼容、11 格式请求往返")
    }
}
