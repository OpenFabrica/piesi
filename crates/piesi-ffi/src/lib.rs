//! UniFFI 导出层：只放导出函数和跨语言的 record/enum，不放逻辑。

uniffi::setup_scaffolding!();

/// 引擎版本号，给 About 页和 P0 的 FFI 连通性验证用。
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// 识别文件格式名；认不出返回 None。P0 用它验证从 Swift 调 Rust 的整条链路。
#[uniffi::export]
pub fn detect_format_name(path: String) -> Option<String> {
    piesi_core::detect_format(std::path::Path::new(&path))
        .ok()
        .flatten()
        .map(|f| f.display_name().to_owned())
}
