// BrowserView.swift —— 浏览窗口内容：工具栏（解压 / 编码 / 搜索）+ NSOutlineView 列表 + 底栏。
import SwiftUI

/// 窗口控制器交给视图的动作。
struct BrowserActions {
    var extractAll: @MainActor () -> Void
    var extractSelected: @MainActor () -> Void
    /// [测试]：解码 + CRC 校验整个包
    var verify: @MainActor () -> Void
    var outline: OutlineActions
}

/// 浏览窗口主视图。
struct BrowserView: View {
    @Bindable var model: BrowserModel
    let actions: BrowserActions

    var body: some View {
        VStack(spacing: 0) {
            content
            Divider()
            bottomBar
        }
        .frame(minWidth: 640, minHeight: 400)
        // 搜索防抖：每敲一个字就 search 一次太浪费，150 ms 没再敲才真正搜；清空则立刻生效
        .task(id: model.searchQuery) {
            let query = model.searchQuery
            if !query.isEmpty {
                try? await Task.sleep(for: .milliseconds(150))
                if Task.isCancelled { return }
            }
            model.debouncedQuery = query
        }
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                extractMenu
                Button {
                    actions.verify()
                } label: {
                    Label("测试", systemImage: "checkmark.shield")
                }
                .labelStyle(.titleAndIcon)
                .help("解码并校验 CRC，不写盘")
                .disabled(!model.isLoaded)
                encodingMenu
                TextField("搜索", text: $model.searchQuery)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 180)
                    .disabled(!model.isLoaded)
            }
        }
    }

    /// 列表区：出错 / 加载中 / 列表。
    @ViewBuilder
    private var content: some View {
        if let error = model.errorMessage {
            ContentUnavailableView("无法打开", systemImage: "exclamationmark.triangle", description: Text(error))
        } else if !model.isLoaded {
            ProgressView("正在打开…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ArchiveOutlineView(model: model, actions: actions.outline, reloadToken: model.reloadToken, searchQuery: model.debouncedQuery)
        }
    }

    /// [解压 ▾]：全部 / 选中。
    private var extractMenu: some View {
        Menu {
            Button("全部解压到…") {
                actions.extractAll()
            }
            Button("解压选中的 \(model.selectedRows.count) 项到…") {
                actions.extractSelected()
            }
            .disabled(model.selectedRows.isEmpty)
        } label: {
            Label("解压", systemImage: "arrow.down.doc")
        }
        .labelStyle(.titleAndIcon)
        .disabled(!model.isLoaded)
    }

    /// [编码 ▾] 常驻显示当前编码，切换后列表重拉。
    private var encodingMenu: some View {
        Menu {
            ForEach(Encoding.allCases) { encoding in
                Button {
                    model.setEncoding(encoding)
                } label: {
                    if encoding == model.encodingInfo?.encoding {
                        Label(encoding.displayName, systemImage: "checkmark")
                    } else {
                        Text(encoding.displayName)
                    }
                }
            }
        } label: {
            // 常驻文字（设计 5.2 第 6 条），不能缩成图标
            Text(model.encodingLabel)
        }
        .disabled(!model.isLoaded)
    }

    /// 底栏统计。
    private var bottomBar: some View {
        HStack {
            Text(model.isLoaded ? model.bottomBarText : " ")
                .font(.callout)
                .foregroundStyle(.secondary)
            if !model.debouncedQuery.isEmpty, model.searchHitLimit {
                Text("仅显示前 \(ArchiveOutlineView.Coordinator.searchLimit) 条")
                    .font(.callout)
                    .foregroundStyle(.orange)
            }
            Spacer()
            if !model.selectedRows.isEmpty {
                Text("已选 \(model.selectedRows.count) 项")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12)
        .frame(height: 26)
        .background(.bar)
    }
}
