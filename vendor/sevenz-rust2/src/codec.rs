#[cfg(feature = "brotli")]
pub mod brotli;
/// Piesi 补丁：LZMA2 编解码走 liblzma
#[cfg(feature = "liblzma")]
pub(crate) mod liblzma;
#[cfg(feature = "lz4")]
pub mod lz4;
