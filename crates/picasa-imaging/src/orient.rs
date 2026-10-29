//! EXIF Orientation：8 个取值全部实现，含 5/7 两种镜像（F2-17 验收要点）。
//!
//! 映射表与 PIL `ImageOps.exif_transpose` / libexif 一致：
//!
//! | 取值 | 含义 | 实现 |
//! | --- | --- | --- |
//! | 1 | 正常 | 恒等 |
//! | 2 | 水平镜像 | `flip_horizontal` |
//! | 3 | 旋转 180° | `rotate180` |
//! | 4 | 垂直镜像 | `flip_vertical` |
//! | 5 | 主对角线镜像（transpose） | `rotate90` → `flip_horizontal` |
//! | 6 | 顺时针 90° | `rotate90` |
//! | 7 | 副对角线镜像（transverse） | `rotate90` → `flip_vertical` |
//! | 8 | 逆时针 90° | `rotate270` |
//!
//! 注意 `image` crate 的 `rotate90` 是**顺时针**。

use image::{RgbaImage, imageops};

/// 该 orientation 是否会交换宽高（用于布局预估，不必真的转一遍）。
pub fn orientation_swaps_axes(orientation: u32) -> bool {
    matches!(orientation, 5..=8)
}

pub fn apply_orientation(img: RgbaImage, orientation: u32) -> RgbaImage {
    match orientation {
        1 | 0 => img,
        2 => imageops::flip_horizontal(&img),
        3 => imageops::rotate180(&img),
        4 => imageops::flip_vertical(&img),
        5 => imageops::flip_horizontal(&imageops::rotate90(&img)),
        6 => imageops::rotate90(&img),
        7 => imageops::flip_vertical(&imageops::rotate90(&img)),
        8 => imageops::rotate270(&img),
        // 未知取值（含 >8 的损坏值）按「正常」处理，绝不猜测——
        // 猜错方向的代价比不转更大（用户会以为照片被弄坏了）。
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// 3×2 的编号图：每像素的 R 通道存一个唯一 id，方便断言位置。
    fn fixture() -> RgbaImage {
        let mut img = RgbaImage::new(3, 2);
        let mut id = 0u8;
        for y in 0..2 {
            for x in 0..3 {
                img.put_pixel(x, y, Rgba([id, 0, 0, 255]));
                id += 1;
            }
        }
        img
    }

    fn ids(img: &RgbaImage) -> Vec<Vec<u8>> {
        (0..img.height())
            .map(|y| (0..img.width()).map(|x| img.get_pixel(x, y)[0]).collect())
            .collect()
    }

    #[test]
    fn identity_keeps_everything() {
        assert_eq!(
            ids(&apply_orientation(fixture(), 1)),
            vec![vec![0, 1, 2], vec![3, 4, 5]]
        );
    }

    #[test]
    fn horizontal_flip() {
        // 2: 左右翻
        assert_eq!(
            ids(&apply_orientation(fixture(), 2)),
            vec![vec![2, 1, 0], vec![5, 4, 3]]
        );
    }

    #[test]
    fn rotate_180() {
        assert_eq!(
            ids(&apply_orientation(fixture(), 3)),
            vec![vec![5, 4, 3], vec![2, 1, 0]]
        );
    }

    #[test]
    fn vertical_flip() {
        assert_eq!(
            ids(&apply_orientation(fixture(), 4)),
            vec![vec![3, 4, 5], vec![0, 1, 2]]
        );
    }

    #[test]
    fn rotate_90_cw() {
        // 6：顺时针 90°。3×2 → 2×3
        let out = apply_orientation(fixture(), 6);
        assert_eq!((out.width(), out.height()), (2, 3));
        assert_eq!(ids(&out), vec![vec![3, 0], vec![4, 1], vec![5, 2]]);
    }

    #[test]
    fn rotate_270_cw() {
        // 8：逆时针 90°
        let out = apply_orientation(fixture(), 8);
        assert_eq!((out.width(), out.height()), (2, 3));
        assert_eq!(ids(&out), vec![vec![2, 5], vec![1, 4], vec![0, 3]]);
    }

    #[test]
    fn transpose_and_transverse() {
        // 5：主对角线镜像。原 (x,y) 的新位置是 (y,x)
        let out = apply_orientation(fixture(), 5);
        assert_eq!((out.width(), out.height()), (2, 3));
        assert_eq!(ids(&out), vec![vec![0, 3], vec![1, 4], vec![2, 5]]);

        // 7：副对角线镜像
        let out = apply_orientation(fixture(), 7);
        assert_eq!((out.width(), out.height()), (2, 3));
        assert_eq!(ids(&out), vec![vec![5, 2], vec![4, 1], vec![3, 0]]);
    }

    #[test]
    fn unknown_orientation_is_identity_not_guess() {
        let img = apply_orientation(fixture(), 99);
        assert_eq!(ids(&img), vec![vec![0, 1, 2], vec![3, 4, 5]]);
    }

    #[test]
    fn applying_four_rotations_returns_to_original() {
        let mut img = fixture();
        for _ in 0..4 {
            img = apply_orientation(img, 6);
        }
        assert_eq!(ids(&img), vec![vec![0, 1, 2], vec![3, 4, 5]]);
    }
}
