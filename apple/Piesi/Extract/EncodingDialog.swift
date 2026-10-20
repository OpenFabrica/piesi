// EncodingDialog.swift —— 编码选择条：候选编码 + 5 个样本名实时预览，切换即 setEncoding（只重解码不重读包）。
import AppKit
import SwiftUI

/// 编码选择对话框。
@MainActor
enum EncodingDialog {
    /// 返回 true = 用户确认（所选编码已经设到 handle 上）；false = 取消。
    static func run(handle: ArchiveHandle, archiveName: String, parent: NSWindow?) async -> Bool {
        await DialogHost.present(title: String(localized: "文件名编码"), parent: parent) { finish in
            EncodingPickerView(handle: handle, archiveName: archiveName, finish: finish)
        }
    }
}

/// 选择条内容。
struct EncodingPickerView: View {
    let handle: ArchiveHandle
    let archiveName: String
    let finish: @MainActor (Bool) -> Void
    private let detected: EncodingInfo
    @State private var selected: Encoding
    @State private var samples: [String]

    init(handle: ArchiveHandle, archiveName: String, finish: @escaping @MainActor (Bool) -> Void) {
        self.handle = handle
        self.archiveName = archiveName
        self.finish = finish
        let info = handle.encodingInfo()
        detected = info
        _selected = State(initialValue: info.encoding)
        _samples = State(initialValue: handle.sampleNames(limit: 5))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("「\(archiveName)」的文件名编码不确定")
                .font(.headline)
            Text("自动识别为 \(detected.encoding.displayName)，置信度 \(Int(detected.confidence * 100))%%。选一个让右边文件名看起来正常的编码。")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack(alignment: .top, spacing: 16) {
                List(Encoding.allCases, selection: $selected) { encoding in
                    Text(encoding.displayName)
                        .tag(encoding)
                }
                .frame(width: 200, height: 196)
                VStack(alignment: .leading, spacing: 6) {
                    Text("预览")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if samples.isEmpty {
                        Text("（没有非 ASCII 的文件名）")
                            .foregroundStyle(.tertiary)
                    }
                    ForEach(samples.indices, id: \.self) { index in
                        Text(samples[index])
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer()
                }
                .frame(width: 320, alignment: .leading)
            }
            HStack {
                Spacer()
                Button("取消") {
                    finish(false)
                }
                .keyboardShortcut(.cancelAction)
                Button("继续") {
                    finish(true)
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .onChange(of: selected) { _, encoding in
            handle.setEncoding(encoding: encoding)
            samples = handle.sampleNames(limit: 5)
        }
    }
}
