// ServicesProvider.swift —— Finder 右键「服务」菜单的入口：Info.plist 的 NSServices 声明 + NSApp.servicesProvider。
import AppKit

/// 服务处理器。方法名必须和 Info.plist 里的 NSMessage 一致，选择器形如 message:userData:error:；系统在主线程调。
@MainActor
final class ServicesProvider: NSObject {
    /// 「用 Piesi 解压到此处」：解到压缩包旁边，走和双击一样的流程。
    @objc func extractHereWithPiesi(_ pasteboard: NSPasteboard, userData: String?, error: AutoreleasingUnsafeMutablePointer<NSString?>) {
        handle(pasteboard, error: error, browse: false)
    }

    /// 「用 Piesi 压缩」：任意文件 / 文件夹进压缩对话框。
    @objc func compressWithPiesi(_ pasteboard: NSPasteboard, userData: String?, error: AutoreleasingUnsafeMutablePointer<NSString?>) {
        let urls = fileURLs(from: pasteboard)
        guard !urls.isEmpty else {
            error.pointee = String(localized: "没有收到文件") as NSString
            return
        }
        Log.open.notice("service compress: \(urls.count) files")
        AppDelegate.shared.compress(sources: urls)
    }

    /// 「用 Piesi 查看」：浏览窗口。
    @objc func browseWithPiesi(_ pasteboard: NSPasteboard, userData: String?, error: AutoreleasingUnsafeMutablePointer<NSString?>) {
        handle(pasteboard, error: error, browse: true)
    }

    /// 取剪贴板里的文件 URL 交给打开队列。
    private func handle(_ pasteboard: NSPasteboard, error: AutoreleasingUnsafeMutablePointer<NSString?>, browse: Bool) {
        let urls = fileURLs(from: pasteboard)
        guard !urls.isEmpty else {
            error.pointee = String(localized: "没有收到文件") as NSString
            return
        }
        Log.open.notice("service \(browse ? "browse" : "extract-here", privacy: .public): \(urls.count) files")
        AppDelegate.shared.open(urls: urls, browse: browse)
    }

    /// 剪贴板里的文件 URL（Finder 发服务时放的是 public.file-url）。
    func fileURLs(from pasteboard: NSPasteboard) -> [URL] {
        pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
    }
}
