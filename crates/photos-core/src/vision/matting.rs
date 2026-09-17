//! 抠图后处理：概率 mask 阈值化 → 形态学开运算去噪 → 边缘羽化（高斯模糊）。

use image::{GrayImage, Luma};
use imageproc::distance_transform::Norm;
use imageproc::morphology::open;

/// 概率 mask（[0,255] 灰度）阈值化得到二值 mask
pub fn threshold_mask(mask: &GrayImage, threshold: u8) -> GrayImage {
    let mut out = GrayImage::new(mask.width(), mask.height());
    for (x, y, p) in mask.enumerate_pixels() {
        let v = if p[0] >= threshold { 255u8 } else { 0u8 };
        out.put_pixel(x, y, Luma([v]));
    }
    out
}

/// 形态学开运算（先腐蚀后膨胀），去除孤立噪点（半径 ≥ 1）
pub fn morph_open(img: &GrayImage, radius: u32) -> GrayImage {
    let k = radius.clamp(1, u8::MAX as u32) as u8;
    open(img, Norm::LInf, k)
}

/// 边缘羽化：高斯模糊，将硬边 mask 过渡平滑（sigma > 0 时生效）
pub fn feather(img: &GrayImage, sigma: f32) -> GrayImage {
    if sigma <= 0.0 {
        return img.clone();
    }
    image::imageops::blur(img, sigma)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(px: &[u8], w: u32, h: u32) -> GrayImage {
        GrayImage::from_raw(w, h, px.to_vec()).unwrap()
    }

    #[test]
    fn 阈值化边界() {
        let m = gray(&[127, 128, 255, 0], 2, 2);
        let t = threshold_mask(&m, 128);
        let vals: Vec<u8> = t.pixels().map(|p| p[0]).collect();
        assert_eq!(vals, vec![0, 255, 255, 0]);
    }

    #[test]
    fn 开运算去除孤立噪点() {
        // 6x6：中心 4x4 块 + 两角孤立点
        let px = vec![
            255, 0, 0, 0, 0, 0,
            0, 255, 255, 255, 255, 0,
            0, 255, 255, 255, 255, 0,
            0, 255, 255, 255, 255, 0,
            0, 255, 255, 255, 255, 0,
            0, 0, 0, 0, 0, 255,
        ];
        let m = gray(&px, 6, 6);
        let o = morph_open(&m, 1);
        let vals: Vec<u8> = o.pixels().map(|p| p[0]).collect();
        // 两角孤立点被去除
        assert_eq!(vals[0], 0);
        assert_eq!(vals[35], 0);
        // 中心 2x2（(2,2)~(3,3)）保留
        assert_eq!(vals[2 * 6 + 2], 255);
        assert_eq!(vals[2 * 6 + 3], 255);
        assert_eq!(vals[3 * 6 + 2], 255);
        assert_eq!(vals[3 * 6 + 3], 255);
    }

    #[test]
    fn 羽化非零sigma不改变尺寸() {
        let m = gray(&[0u8; 16], 4, 4);
        let f = feather(&m, 1.5);
        assert_eq!(f.dimensions(), (4, 4));
        // 全 0 mask 羽化后仍接近 0
        assert!(f.pixels().all(|p| p[0] < 10));
        assert_eq!(feather(&m, 0.0), m);
    }
}
