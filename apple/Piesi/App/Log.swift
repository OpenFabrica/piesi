// Log.swift —— 统一日志入口。查看：log show --predicate 'subsystem == "com.citrolabs.piesi"' --style compact
import OSLog

/// 各模块的 Logger；subsystem 固定为 bundle id，分类按模块分。
enum Log {
    static let subsystem = "com.citrolabs.piesi"
    static let app = Logger(subsystem: subsystem, category: "app")
    static let open = Logger(subsystem: subsystem, category: "open")
    static let extract = Logger(subsystem: subsystem, category: "extract")
    static let browser = Logger(subsystem: subsystem, category: "browser")
    static let create = Logger(subsystem: subsystem, category: "create")
}
