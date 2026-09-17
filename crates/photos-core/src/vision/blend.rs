//! alpha 混合换底色：out = fg × alpha + bg × (1 - alpha)。

use image::{GrayImage, Rgb, RgbImage};

/// 逐像素 alpha 混合（alpha 为 [0,255] 灰度，先羽化再混合可获得平滑边缘）
pub fn composite(fg: &RgbImage, alpha: &GrayImage, bg: [u8; 3]) -> RgbImage {
    assert_eq!(
        fg.dimensions(),
        alpha.dimensions(),
        "前景与 alpha 尺寸必须一致"
    );
    let (w, h) = fg.dimensions();
    let mut out = RgbImage::new(w, h);
    for (x, y, px) in fg.enumerate_pixels() {
        let a = alpha.get_pixel(x, y)[0] as f32 / 255.0;
        let r = px[0] as f32 * a + bg[0] as f32 * (1.0 - a);
        let g = px[1] as f32 * a + bg[1] as f32 * (1.0 - a);
        let b = px[2] as f32 * a + bg[2] as f32 * (1.0 - a);
        out.put_pixel(
            x,
            y,
            Rgb([r.round() as u8, g.round() as u8, b.round() as u8]),
        );
    }
    out
}

/// 羽化后混合：对 mask 高斯模糊（sigma）再混合
pub fn composite_feathered(fg: &RgbImage, mask: &GrayImage, bg: [u8; 3], sigma: f32) -> RgbImage {
    let alpha = super::matting::feather(mask, sigma);
    composite(fg, &alpha, bg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    #[test]
    fn 全透明换为背景色() {
        let fg = RgbImage::from_pixel(2, 2, Rgb([10, 20, 30]));
        let alpha = GrayImage::from_pixel(2, 2, Luma([0u8]));
        let out = composite(&fg, &alpha, [200, 100, 50]);
        assert_eq!(out.get_pixel(0, 0), &Rgb([200, 100, 50]));
    }

    #[test]
    fn 全不透明保留前景() {
        let fg = RgbImage::from_pixel(2, 2, Rgb([10, 20, 30]));
        let alpha = GrayImage::from_pixel(2, 2, Luma([255u8]));
        let out = composite(&fg, &alpha, [200, 100, 50]);
        assert_eq!(out.get_pixel(1, 1), &Rgb([10, 20, 30]));
    }

    #[test]
    fn 半透明为插值() {
        let fg = RgbImage::from_pixel(1, 1, Rgb([0, 0, 0]));
        let alpha = GrayImage::from_pixel(1, 1, Luma([128u8]));
        let out = composite(&fg, &alpha, [200, 100, 0]);
        // 0*0.502 + 200*0.498 ≈ 100（四舍五入）
        let p = out.get_pixel(0, 0);
        assert!((p[0] as i32 - 100).abs() <= 1, "实际 {}", p[0]);
        assert!((p[1] as i32 - 50).abs() <= 1);
    }
}
