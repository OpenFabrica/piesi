//  CoreText.swift —— 核心（Rust）传来的结构化警告 → 用户看的本地化文案。
//  核心只给类型和参数（docs/design.md 5.5），句子在这里按 String Catalog 拼；CLI 走核心自己的中文 Display。
//  错误（PiesiError / ErrorDetail）的文案在 apple/Shared/CoreErrorText.swift，Quick Look 扩展也要用。

import Foundation

extension Warning {
    /// 警告的本地化文案：按 kind 查表，subject（哪个文件）和 detail（链接目标、新名、系统错误）原样填入；不安全路径的原因单独翻译。
    var localizedText: String {
        switch kind {
        // detail 是核心的中文原因；按条目名重算一遍结构化原因再翻译（纯函数，和计划时结果一致），算不出来才退回原文
        case .unsafePathSkipped: String(localized: "跳过条目 \(subject)：\(unsafePathReason(name: subject)?.localizedText ?? detail)")
        case .caseConflictRenamed: String(localized: "大小写冲突：\(subject) 改名为 \(detail)")
        case .hardLinkSkipped: String(localized: "\(subject)：硬链接暂不支持，已跳过")
        case .specialEntrySkipped: String(localized: "\(subject)：设备文件或管道，已跳过")
        case .rarReferenceSkipped: String(localized: "\(subject)：引用记录（rar -oi）暂不支持，已跳过")
        case .noDataSkipped: String(localized: "\(subject)：条目没有数据，已跳过")
        case .symlinkEscapeSkipped: String(localized: "跳过越出目标目录的符号链接：\(subject) → \(detail)")
        case .rarSymlinkRejected: String(localized: "\(subject)：unrar 拒绝还原该符号链接（目标不安全或读取失败），已跳过")
        case .symlinkCreateFailed: String(localized: "创建符号链接失败：\(subject)：\(detail)")
        case .rarSkipDecodeError: String(localized: "\(subject)：跳过时解码出错（\(detail)），已忽略")
        case .specialFileSkipped: String(localized: "跳过特殊文件：\(subject)")
        case .renamedForWindows: String(localized: "已改名以兼容 Windows：\(subject) → \(detail)")
        case .zipCannotEncryptNames: String(localized: "zip 不能加密文件名（只有 7z 可以），文件名未加密")
        case .macMetadataReadFailed: String(localized: "读取 Mac 元数据失败，已跳过：\(subject)：\(detail)")
        case .macResourceForkTooLarge: String(localized: "跳过 \(subject)：资源分支超过 64 MB 上限")
        case .macXattrTooLarge: String(localized: "跳过 \(subject)：扩展属性 \(detail) 放不进 AppleDouble")
        case .macXattrNameTooLong: String(localized: "跳过 \(subject)：扩展属性名 \(detail) 太长")
        case .macFinderInfoInvalid: String(localized: "跳过 \(subject)：Finder 信息不是 32 字节")
        }
    }
}
