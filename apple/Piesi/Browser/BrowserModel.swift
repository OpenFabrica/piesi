// BrowserModel.swift —— 浏览窗口的状态：句柄、密码、编码、搜索、选中。列表数据由 outline 按层级向句柄拉取，Swift 不持有整棵树。
import AppKit
import Observation

/// 浏览窗口模型。句柄只做列表 / 搜索 / 预览；真正解压由 ExtractCoordinator 另开句柄。
@MainActor
@Observable
final class BrowserModel {
    let url: URL
    private(set) var handle: ArchiveHandle?
    private(set) var formatName = ""
    private(set) var entryCount: UInt32 = 0
    /// 文件原始大小之和（Rust 的 totalSize）
    private(set) var totalBytes: UInt64?
    private(set) var encodingInfo: EncodingInfo?
    private(set) var passwordState: PasswordState = .notNeeded
    private(set) var errorMessage: String?
    /// 用户输入过并验证通过的密码，预览 / 拖出 / 解压沿用
    private(set) var password: String?
    /// 搜索框文字（每敲一个字都变）
    var searchQuery = ""
    /// 防抖后的搜索词（150 ms 没再敲才更新），非空时列表变成平铺结果
    var debouncedQuery = ""
    /// 搜索结果碰到上限，底栏提示「仅显示前 200 条」
    var searchHitLimit = false
    /// 当前选中的行（outline 回写）
    var selectedRows: [Row] = []
    /// 每加一次，outline 就丢掉缓存重新拉根目录（切编码后）
    private(set) var reloadToken = 0

    init(url: URL) {
        self.url = url
    }

    var isLoaded: Bool {
        handle != nil
    }

    /// 后台打开；头加密（forNames）的包先要密码，forData 留到预览 / 解压时再要。
    func load(parent: NSWindow?) async {
        let name = url.lastPathComponent
        do {
            let opened = try await CoreBridge.open(url)
            if opened.passwordState() == .forNames {
                guard await askPassword(for: opened, name: name, parent: parent) else {
                    errorMessage = String(localized: "需要密码才能列出内容")
                    return
                }
            }
            handle = opened
            formatName = opened.formatName()
            entryCount = opened.entryCount()
            encodingInfo = opened.encodingInfo()
            passwordState = opened.passwordState()
            totalBytes = opened.totalSize()
            Log.browser.notice("opened \(self.url.path, privacy: .public) format=\(self.formatName, privacy: .public) entries=\(self.entryCount)")
        } catch {
            errorMessage = CoreBridge.message(for: error)
            Log.browser.error("open failed \(self.url.path, privacy: .public): \(self.errorMessage ?? "", privacy: .public)")
        }
    }

    /// 切换编码：只重解码不重读包，然后让列表重拉。
    func setEncoding(_ encoding: Encoding) {
        guard let handle else { return }
        handle.setEncoding(encoding: encoding)
        encodingInfo = handle.encodingInfo()
        reloadToken += 1
        Log.browser.notice("encoding → \(encoding.shortName, privacy: .public)")
    }

    /// forData 的包在预览 / 校验 / 拖出前要密码；已经有就直接通过。wrongBefore = 上一次解码时发现密码不对。
    func ensurePassword(parent: NSWindow?, wrongBefore: Bool) async -> Bool {
        guard let handle else { return false }
        if passwordState == .notNeeded || password != nil {
            return true
        }
        return await askPassword(for: handle, name: url.lastPathComponent, parent: parent, wrongBefore: wrongBefore)
    }

    /// 解压时才发现密码不对（forData 的包 setPassword 验不出来）：清掉，下次再问。
    func forgetPassword() {
        password = nil
    }

    /// 反复要密码直到通过或取消。
    private func askPassword(for handle: ArchiveHandle, name: String, parent: NSWindow?, wrongBefore: Bool = false) async -> Bool {
        var wrong = wrongBefore
        while true {
            guard let entered = await PasswordDialog.ask(archiveName: name, wrongBefore: wrong, parent: parent) else { return false }
            do {
                try await CoreBridge.setPassword(handle, entered)
                password = entered
                return true
            } catch {
                guard CoreBridge.isWrongPassword(error) else {
                    errorMessage = CoreBridge.message(for: error)
                    return false
                }
                wrong = true
            }
        }
    }

    /// 解压时沿用的编码 / 密码。
    var presets: ExtractPresets {
        ExtractPresets(encoding: encodingInfo?.encoding, password: password)
    }

    /// 把选中行转成可解压的 id。合成目录（id == max）没有真实条目，用 idsUnder 一次遍历拿到它下面全部 id
    /// （递归 listDir 每层都全量扫一遍，10 万条目会卡主线程）；真实目录只给自己的 id，Rust 会连同子树一起解。
    func extractionIDs(for rows: [Row]) -> [UInt32] {
        guard let handle else { return [] }
        var ids: [UInt32] = []
        for row in rows {
            if row.id == UInt32.max {
                ids.append(contentsOf: handle.idsUnder(dir: row.path))
            } else {
                ids.append(row.id)
            }
        }
        return ids
    }

    /// 顶栏常驻的「文件名编码：GB18030（自动）」
    var encodingLabel: String {
        guard let info = encodingInfo else { return String(localized: "文件名编码") }
        let how = info.source == "manual" ? String(localized: "手动") : String(localized: "自动")
        return String(localized: "文件名编码：\(info.encoding.shortName)（\(how)）")
    }

    /// 底栏：条目数、总大小、格式、是否加密
    var bottomBarText: String {
        var parts = [String(localized: "\(entryCount) 个条目")]
        if let totalBytes {
            parts.append(ByteText.string(totalBytes))
        }
        parts.append(formatName)
        if passwordState != .notNeeded {
            parts.append(String(localized: "密码保护"))
        }
        return parts.joined(separator: " · ")
    }
}
