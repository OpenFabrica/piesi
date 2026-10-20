//  CoreErrorText.swift —— 核心（Rust）传来的结构化错误 → 用户看的本地化文案。
//  主 app 和 Quick Look 扩展共用（apple/project.yml 两个 target 都引用 Shared/），所以两边的 Localizable.xcstrings 都要有这里的键。
//  核心只给变体和细节类型（docs/design.md 5.5），句子在这里按 String Catalog 拼；CLI 走核心自己的中文 Display。

import Foundation

extension PiesiError {
    /// 错误的本地化文案：按变体翻译标题，结构化细节按 kind 翻译后接在后面；系统错误原文照抄。
    var localizedText: String {
        switch self {
        case .UnknownFormat: String(localized: "无法识别的压缩格式")
        case .Unsupported(let detail): String(localized: "不支持：\(detail.localizedText)")
        case .Corrupt(let detail): String(localized: "压缩包损坏：\(detail.localizedText)")
        case .PasswordRequired: String(localized: "需要密码")
        case .WrongPassword: String(localized: "密码错误")
        case .Cancelled: String(localized: "已取消")
        case .UnsafePath(let detail): String(localized: "不安全的路径：\(detail.localizedText)")
        // Quick Look 扩展没有 ByteText（在主 app 的 CoreBridge.swift），这里直接用同样的 ByteCountFormatter 设置
        case .NoSpace(let needed, let available): String(localized: "磁盘空间不足：需要 \(Self.bytes(needed))，可用 \(Self.bytes(available))")
        case .Io(let message): message
        case .Other(let message): message
        }
    }

    /// 字节数显示，和主 app 的 `ByteText.string(_:)` 一致。
    private static func bytes(_ count: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: count), countStyle: .file)
    }
}

