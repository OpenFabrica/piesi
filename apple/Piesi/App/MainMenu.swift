// MainMenu.swift —— 没有 nib，菜单栏用代码建。编辑菜单必须有，否则对话框里的文本框不能 ⌘C/⌘V。
import AppKit

/// 菜单栏。动作走响应链，AppDelegate 和各窗口控制器按需实现。
@MainActor
enum MainMenu {
    static func build() -> NSMenu {
        let menubar = NSMenu()
        menubar.addItem(appMenu())
        menubar.addItem(fileMenu())
        menubar.addItem(editMenu())
        menubar.addItem(windowMenu())
        menubar.addItem(helpMenu())
        return menubar
    }

    /// 顶层项 + 子菜单的小工具。
    private static func item(_ title: String, submenu: NSMenu) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.submenu = submenu
        return item
    }

    private static func appMenu() -> NSMenuItem {
        let menu = NSMenu(title: "Piesi")
        menu.addItem(withTitle: "关于 Piesi", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
        menu.addItem(.separator())
        menu.addItem(withTitle: "设置…", action: #selector(AppDelegate.showSettingsWindow(_:)), keyEquivalent: ",")
        menu.addItem(.separator())
        let services = NSMenu(title: "服务")
        NSApp.servicesMenu = services
        menu.addItem(item("服务", submenu: services))
        menu.addItem(.separator())
        menu.addItem(withTitle: "隐藏 Piesi", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        let hideOthers = menu.addItem(withTitle: "隐藏其他", action: #selector(NSApplication.hideOtherApplications(_:)), keyEquivalent: "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        menu.addItem(withTitle: "全部显示", action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: "")
        menu.addItem(.separator())
        menu.addItem(withTitle: "退出 Piesi", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        return item("Piesi", submenu: menu)
    }

    private static func fileMenu() -> NSMenuItem {
        let menu = NSMenu(title: "文件")
        menu.addItem(withTitle: "新建压缩包…", action: #selector(AppDelegate.newArchive(_:)), keyEquivalent: "n")
        menu.addItem(withTitle: "打开压缩包…", action: #selector(AppDelegate.openDocument(_:)), keyEquivalent: "o")
        menu.addItem(.separator())
        menu.addItem(withTitle: "关闭窗口", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        return item("文件", submenu: menu)
    }

    private static func editMenu() -> NSMenuItem {
        let menu = NSMenu(title: "编辑")
        menu.addItem(withTitle: "撤销", action: Selector(("undo:")), keyEquivalent: "z")
        let redo = menu.addItem(withTitle: "重做", action: Selector(("redo:")), keyEquivalent: "z")
        redo.keyEquivalentModifierMask = [.command, .shift]
        menu.addItem(.separator())
        menu.addItem(withTitle: "剪切", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        menu.addItem(withTitle: "拷贝", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        menu.addItem(withTitle: "粘贴", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        menu.addItem(withTitle: "删除", action: #selector(NSText.delete(_:)), keyEquivalent: "")
        menu.addItem(withTitle: "全选", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        return item("编辑", submenu: menu)
    }

    private static func windowMenu() -> NSMenuItem {
        let menu = NSMenu(title: "窗口")
        menu.addItem(withTitle: "最小化", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        menu.addItem(withTitle: "缩放", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")
        menu.addItem(.separator())
        let tasks = menu.addItem(withTitle: "任务", action: #selector(AppDelegate.showTasksWindow(_:)), keyEquivalent: "t")
        tasks.keyEquivalentModifierMask = [.command, .option]
        menu.addItem(.separator())
        menu.addItem(withTitle: "前置全部窗口", action: #selector(NSApplication.arrangeInFront(_:)), keyEquivalent: "")
        NSApp.windowsMenu = menu
        return item("窗口", submenu: menu)
    }

    private static func helpMenu() -> NSMenuItem {
        let menu = NSMenu(title: "帮助")
        NSApp.helpMenu = menu
        return item("帮助", submenu: menu)
    }
}
