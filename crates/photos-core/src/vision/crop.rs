//! 证件照裁剪：复刻 HivisionIDPhotos `adjust_photo` 的两轮构图。
//!
//! 第一轮（几何定位）：裁剪面积 = 脸面积 / `HEAD_MEASURE_RATIO`（即 5×脸面积），
//! 脸中心置于裁剪框 `HEAD_HEIGHT_RATIO`（45%）高度处，人脸水平居中；
//! 第二轮（人像边界修正，需 alpha 掩膜）：左右贴边等比收缩、头顶距顶部
//! [10%,12%] 两轮调整、底部贴底——与 Hivision 的 `get_box` / `detect_distance` / `move` 等价。

use crate::error::{CoreError, CoreResult};
use crate::vision::face::FaceBox;
use image::GrayImage;

/// 裁剪框面积 = 脸面积 / head_measure_ratio（0.2 → 5×脸面积，Hivision `head_measure_ratio`）
pub const HEAD_MEASURE_RATIO: f64 = 0.2;
/// 脸中心在裁剪框中的垂直位置（占裁剪框高度，Hivision `head_height_ratio`）
pub const HEAD_HEIGHT_RATIO: f64 = 0.45;
/// 头顶距裁剪框顶部的上限（占裁剪框高度，超出则上移；Hivision `head_top_range` 上界）
pub const HEAD_TOP_MAX: f64 = 0.12;
/// 头顶距裁剪框顶部的下限（占裁剪框高度，不足则下移；Hivision `head_top_range` 下界）
pub const HEAD_TOP_MIN: f64 = 0.10;
/// alpha 视为人像像素的存在阈值（过滤羽化过渡带噪声）
const PERSON_ALPHA_THRESHOLD: u8 = 8;

/// 裁剪框（原图坐标）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// 计算证件照裁剪框（两轮构图，见模块注释）。
///
/// - `alpha`：纠偏后坐标系的抠图掩膜（可选）；缺省时仅做第一轮几何定位
/// - 裁剪框宽高比固定为目标比例；越界时先等比缩小到图幅、再钳制平移。
pub fn compute_crop(
    face: &FaceBox,
    alpha: Option<&GrayImage>,
    img_w: u32,
    img_h: u32,
    target_w: u32,
    target_h: u32,
) -> CoreResult<CropRect> {
    if img_w == 0 || img_h == 0 || target_w == 0 || target_h == 0 {
        return Err(CoreError::Image("裁剪参数不能为零".into()));
    }
    let fw = face.width() as f64;
    let fh = face.height() as f64;
    if fw <= 0.0 || fh <= 0.0 {
        return Err(CoreError::Image("人脸框尺寸非法".into()));
    }

    // —— 第一轮：面积定标 + 脸心 45% 高度定位 ——
    let crop_area = fw * fh / HEAD_MEASURE_RATIO;
    let unit = (crop_area / (target_w as f64 * target_h as f64)).sqrt();
    let mut cw = target_w as f64 * unit;
    let mut ch = target_h as f64 * unit;
    // 裁剪框超出图幅时等比缩小（保持目标比例）
    let fit = (img_w as f64 / cw).min(img_h as f64 / ch).min(1.0);
    if fit < 1.0 {
        cw *= fit;
        ch *= fit;
    }
    let fcx = (face.x1 + face.x2) as f64 / 2.0;
    let fcy = (face.y1 + face.y2) as f64 / 2.0;
    let mut x0 = (fcx - cw / 2.0).clamp(0.0, (img_w as f64 - cw).max(0.0));
    let mut y0 = (fcy - ch * HEAD_HEIGHT_RATIO).clamp(0.0, (img_h as f64 - ch).max(0.0));

    // —— 第二轮：alpha 人像边界修正 ——
    if let Some((bx0, by0, bx1, by1, bottom_clipped)) =
        alpha.and_then(|a| person_bbox(a, x0, y0, cw, ch))
    {
        let left = (bx0 - x0).max(0.0);
        let right = (x0 + cw - bx1).max(0.0);
        if left > 0.0 || right > 0.0 {
            // 左右贴边等比收缩（Hivision cut_value_top）：宽减 (left+right)，
            // 高减 (left+right)×(高/宽)，保持目标宽高比
            let cut_top = (left + right) * ch / cw / 2.0;
            x0 += left;
            cw -= left + right;
            y0 += cut_top;
            ch -= 2.0 * cut_top;
        }
        if cw > 1.0 && ch > 1.0 {
            // 头顶距离修正（Hivision detect_distance）：距顶过大 → 下移裁剪框（人像上提），
            // 过小 → 上移裁剪框（留出头顶留白）；人像被裁顶时距离按 0 处理
            let head_top = (by0 - y0).max(0.0);
            if head_top > HEAD_TOP_MAX * ch {
                y0 += head_top - HEAD_TOP_MAX * ch;
            } else if head_top < HEAD_TOP_MIN * ch {
                y0 -= HEAD_TOP_MIN * ch - head_top;
            }
            // 底部贴底（Hivision move）：人像下方有空隙则上移裁剪框填满；
            // 人像底部被扫描窗截断（延伸出第一轮裁剪框）时视为无空隙，跳过
            if !bottom_clipped {
                let bottom_gap = y0 + ch - by1;
                if bottom_gap > 0.0 {
                    y0 -= bottom_gap;
                }
            }
        }
    }

    // 取整并钳制回图幅（保持比例；x 向下取整、宽高就近取整）
    let cw = cw.round().max(1.0).min(img_w as f64);
    let ch = ch.round().max(1.0).min(img_h as f64);
    let x0 = x0.floor().clamp(0.0, img_w as f64 - cw);
    let y0 = y0.floor().clamp(0.0, img_h as f64 - ch);
    Ok(CropRect {
        x: x0 as u32,
        y: y0 as u32,
        width: cw as u32,
        height: ch as u32,
    })
}

