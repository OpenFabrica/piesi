//! 生成 Swift/C# 绑定的命令行入口，等价于安装 uniffi-bindgen，但版本跟随 workspace。
fn main() {
    uniffi::uniffi_bindgen_main()
}
