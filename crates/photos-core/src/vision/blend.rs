//! alpha 混合换底色：out = fg × alpha + bg × (1 - alpha)。
//!
//! 含抠图质量增强算子：边缘去色边（颜色解混）、自定义背景图合成、透明底 RGBA 生成。

use image::{GrayImage, Rgb, RgbImage, Rgba, RgbaImage};
use imageproc::distance_transform::Norm;
use imageproc::morphology::{dilate, erode};

/// 去色边生效的 alpha 下界（低于此值前景几乎不可见，解混会放大噪声）
pub const DECONTAMINATE_MIN_ALPHA: u8 = 26;
/// 去色边生效的 alpha 上界（达到即视为纯前景，无需解混）
pub const DECONTAMINATE_MAX_ALPHA: u8 = 250;

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

/// 估计背景色：取紧邻前景（掩膜扩张半径 2 内）的透明像素 RGB 均值。
///
/// 邻近前景的透明像素才是边缘渗色的来源；远离前景的区域可能是纠偏旋转填充的
/// 黑色画布，直接全图取样会污染估计。无邻近透明像素时退化为全图透明像素均值，
/// 仍无样本时返回白色。
pub fn estimate_background_color(fg: &RgbImage, alpha: &GrayImage) -> [u8; 3] {
    assert_eq!(
        fg.dimensions(),
        alpha.dimensions(),
        "前景与 alpha 尺寸必须一致"
    );
    // 前景二值图 → 扩张，得到「前景邻域」区域
    let fg_bin = super::matting::threshold_mask(alpha, 1);
    let near_fg = dilate(&fg_bin, Norm::LInf, 2);
    let mut near = [0u64; 3];
    let mut near_n = 0u64;
    let mut all = [0u64; 3];
    let mut all_n = 0u64;
    for (x, y, p) in fg.enumerate_pixels() {
        if alpha.get_pixel(x, y)[0] != 0 {
            continue;
        }
        all[0] += p[0] as u64;
        all[1] += p[1] as u64;
        all[2] += p[2] as u64;
        all_n += 1;
        if near_fg.get_pixel(x, y)[0] > 0 {
            near[0] += p[0] as u64;
            near[1] += p[1] as u64;
            near[2] += p[2] as u64;
            near_n += 1;
        }
    }
    let (s, c) = if near_n > 0 {
        (near, near_n)
    } else {
        (all, all_n)
    };
    if c == 0 {
        return [255, 255, 255];
    }
    [(s[0] / c) as u8, (s[1] / c) as u8, (s[2] / c) as u8]
}

/// 边缘去色边（decontamination）：对半透明边缘像素做颜色解混，抑制白边/黑边/底色残留。
///
/// 解混模型：观测色 `C = F×α + B×(1-α)` → 前景色 `F = (C - B×(1-α)) / α`，
/// 其中 `B` 为 [`estimate_background_color`] 估计的原始背景色。
/// `trimap_radius` 给出「内部核心」半径：距骨架边界超过该半径的主体像素保持原样，
/// 避免薄纱、发内层等主体内部半透明像素被误解混；alpha 过高（纯前景）或过低
/// （几乎不可见）的像素同样跳过，避免噪声放大。
pub fn decontaminate(fg: &RgbImage, alpha: &GrayImage, trimap_radius: u32) -> RgbImage {
    assert_eq!(
        fg.dimensions(),
        alpha.dimensions(),
        "前景与 alpha 尺寸必须一致"
    );
    let bg = estimate_background_color(fg, alpha);
    // trimap：腐蚀出内部核心（距边界 > trimap_radius 的主体区域），核心内不做解混
    let bin = super::matting::threshold_mask(alpha, 1);
    let core = erode(
        &bin,
        Norm::LInf,
        trimap_radius.clamp(1, u8::MAX as u32) as u8,
    );
    let mut out = fg.clone();
    for (x, y, p) in fg.enumerate_pixels() {
        if core.get_pixel(x, y)[0] > 0 {
            continue;
        }
        let a = alpha.get_pixel(x, y)[0];
        if !(DECONTAMINATE_MIN_ALPHA..DECONTAMINATE_MAX_ALPHA).contains(&a) {
            continue;
        }
        let af = a as f32 / 255.0;
        let mut q = [0u8; 3];
        for c in 0..3 {
            let v = (p[c] as f32 - bg[c] as f32 * (1.0 - af)) / af;
            q[c] = v.round().clamp(0.0, 255.0) as u8;
        }
        out.put_pixel(x, y, Rgb(q));
    }
    out
}