/// 人像边界框：alpha 掩膜在裁剪框区域内的前景范围（原图坐标，右下开区间）。
/// 返回值末位为「人像底部是否被扫描窗（第一轮裁剪框）截断」——截断时人像实际
/// 延伸出窗口，后续不能据此做底部贴底修正。
fn person_bbox(
    alpha: &GrayImage,
    x0: f64,
    y0: f64,
    cw: f64,
    ch: f64,
) -> Option<(f64, f64, f64, f64, bool)> {
    let (aw, ah) = alpha.dimensions();
    let (sx, sy) = (x0.max(0.0).floor() as u32, y0.max(0.0).floor() as u32);
    let ex = ((x0 + cw).ceil() as u32).min(aw);
    let ey = ((y0 + ch).ceil() as u32).min(ah);
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for y in sy..ey {
        for x in sx..ex {
            if alpha.get_pixel(x, y)[0] > PERSON_ALPHA_THRESHOLD {
                bbox = match bbox {
                    None => Some((x, y, x, y)),
                    Some((bx0, by0, bx1, by1)) => {
                        Some((bx0.min(x), by0.min(y), bx1.max(x), by1.max(y)))
                    }
                };
            }
        }
    }
    bbox.map(|(a, b, c, d)| {
        let bottom_clipped = d + 1 >= ey;
        (a as f64, b as f64, (c + 1) as f64, (d + 1) as f64, bottom_clipped)
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
    let resized = image::imageops::resize(
        &cropped,
        target_w,
        target_h,
        image::imageops::FilterType::Lanczos3,
    );
    Ok(resized)
}

/// 裁剪并缩放到目标像素（RGBA 版本，供透明底 PNG 输出使用）
pub fn crop_resize_rgba(
    img: &image::RgbaImage,
    rect: &CropRect,
    target_w: u32,
    target_h: u32,
) -> CoreResult<image::RgbaImage> {
    if rect.width == 0 || rect.height == 0 || target_w == 0 || target_h == 0 {
        return Err(CoreError::Image("裁剪目标尺寸不能为零".into()));
    }
    let mut cropped = image::RgbaImage::new(rect.width, rect.height);
    for (dx, dy, p) in cropped.enumerate_pixels_mut() {
        *p = *img.get_pixel(rect.x + dx, rect.y + dy);
    }
    Ok(image::imageops::resize(
        &cropped,
        target_w,
        target_h,
        image::imageops::FilterType::Lanczos3,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 无alpha时第一轮面积定标与脸心高度() {
        // 100x140 图，脸 (30,40)-(70,90)（40×50=2000）
        // 裁剪面积 = 2000/0.2 = 10000；unit = √(10000/(295×413)) ≈ 0.2865
        // → 裁剪框 ≈ 84.5×118.3，脸心 (50,65) 置于 45% 高度：y0 = 65 − 118.3×0.45 ≈ 11.8
        let face = FaceBox {
            x1: 30.0,
            y1: 40.0,
            x2: 70.0,
            y2: 90.0,
            score: 0.99,
        };
        let r = compute_crop(&face, None, 100, 140, 295, 413).unwrap();
        let ratio = r.width as f64 / r.height as f64;
        assert!((ratio - 295.0 / 413.0).abs() < 0.01, "比例失真 {ratio}");
        // 脸心垂直位置 ≈ 45% 裁剪框高度
        let fc_y = 65.0 - r.y as f64;
        let pos = fc_y / r.height as f64;
        assert!((pos - 0.45).abs() < 0.02, "脸心应在 45% 高度，实际 {pos}");
        // 脸心水平居中
        let fc_x = 50.0 - r.x as f64;
        assert!((fc_x / r.width as f64 - 0.5).abs() < 0.02, "脸心应水平居中");
    }

    #[test]
    fn 左右贴边等比收缩() {
        // 300x300 图，人像 bbox 明显窄于裁剪框 → 第二轮收缩宽度、头顶按比例修正
        let face = FaceBox {
            x1: 135.0,
            y1: 100.0,
            x2: 165.0,
            y2: 160.0,
            score: 0.99,
        };
        // 人像竖条 bbox：x∈[140,160]，y∈[20,280]
        let mut alpha = GrayImage::new(300, 300);
        for y in 20..280 {
            for x in 140..160 {
                alpha.put_pixel(x, y, image::Luma([255]));
            }
        }
        let r = compute_crop(&face, Some(&alpha), 300, 300, 295, 413).unwrap();
        let ratio = r.width as f64 / r.height as f64;
        assert!((ratio - 295.0 / 413.0).abs() < 0.01, "比例失真 {ratio}");
        // 收缩后人像左右应贴边（容差 2px）
        assert!(r.x as f64 <= 141.0, "左缘应贴人像 {r:?}");
        assert!(
            (r.x + r.width) as f64 >= 159.0,
            "右缘应贴人像 {r:?}"
        );
    }

    #[test]
    fn 头顶距离过近时上移留出头顶留白() {
        // 人像头顶距第一轮裁剪框顶部 < 10% → 裁剪框上移，最终头顶距离落在 [10%,12%] 附近
        let face = FaceBox {
            x1: 140.0,
            y1: 60.0,
            x2: 160.0,
            y2: 100.0,
            score: 0.99,
        };
        let mut alpha = GrayImage::new(300, 300);
        for y in 50..250 {
            for x in 100..200 {
                alpha.put_pixel(x, y, image::Luma([255]));
            }
        }
        let r = compute_crop(&face, Some(&alpha), 300, 300, 295, 413).unwrap();
        let head_top = 50.0 - r.y as f64;
        let pos = head_top / r.height as f64;
        assert!(
            (0.08..=0.14).contains(&pos),
            "头顶距离应约 10%~12%，实际 {pos}（{r:?}）"
        );
    }

    #[test]
    fn 头顶距离过远时人像上提() {
        // 人像头顶距裁剪框顶部 > 12% → 裁剪框下移，最终头顶距离回到约 12%
        let face = FaceBox {
            x1: 140.0,
            y1: 60.0,
            x2: 160.0,
            y2: 100.0,
            score: 0.99,
        };
        let mut alpha = GrayImage::new(300, 300);
        for y in 100..250 {
            for x in 100..200 {
                alpha.put_pixel(x, y, image::Luma([255]));
            }
        }
        let r = compute_crop(&face, Some(&alpha), 300, 300, 295, 413).unwrap();
        let head_top = 100.0 - r.y as f64;
        let pos = head_top / r.height as f64;
        assert!(
            (0.10..=0.14).contains(&pos),
            "头顶距离应约 12%，实际 {pos}（{r:?}）"
        );
    }

    #[test]
    fn 底部贴底() {
        // 人像底部之下有空隙 → 裁剪框上移使人像贴近底部（Hivision move）
        let face = FaceBox {
            x1: 140.0,
            y1: 60.0,
            x2: 160.0,
            y2: 100.0,
            score: 0.99,
        };
        let mut alpha = GrayImage::new(300, 300);
        // 人像集中在 y∈[60,120]，底部 180px 全空
        for y in 60..120 {
            for x in 100..200 {
                alpha.put_pixel(x, y, image::Luma([255]));
            }
        }
        let r = compute_crop(&face, Some(&alpha), 300, 300, 295, 413).unwrap();
        let bottom_gap = 120i32 - (r.y + r.height) as i32;
        assert!(
            bottom_gap.abs() <= 2,
            "人像应贴近裁剪框底部，空隙 {bottom_gap}px（{r:?}）"
        );
    }

    #[test]
    fn 超图幅等比缩小() {
        // 极小图幅：裁剪框超界 → 等比缩小并钳制，比例保持
        let face = FaceBox {
            x1: 0.0,
            y1: 0.0,
            x2: 10.0,
            y2: 10.0,
            score: 0.9,
        };
        let r = compute_crop(&face, None, 50, 40, 1, 2).unwrap();
        let ratio = r.width as f64 / r.height as f64;
        assert!((ratio - 0.5).abs() < 0.01, "比例失真 {ratio}");
        assert!(r.width <= 50 && r.height <= 40);
        assert!(r.x + r.width <= 50 && r.y + r.height <= 40);
    }

    #[test]
    fn 参数非法报错() {
        let face = FaceBox {
            x1: 0.0,
            y1: 0.0,
            x2: 10.0,
            y2: 10.0,
            score: 0.9,
        };
        assert!(compute_crop(&face, None, 0, 100, 100, 100).is_err());
        let bad = FaceBox {
            x1: 5.0,
            y1: 5.0,
            x2: 5.0,
            y2: 5.0,
            score: 0.9,
        };
        assert!(compute_crop(&bad, None, 100, 100, 100, 100).is_err());
    }

    #[test]
    fn 裁剪缩放尺寸正确() {
        let img = image::RgbImage::from_pixel(100, 140, image::Rgb([5, 6, 7]));
        let face = FaceBox {
            x1: 30.0,
            y1: 40.0,
            x2: 70.0,
            y2: 90.0,
            score: 0.99,
        };
        let r = compute_crop(&face, None, 100, 140, 295, 413).unwrap();
        let out = crop_resize(&img, &r, 295, 413).unwrap();
        assert_eq!(out.dimensions(), (295, 413));
    }

    #[test]
    fn 透明底裁剪缩放尺寸正确() {
        let img = image::RgbaImage::from_pixel(100, 140, image::Rgba([5, 6, 7, 128]));
        let face = FaceBox {
            x1: 30.0,
            y1: 40.0,
            x2: 70.0,
            y2: 90.0,
            score: 0.99,
        };
        let r = compute_crop(&face, None, 100, 140, 295, 413).unwrap();
        let out = crop_resize_rgba(&img, &r, 295, 413).unwrap();
        assert_eq!(out.dimensions(), (295, 413));
        assert!(out.pixels().all(|p| (p[3] as i32 - 128).abs() <= 2));
        assert!(crop_resize_rgba(&img, &r, 0, 413).is_err());
    }
}
