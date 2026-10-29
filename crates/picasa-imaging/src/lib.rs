//! # picasa-imaging · 解码 / Orientation / 缩略图
//!
//! **M1 的范围红线**：本 crate 里**没有任何效果算子**（没有亮度、对比度、滤镜、裁切）。
//! M1 只做三件事：把文件解成像素、按 EXIF Orientation 摆正、缩成缩略图。
//!
//! 这是红线 R-2（编辑室入口不存在）在代码层面的体现：**M1 的依赖图里没有 `picasa-edit`**，
//! 也没有任何可以「顺手加个滤镜」的地方。

mod decode;
mod exif;
mod orient;
mod thumb;

pub use decode::{decode, decode_oriented, probe_dimensions};
pub use exif::{ExifSummary, probe};
pub use orient::{apply_orientation, orientation_swaps_axes};
pub use thumb::{
    ThumbImage, encode_jpeg, flatten_on_white, oriented_size, resize_to_fit, thumb_from_file,
    thumb_t0,
};

/// 解码器版本。**任何影响像素输出的改动都必须 +1**，否则 10 万张的旧缓存不会失效（02 §5.3）。
pub const DECODER_VER: i64 = 1;

/// T0（栅格档）缩略图长边。取 512：三档单元格最大 200pt，2x Retina 下需要 400px，
/// 留出余量给「大档 + 用户放大系统缩放」的情况，同时保证一张缩略图 < 40KB。
pub const THUMB_T0: u32 = 512;

/// T1（查看器档）长边。查看器「适应窗口」用的是它，100% 时才回源解码原图。
pub const THUMB_T1: u32 = 2048;

/// 本 crate 的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] image::ImageError),
    #[error("不支持的格式：{0}")]
    Unsupported(String),
    #[error("exif: {0}")]
    Exif(String),
}

pub type Result<T> = std::result::Result<T, ImageError>;
