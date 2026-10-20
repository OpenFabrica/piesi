//  CoreText.swift —— 核心（Rust）传来的结构化错误、警告 → 用户看的本地化文案。
//  核心只给类型和参数（docs/design.md 5.5），句子在这里按 String Catalog 拼；CLI 走核心自己的中文 Display。

import Foundation

extension PiesiError {
    /// 错误的本地化文案：按变体翻译标题，核心给的细节（第三方库文案、系统错误）原样附在后面。
    var localizedText: String {
        switch self {
        case .UnknownFormat: String(localized: "无法识别的压缩格式")
        case .Unsupported(let detail): String(localized: "不支持：\(detail)")
        case .Corrupt(let detail): String(localized: "压缩包损坏：\(detail)")
        case .PasswordRequired: String(localized: "需要密码")
        case .WrongPassword: String(localized: "密码错误")
        case .Cancelled: String(localized: "已取消")
        case .UnsafePath(let detail): String(localized: "不安全的路径：\(detail)")
        case .NoSpace(let needed, let available): String(localized: "磁盘空间不足：需要 \(ByteText.string(needed))，可用 \(ByteText.string(available))")
        case .Io(let message): message
        case .Other(let message): message
        }
    }
}

extension Warning {
    /// 警告的本地化文案：按 kind 查表，subject（哪个文件）和 detail（链接目标、新名、系统错误）原样填入。
    var localizedText: String {
        switch kind {
        case .unsafePathSkipped: String(localized: "跳过条目 \(subject)：\(detail)")
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
