//! 证件照裁剪：基于人脸框计算「头顶留白 + 下巴余量 + 目标宽高比」的裁剪框（人脸水平居中）。

use crate::error::{CoreError, CoreResult};
use crate::vision::face::FaceBox;

/// 裁剪框（原图坐标）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// 计算证件照裁剪框。
///
/// - `top_ratio`：头顶上方留白 = top_ratio × 脸高
/// - `bottom_ratio`：下巴下方余量 = bottom_ratio × 脸高
/// - 裁剪框宽高比固定为目标比例；水平以人脸中心居中；越界钳制后保持比例。
pub fn compute_crop(
    face: &FaceBox,
    img_w: u32,
    img_h: u32,
    target_w: u32,
    target_h: u32,
    top_ratio: f64,
    bottom_ratio: f64,
) -> CoreResult<CropRect> {
    if img_w == 0 || img_h == 0 || target_w == 0 || target_h == 0 {
        return Err(CoreError::Image("裁剪参数不能为零".into()));
    }
    let hw = face.width() as f64;
    let hh = face.height() as f64;
    if hw <= 0.0 || hh <= 0.0 {
        return Err(CoreError::Image("人脸框尺寸非法".into()));
    }
    let ratio = target_w as f64 / target_h as f64;

    let top = (face.y1 as f64 - top_ratio * hh).max(0.0);
    let need_h = ((face.y2 as f64 + bottom_ratio * hh).min(img_h as f64) - top).max(1.0);

    // 宽按比例 = need_h × ratio；超图宽则钳制后等比重算高度
    let w = (need_h * ratio).min(img_w as f64);
    let cx = (face.x1 + face.x2) as f64 / 2.0;
    let mut x0 = (cx - w / 2.0).max(0.0);
    if x0 + w > img_w as f64 {
        x0 = (img_w as f64 - w).max(0.0);
    }
    let h = w / ratio; // 保持目标比例
    let y0 = top.min((img_h as f64 - h).max(0.0));

    Ok(CropRect {
        x: x0.round() as u32,
        y: y0.round() as u32,
        width: w.round() as u32,
        height: h.round() as u32,
    })
}

/// 裁剪并缩放到目标像素（兰索斯插值）
pub fn crop_resize(
    img: &image::RgbImage,
    rect: &CropRect,
    target_w: u32,
    target_h: u32,
) -> CoreResult<image::RgbImage> {
    if rect.width == 0 || rect.height == 0 || target_w == 0 || target_h == 0 {
        return Err(CoreError::Image("裁剪目标尺寸不能为零".into()));
    }
    // 手动逐像素拷贝裁剪区域（绕开 crop_imm 返回 SubImage 的 trait 限制）
    let mut cropped = image::RgbImage::new(rect.width, rect.height);
    for (dx, dy, p) in cropped.enumerate_pixels_mut() {
        *p = *img.get_pixel(rect.x + dx, rect.y + dy);
    }
    let resized = image::imageops::resize(&cropped, target_w, target_h, image::imageops::FilterType::Lanczos3);
    Ok(resized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 无钳制时比例与位置正确() {
        // 100x140 图，脸 (30,40)-(70,90)（w=40,h=50）
        let face = FaceBox { x1: 30.0, y1: 40.0, x2: 70.0, y2: 90.0, score: 0.99 };
        let r = compute_crop(&face, 100, 140, 295, 413, 0.2, 0.1).unwrap();
        let ratio = r.width as f64 / r.height as f64;
        assert!((ratio - 295.0 / 413.0).abs() < 0.01, "比例失真 {ratio}");
        // 包含人脸框
        assert!(r.x <= 30 && r.x + r.width >= 70);
        assert!(r.y <= 40 && r.y + r.height >= 90);
        // 头顶留白：top = 40 - 0.2*50 = 30
        assert_eq!(r.y, 30);
    }

    #[test]
    fn 超宽钳制保持比例() {
        // 极宽图导致按高度算的宽度超限 → 钳制宽度，比例保持
        let face = FaceBox { x1: 0.0, y1: 0.0, x2: 10.0, y2: 10.0, score: 0.9 };
        let r = compute_crop(&face, 50, 500, 1, 2, 0.0, 0.0).unwrap();
        let ratio = r.width as f64 / r.height as f64;
        assert!((ratio - 0.5).abs() < 0.01, "比例失真 {ratio}");
        assert!(r.width <= 50);
    }

    #[test]
    fn 参数非法报错() {
        let face = FaceBox { x1: 0.0, y1: 0.0, x2: 10.0, y2: 10.0, score: 0.9 };
        assert!(compute_crop(&face, 0, 100, 100, 100, 0.1, 0.1).is_err());
        let bad = FaceBox { x1: 5.0, y1: 5.0, x2: 5.0, y2: 5.0, score: 0.9 };
        assert!(compute_crop(&bad, 100, 100, 100, 100, 0.1, 0.1).is_err());
    }

    #[test]
    fn 裁剪缩放尺寸正确() {
        let img = image::RgbImage::from_pixel(100, 140, image::Rgb([5, 6, 7]));
        let face = FaceBox { x1: 30.0, y1: 40.0, x2: 70.0, y2: 90.0, score: 0.99 };
        let r = compute_crop(&face, 100, 140, 295, 413, 0.2, 0.1).unwrap();
        let out = crop_resize(&img, &r, 295, 413).unwrap();
        assert_eq!(out.dimensions(), (295, 413));
    }
}