/// 背景图按 cover 语义缩放（等比放大至铺满）并居中裁切到目标尺寸
pub fn fit_cover(src: &RgbImage, target_w: u32, target_h: u32) -> RgbImage {
    let (sw, sh) = src.dimensions();
    if sw == 0 || sh == 0 || target_w == 0 || target_h == 0 {
        return RgbImage::new(target_w.max(1), target_h.max(1));
    }
    let scale = (target_w as f64 / sw as f64).max(target_h as f64 / sh as f64);
    let nw = ((sw as f64 * scale).round() as u32).max(target_w);
    let nh = ((sh as f64 * scale).round() as u32).max(target_h);
    let scaled = image::imageops::resize(src, nw, nh, image::imageops::FilterType::Triangle);
    let ox = (nw - target_w) / 2;
    let oy = (nh - target_h) / 2;
    let mut out = RgbImage::new(target_w, target_h);
    for (dx, dy, p) in out.enumerate_pixels_mut() {
        *p = *scaled.get_pixel(ox + dx, oy + dy);
    }
    out
}

/// 以背景图混合（前景与背景图尺寸必须一致）：out = fg × α + bg_img × (1 - α)
pub fn composite_with_image(fg: &RgbImage, alpha: &GrayImage, bg_img: &RgbImage) -> RgbImage {
    assert_eq!(
        fg.dimensions(),
        alpha.dimensions(),
        "前景与 alpha 尺寸必须一致"
    );
    assert_eq!(
        fg.dimensions(),
        bg_img.dimensions(),
        "前景与背景图尺寸必须一致"
    );
    let (w, h) = fg.dimensions();
    let mut out = RgbImage::new(w, h);
    for (x, y, px) in fg.enumerate_pixels() {
        let a = alpha.get_pixel(x, y)[0] as f32 / 255.0;
        let b = bg_img.get_pixel(x, y);
        out.put_pixel(
            x,
            y,
            Rgb([
                (px[0] as f32 * a + b[0] as f32 * (1.0 - a)).round() as u8,
                (px[1] as f32 * a + b[1] as f32 * (1.0 - a)).round() as u8,
                (px[2] as f32 * a + b[2] as f32 * (1.0 - a)).round() as u8,
            ]),
        );
    }
    out
}

