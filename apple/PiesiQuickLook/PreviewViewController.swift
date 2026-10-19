// PreviewViewController.swift —— Quick Look 预览扩展。设计 6.4：appex 沙盒且内存受限，只读索引、绝不解压。
import Cocoa
import Quartz

/// Quick Look 预览控制器：调 Rust 识别格式，显示「<格式> 压缩包（Piesi）」一行文字。
/// 由 Info.plist 的 NSExtensionPrincipalClass 指定，系统直接实例化，没有 nib。
final class PreviewViewController: NSViewController, QLPreviewingController {
    /// 显示结果的标签（可换行、可选中复制）
    private let label = NSTextField(wrappingLabelWithString: "")

    /// 没有 nib，手动建视图：一个居中的标签，左右各留 20pt 边距。
    override func loadView() {
        let container = NSView(frame: NSRect(x: 0, y: 0, width: 480, height: 240))
        label.alignment = .center
        label.font = .systemFont(ofSize: 18, weight: .medium)
        label.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(label)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: container.centerXAnchor),
            label.centerYAnchor.constraint(equalTo: container.centerYAnchor),
            label.leadingAnchor.constraint(greaterThanOrEqualTo: container.leadingAnchor, constant: 20),
            container.trailingAnchor.constraint(greaterThanOrEqualTo: label.trailingAnchor, constant: 20),
        ])
        view = container
    }

    /// 系统要预览某个文件时调用。只调 detect_format_name 读文件头；读文件放后台线程，结果回主线程填标签。
    func preparePreviewOfFile(at url: URL) async throws {
        let path = url.path
        let name = await Task.detached(priority: .userInitiated) {
            detectFormatName(path: path)
        }.value
        label.stringValue = name.map { "\($0) 压缩包（Piesi）" } ?? "未识别的压缩包（Piesi）"
    }
}