extension ErrorDetail {
    /// 细节的本地化文案：按 kind 查表，参数（条目名、偏移、方法号…）原样填入。
    /// `library` 是第三方库或系统给的原文、`plain` 是格式名或路径，都翻不了，原样显示。
    var localizedText: String {
        let a = arg(0), b = arg(1), c = arg(2)
        return switch kind {
        case .library, .plain: a
        case .pathControlChars: String(localized: "含控制字符：\(a)")
        case .pathAbsolute: String(localized: "绝对路径：\(a)")
        case .pathDrivePrefix: String(localized: "Windows 盘符：\(a)")
        case .pathDotComponent: String(localized: "路径含 `\(a)` 分量：\(b)")
        case .pathEmpty: String(localized: "空路径：\(a)")
        case .entryIndexOutOfRange: String(localized: "条目序号越界")
        case .tooManyEntries: String(localized: "条目数超过上限")
        case .symlinkTargetTooLong: String(localized: "符号链接目标过长：\(a)")
        case .indexMismatch: String(localized: "压缩包内容与打开时建立的索引不一致")
        case .compressionMethod: String(localized: "压缩方法 \(a)")
        case .decompressFailed: String(localized: "解压失败：\(a)")
        case .rarArchiveHeaderBad: String(localized: "RAR 压缩包头损坏")
        case .rarFileHeaderBad: String(localized: "RAR 文件头损坏")
        case .rarCrcFailed: String(localized: "RAR 数据 CRC 校验失败")
        case .notRar: String(localized: "不是 RAR 压缩包")
        case .nextVolumeMissing: String(localized: "找不到下一个分卷")
        case .rarReadError: String(localized: "RAR 读取错误")
        case .rarUnknownFormat: String(localized: "RAR：未知的格式或加密方式")
        case .rarReference: String(localized: "RAR 引用记录（-oi）需要完整解压")
        case .sevenZMissingBlock: String(localized: "7z：条目引用了不存在的块")
        case .sevenZBlockMismatch: String(localized: "7z：块与文件表对不上")
        case .sevenZEntryMismatch: String(localized: "块内条目与目录不一致：\(a)")
        case .sevenZMethod: String(localized: "7z 压缩方法 \(a)")
        case .sevenZLibrary: String(localized: "7z：\(a)")
        case .tarDriverMismatch: String(localized: "tar 驱动不处理 \(a)")
        case .tarEntryReadFailed: String(localized: "tar 第 \(a) 个条目读取失败：\(b)")
        case .notTarFormat: String(localized: "\(a) 不是 tar 格式")
        case .zipTooShort: String(localized: "文件太短，不是 zip")
        case .zipTailReadFailed: String(localized: "读取文件尾部失败")
        case .eocdNotFound: String(localized: "找不到中央目录结束记录（EOCD）")
        case .zip64LocatorMissing: String(localized: "字段溢出但找不到 zip64 定位记录")
        case .zip64EocdBad: String(localized: "zip64 中央目录结束记录缺失或损坏")
        case .cdHeaderIncomplete: String(localized: "中央目录头不完整")
        case .cdNameIncomplete: String(localized: "中央目录条目名不完整")
        case .cdExtraIncomplete: String(localized: "中央目录扩展字段不完整")
        case .cdCommentIncomplete: String(localized: "中央目录条目注释不完整")
        case .zip64MissingSize: String(localized: "zip64 扩展字段缺少原始大小")
        case .zip64MissingPacked: String(localized: "zip64 扩展字段缺少压缩后大小")
        case .zip64MissingOffset: String(localized: "zip64 扩展字段缺少本地头偏移")
        case .zip64MissingDisk: String(localized: "zip64 扩展字段缺少盘号")
        case .cdOutOfRange: String(localized: "中央目录越界")
        case .cdMisplaced: String(localized: "中央目录位置不对，读不到任何条目")
        case .offsetOverflow: String(localized: "偏移溢出")
        case .diskMissing: String(localized: "引用了不存在的分卷 #\(a)")
        case .recordTruncated: String(localized: "记录不完整")
        case .localHeaderBadSignature: String(localized: "本地文件头签名不对")
        case .localHeaderOffsetOutOfRange: String(localized: "本地文件头偏移 \(a) 越界")
        case .localHeaderIncomplete: String(localized: "本地文件头不完整")
        case .localHeaderOutOfRange: String(localized: "本地文件头越界")
        case .entryDataOutOfRange: String(localized: "条目数据越界：\(a)")
        case .dataTruncated: String(localized: "数据提前结束：\(a)")
        case .crcMismatch: String(localized: "CRC 不匹配：\(a)（期望 \(b)，实际 \(c)）")
        case .repairNoEntries: String(localized: "扫描不到任何完整条目")
        case .pkwareStrongEncryption: String(localized: "PKWARE 强加密")
        case .aesExtraMissing: String(localized: "AES 条目缺少 0x9901 扩展字段")
        case .aesKeyLength: String(localized: "AES 密钥长度错误")
        case .aesStrength: String(localized: "AES 强度 \(a)")
        case .hmacKeyLength: String(localized: "HMAC 密钥长度错误")
        case .aesEntryTooShort: String(localized: "AES 条目长度不足")
        case .aesSaltIncomplete: String(localized: "AES 盐不完整")
        case .aesVerifierIncomplete: String(localized: "AES 口令校验值不完整")
        case .aesCiphertextTruncated: String(localized: "AES 密文提前结束")
        case .aesAuthCodeMissing: String(localized: "AES 认证码缺失")
        case .aesAuthFailed: String(localized: "AES 认证失败：数据被改动或密码错误")
        case .zipCryptoHeaderIncomplete: String(localized: "ZipCrypto 加密头不完整")
        case .volumeTotalOverflow: String(localized: "分卷总长溢出")
        case .volumeCountInvalid: String(localized: "分卷数非法：\(a)")
        case .volumeMissing: String(localized: "缺少分卷：\(a)")
        case .streamHeaderIncomplete: String(localized: "\(a)：流头部不完整")
        case .lzmaPropsInvalid: String(localized: "LZMA 属性字节非法")
        case .lzmaDictTooLarge: String(localized: "LZMA 字典过大：\(a) MB")
        case .lzmaPropsLength: String(localized: "LZMA 属性长度非法：\(a)")
        case .lzmaPropsShort: String(localized: "LZMA 属性不足 5 字节")
        case .lzma2PropsMissing: String(localized: "LZMA2 缺少属性字节")
        case .lzma2DictTooLarge: String(localized: "LZMA2 字典过大")
        case .ppmdRestoreMode: String(localized: "PPMd 恢复模式 \(a)")
        case .ppmdParamsInvalid: String(localized: "PPMd 参数非法：\(a)")
        case .ppmdMemTooLarge: String(localized: "PPMd 内存需求过大：\(a) MB")
        case .ppmdPropsShort: String(localized: "PPMd 属性不足 5 字节")
        }
    }

    /// 第 `i` 个参数，缺了给空串（和核心 Display 的防御一致）。
    private func arg(_ i: Int) -> String {
        i < args.count ? args[i] : ""
    }
}