/// 生成透明底 RGBA（RGB 取前景色，alpha 取羽化掩膜），供 PNG 输出
pub fn to_rgba(fg: &RgbImage, alpha: &GrayImage) -> RgbaImage {
    assert_eq!(
        fg.dimensions(),
        alpha.dimensions(),
        "前景与 alpha 尺寸必须一致"
    );
    let (w, h) = fg.dimensions();
    let mut out = RgbaImage::new(w, h);
    for (x, y, p) in fg.enumerate_pixels() {
        out.put_pixel(x, y, Rgba([p[0], p[1], p[2], alpha.get_pixel(x, y)[0]]));
    }
    out
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

    #[test]
    fn 背景色估计取前景邻域透明像素() {
        // 2x1：左侧 alpha=0（背景色），右侧 alpha=255（前景）→ 左侧属于前景邻域
        let fg = RgbImage::from_raw(2, 1, vec![200, 100, 50, 10, 20, 30]).unwrap();
        let alpha = GrayImage::from_raw(2, 1, vec![0u8, 255]).unwrap();
        assert_eq!(estimate_background_color(&fg, &alpha), [200, 100, 50]);
        // 全不透明：无背景样本 → 白色兜底
        let full = GrayImage::from_pixel(2, 1, Luma([255u8]));
        assert_eq!(estimate_background_color(&fg, &full), [255, 255, 255]);
    }

    #[test]
    fn 去色边还原被背景污染的前景色() {
        // 逐像素构造：背景 (200,100,50)，前景纯黑，右像素 α=128 → 观测色 = 前景×0.502 + 背景×0.498
        let af = 128f32 / 255.0;
        let bg = [200f32, 100.0, 50.0];
        let observed = [
            (0.0 * af + bg[0] * (1.0 - af)).round() as u8,
            (0.0 * af + bg[1] * (1.0 - af)).round() as u8,
            (0.0 * af + bg[2] * (1.0 - af)).round() as u8,
        ];
        let fg = RgbImage::from_raw(
            2,
            1,
            vec![200, 100, 50, observed[0], observed[1], observed[2]],
        )
        .unwrap();
        let alpha = GrayImage::from_raw(2, 1, vec![0u8, 128]).unwrap();
        let clean = decontaminate(&fg, &alpha, 2);
        // 全透明像素保持原样
        assert_eq!(clean.get_pixel(0, 0), &Rgb([200, 100, 50]));
        // 半透明像素解混回接近纯黑（去白边）
        let p = clean.get_pixel(1, 0);
        assert!(
            p[0] < 10 && p[1] < 10 && p[2] < 10,
            "解混结果应接近纯黑，实际 {p:?}"
        );
        // 纯前景像素不被改动
        let fg2 = RgbImage::from_pixel(1, 1, Rgb([12, 34, 56]));
        let a2 = GrayImage::from_pixel(1, 1, Luma([255u8]));
        assert_eq!(
            decontaminate(&fg2, &a2, 2).get_pixel(0, 0),
            &Rgb([12, 34, 56])
        );
    }

    #[test]
    fn 去色边跳过主体内部核心() {
        // 7x1：α=[0,0,128,128,128,255,255]，腐蚀半径 2 后仅 x=4 位于内部核心
        let fg = RgbImage::from_raw(
            7,
            1,
            vec![
                200, 100, 50, 200, 100, 50, 10, 10, 10, 10, 10, 10, 120, 120, 120, 10, 10, 10, 10,
                10, 10,
            ],
        )
        .unwrap();
        let alpha = GrayImage::from_raw(7, 1, vec![0u8, 0, 128, 128, 128, 255, 255]).unwrap();
        let clean = decontaminate(&fg, &alpha, 2);
        assert_eq!(
            clean.get_pixel(4, 0),
            &Rgb([120, 120, 120]),
            "主体内部核心不应被解混"
        );
        assert_ne!(
            clean.get_pixel(3, 0),
            &Rgb([10, 10, 10]),
            "过渡带半透明像素应被解混"
        );
    }

    #[test]
    fn 背景图按cover缩放并居中裁切() {
        // 4x2 → 2x2：等比缩放后裁去左右
        let src = RgbImage::from_pixel(4, 2, Rgb([1, 2, 3]));
        assert_eq!(fit_cover(&src, 2, 2).dimensions(), (2, 2));
        // 2x4 → 4x4：等比放大后裁去上下
        let src2 = RgbImage::from_pixel(2, 4, Rgb([9, 8, 7]));
        let out = fit_cover(&src2, 4, 4);
        assert_eq!(out.dimensions(), (4, 4));
        assert_eq!(out.get_pixel(0, 0), &Rgb([9, 8, 7]));
        // 尺寸为零不 panic
        assert_eq!(fit_cover(&src, 0, 0).dimensions(), (1, 1));
    }

    #[test]
    fn 背景图合成按alpha取值() {
        let fg = RgbImage::from_pixel(2, 1, Rgb([10, 20, 30]));
        let alpha = GrayImage::from_raw(2, 1, vec![0u8, 255]).unwrap();
        let bg = RgbImage::from_pixel(2, 1, Rgb([200, 100, 50]));
        let out = composite_with_image(&fg, &alpha, &bg);
        assert_eq!(out.get_pixel(0, 0), &Rgb([200, 100, 50]));
        assert_eq!(out.get_pixel(1, 0), &Rgb([10, 20, 30]));
    }

    #[test]
    fn 透明底rgba携带alpha通道() {
        let fg = RgbImage::from_pixel(2, 1, Rgb([11, 22, 33]));
        let alpha = GrayImage::from_raw(2, 1, vec![0u8, 255]).unwrap();
        let rgba = to_rgba(&fg, &alpha);
        assert_eq!(rgba.dimensions(), (2, 1));
        assert_eq!(rgba.get_pixel(0, 0), &Rgba([11, 22, 33, 0]));
        assert_eq!(rgba.get_pixel(1, 0), &Rgba([11, 22, 33, 255]));
    }
}
