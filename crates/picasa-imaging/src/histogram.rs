//! 直方图（M4 用户功能 #11：查看器余项）。
//!
//! 在**缩小后的**查看器纹理源图上算（2560 长边以内），RGB 三通道各 256 桶。
//! 每第 N 个像素采样一次把循环压到 ~4 万次，绘制端画三条半透明折线。

/// RGB 直方图。`r/g/b` 各 256 桶，值为归一化后的像素计数（最大桶 = 1.0）。
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub r: [f32; 256],
    pub g: [f32; 256],
    pub b: [f32; 256],
    /// 像素总数（采样前）。
    pub pixels: u64,
}

/// 从 RGBA8 图算直方图。`img` 建议是已缩小的查看器纹理源。
pub fn histogram(img: &image::RgbaImage) -> Histogram {
    let (w, h) = (img.width() as u64, img.height() as u64);
    let mut hist = Histogram {
        r: [0.0; 256],
        g: [0.0; 256],
        b: [0.0; 256],
        pixels: w * h,
    };
    // 采样步长：让采样点数 ≈ 512²，小图全采样
    let total = (w * h).max(1);
    let step = ((total / (512 * 512)).max(1) as f64).sqrt().max(1.0) as u32;
    let mut counts = [[0u64; 256]; 3];
    let mut sampled = 0u64;
    for y in (0..img.height()).step_by(step as usize) {
        for x in (0..img.width()).step_by(step as usize) {
            let p = img.get_pixel(x, y);
            counts[0][p[0] as usize] += 1;
            counts[1][p[1] as usize] += 1;
            counts[2][p[2] as usize] += 1;
            sampled += 1;
        }
    }
    let max = counts
        .iter()
        .flat_map(|c| c.iter())
        .copied()
        .fold(1u64, u64::max);
    let norm = 1.0 / max as f32;
    for (i, slot) in hist.r.iter_mut().enumerate() {
        *slot = counts[0][i] as f32 * norm;
        hist.g[i] = counts[1][i] as f32 * norm;
        hist.b[i] = counts[2][i] as f32 * norm;
    }
    let _ = sampled;
    hist
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_white_image_peaks_at_255() {
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 255, 255, 255]));
        let h = histogram(&img);
        assert!((h.r[255] - 1.0).abs() < 1e-6, "纯白图的 R 峰必须在 255: {}", h.r[255]);
        assert!((h.g[255] - 1.0).abs() < 1e-6);
        assert!((h.b[255] - 1.0).abs() < 1e-6);
        assert!(h.r[0] < 0.01);
    }

    #[test]
    fn solid_black_image_peaks_at_0() {
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 255]));
        let h = histogram(&img);
        assert!((h.r[0] - 1.0).abs() < 1e-6);
        assert!(h.r[255] < 0.01);
        assert_eq!(h.pixels, 64 * 64);
    }

    #[test]
    fn half_black_half_white_splits_two_peaks() {
        let mut img = image::RgbaImage::new(128, 64);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < 64 {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            };
        }
        let h = histogram(&img);
        // 两个峰都要显著（采样步长在 8k 像素上是全采样）
        assert!(h.r[0] > 0.3, "黑峰: {}", h.r[0]);
        assert!(h.r[255] > 0.3, "白峰: {}", h.r[255]);
    }
}
