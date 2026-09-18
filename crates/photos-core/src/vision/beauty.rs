//! 美颜算子：磨皮（双边滤波）、提亮、美白（肤色区域向白调整）。
//! 纯 Rust 实现（image + imageproc），参数来自配置 `[beauty]` 段。

use image::{GrayImage, Luma, Rgb, RgbImage};
use imageproc::filter::bilateral_filter;

use crate::config::BeautyConfig;
use crate::vision::face::FaceBox;
use crate::vision::geometry::Point2;
use crate::vision::matting::feather;

/// 双边滤波窗口边长（像素，越大越糊）
const BILATERAL_WINDOW: u32 = 5;
/// 颜色相似度 sigma（0-255 灰度尺度，越大保留边缘越少）
const BILATERAL_SIGMA_COLOR: f32 = 32.0;
/// 空间相似度 sigma
const BILATERAL_SIGMA_SPATIAL: f32 = 4.0;
/// 五官保护区域羽化 sigma（避免保护边界出现硬过渡）
const FEATURE_FEATHER_SIGMA: f32 = 1.5;

/// 五官保护区域（椭圆，图像坐标）：磨皮避让五官，保留眼睛/眉毛/鼻/嘴锐度
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeatureRegion {
    /// 椭圆中心 x
    pub cx: f32,
    /// 椭圆中心 y
    pub cy: f32,
    /// 横向半径
    pub rx: f32,
    /// 纵向半径
    pub ry: f32,
}

/// 由人脸框与 5 关键点（左眼、右眼、鼻尖、左嘴角、右嘴角）推导五官保护区域：
/// 双眼（含眉，中心上移 0.05 脸高）、鼻、嘴各一个椭圆
pub fn face_feature_regions(face: &FaceBox, landmarks: &[Point2; 5]) -> Vec<FeatureRegion> {
    let fw = face.width().max(1.0);
    let fh = face.height().max(1.0);
    // 双眼（含眉：中心上移 0.05 脸高，纵向半径覆盖眉区）
    let mut regions = Vec::with_capacity(4);
    for eye in [&landmarks[0], &landmarks[1]] {
        regions.push(FeatureRegion {
            cx: eye.x as f32,
            cy: (eye.y - 0.05 * fh as f64) as f32,
            rx: 0.17 * fw,
            ry: 0.12 * fh,
        });
    }
    // 鼻
    regions.push(FeatureRegion {
        cx: landmarks[2].x as f32,
        cy: landmarks[2].y as f32,
        rx: 0.14 * fw,
        ry: 0.13 * fh,
    });
    // 嘴（两嘴角中点）
    regions.push(FeatureRegion {
        cx: ((landmarks[3].x + landmarks[4].x) / 2.0) as f32,
        cy: ((landmarks[3].y + landmarks[4].y) / 2.0) as f32,
        rx: 0.22 * fw,
        ry: 0.10 * fh,
    });
    regions
}

/// 生成五官保护掩膜（255 = 保护不磨皮，0 = 可磨皮；椭圆边界高斯羽化）
pub fn feature_protect_mask(w: u32, h: u32, regions: &[FeatureRegion]) -> GrayImage {
    let mut mask = GrayImage::from_pixel(w, h, Luma([0u8]));
    if w == 0 || h == 0 {
        return mask;
    }
    for r in regions {
        if r.rx <= 0.0 || r.ry <= 0.0 {
            continue;
        }
        let x1 = (r.cx - r.rx).floor().max(0.0) as u32;
        let x2 = (r.cx + r.rx).ceil().clamp(0.0, w as f32) as u32;
        let y1 = (r.cy - r.ry).floor().max(0.0) as u32;
        let y2 = (r.cy + r.ry).ceil().clamp(0.0, h as f32) as u32;
        for y in y1..y2 {
            for x in x1..x2 {
                let dx = (x as f32 + 0.5 - r.cx) / r.rx;
                let dy = (y as f32 + 0.5 - r.cy) / r.ry;
                if dx * dx + dy * dy <= 1.0 {
                    mask.put_pixel(x, y, Luma([255]));
                }
            }
        }
    }
    // 羽化保护边界，避免磨皮/非磨皮之间出现硬过渡
    feather(&mask, FEATURE_FEATHER_SIGMA)
}

/// 对 RGB 图应用美颜：磨皮 → 提亮 → 美白。
/// `params.enabled == false` 时原样返回。
pub fn apply_beauty(img: &RgbImage, params: &BeautyConfig) -> RgbImage {
    apply_beauty_protected(img, params, None)
}

/// 带五官保护的分区美颜：磨皮仅作用于保护区外（提亮/美白不受影响）
pub fn apply_beauty_protected(
    img: &RgbImage,
    params: &BeautyConfig,
    protect: Option<&GrayImage>,
) -> RgbImage {
    if !params.enabled {
        return img.clone();
    }
    let mut out = smooth(img, params.skin_smooth, protect);
    out = brighten(&out, params.brighten);
    whiten(&mut out, params.whiten);
    out
}

/// 磨皮：逐通道双边滤波后与原图按强度混合；`protect` 为 255 的像素不参与混合（保留五官）
fn smooth(img: &RgbImage, strength: f64, protect: Option<&GrayImage>) -> RgbImage {
    if strength <= 0.0 {
        return img.clone();
    }
    let s = strength.min(1.0);
    let (w, _h) = img.dimensions();
    let mask = protect.filter(|m| m.dimensions() == img.dimensions());
    let filtered = bilateral_rgb(img);
    let mut out = img.clone();
    for (i, (p, q)) in out.pixels_mut().zip(filtered.pixels()).enumerate() {
        let weight = match mask {
            Some(m) => {
                let (x, y) = (i as u32 % w, i as u32 / w);
                s * (1.0 - m.get_pixel(x, y)[0] as f64 / 255.0)
            }
            None => s,
        };
        if weight <= 0.0 {
            continue;
        }
        for c in 0..3 {
            let v = p[c] as f64 * (1.0 - weight) + q[c] as f64 * weight;
            p[c] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// 逐通道双边滤波（imageproc 仅提供灰度图双边滤波）
fn bilateral_rgb(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    let mut chans = Vec::with_capacity(3);
    for c in 0..3 {
        let gray = GrayImage::from_fn(w, h, |x, y| image::Luma([img.get_pixel(x, y)[c]]));
        chans.push(bilateral_filter(
            &gray,
            BILATERAL_WINDOW,
            BILATERAL_SIGMA_COLOR,
            BILATERAL_SIGMA_SPATIAL,
        ));
    }
    RgbImage::from_fn(w, h, |x, y| {
        Rgb([
            chans[0].get_pixel(x, y)[0],
            chans[1].get_pixel(x, y)[0],
            chans[2].get_pixel(x, y)[0],
        ])
    })
}

/// 提亮：整体亮度提升（0..=1 对应 0..255 增量）
fn brighten(img: &RgbImage, amount: f64) -> RgbImage {
    if amount <= 0.0 {
        return img.clone();
    }
    let delta = 255.0 * amount.min(1.0);
    let mut out = img.clone();
    for p in out.pixels_mut() {
        for c in 0..3 {
            p[c] = (p[c] as f64 + delta).round().min(255.0) as u8;
        }
    }
    out
}

/// 美白：肤色像素向白色方向按强度调整（含肤色均衡，仅作用皮肤区域）
fn whiten(img: &mut RgbImage, amount: f64) {
    if amount <= 0.0 {
        return;
    }
    let w = amount.min(1.0);
    for p in img.pixels_mut() {
        if is_skin(*p) {
            for c in 0..3 {
                let v = p[c] as f64 * (1.0 - w) + 255.0 * w;
                p[c] = v.round() as u8;
            }
        }
    }
}

/// 经典肤色检测规则（RGB 空间，面向证件照人像）
fn is_skin(p: Rgb<u8>) -> bool {
    let (r, g, b) = (p[0] as i16, p[1] as i16, p[2] as i16);
    r > 95 && g > 40 && b > 20 && r > g && r > b && r - g > 15
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, skin_smooth: f64, brighten: f64, whiten: f64) -> BeautyConfig {
        BeautyConfig {
            enabled,
            skin_smooth,
            brighten,
            whiten,
        }
    }

    /// 构造含高频噪声的人像肤色图（肤色 RGB 均值 (180,120,90) + 噪声）
    fn noisy_skin_image() -> RgbImage {
        let mut img = RgbImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let n = (x as i32 * 7 + y as i32 * 13) % 15 - 7; // 伪随机噪声
                img.put_pixel(
                    x,
                    y,
                    Rgb([
                        (180 + n).clamp(0, 255) as u8,
                        (120 + n).clamp(0, 255) as u8,
                        (90 + n).clamp(0, 255) as u8,
                    ]),
                );
            }
        }
        img
    }

    /// 3x3 邻域均值方差（衡量磨皮效果，越小越平滑）
    fn local_variance(img: &RgbImage) -> f64 {
        let (w, h) = img.dimensions();
        let mut sum = 0.0;
        let mut count = 0.0;
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let mut acc = 0.0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let p = img.get_pixel((x as i64 + dx) as u32, (y as i64 + dy) as u32);
                        acc += p[0] as f64;
                    }
                }
                let mean = acc / 9.0;
                let mut var = 0.0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let p = img.get_pixel((x as i64 + dx) as u32, (y as i64 + dy) as u32);
                        var += (p[0] as f64 - mean).powi(2);
                    }
                }
                sum += var / 9.0;
                count += 1.0;
            }
        }
        sum / count
    }

    fn mean_rgb(img: &RgbImage) -> [f64; 3] {
        let n = (img.width() * img.height()) as f64;
        let mut acc = [0.0f64; 3];
        for p in img.pixels() {
            for c in 0..3 {
                acc[c] += p[c] as f64;
            }
        }
        [acc[0] / n, acc[1] / n, acc[2] / n]
    }

    #[test]
    fn 关美颜时原图不变() {
        let img = noisy_skin_image();
        let out = apply_beauty(&img, &cfg(false, 0.5, 0.3, 0.2));
        assert_eq!(img, out);
    }

    #[test]
    fn 磨皮降低局部噪声方差() {
        let img = noisy_skin_image();
        let out = apply_beauty(&img, &cfg(true, 1.0, 0.0, 0.0));
        let before = local_variance(&img);
        let after = local_variance(&out);
        assert!(
            after < before * 0.5,
            "磨皮后方差 {after} 应显著低于磨皮前 {before}"
        );
    }

    #[test]
    fn 提亮提高平均亮度() {
        let img = noisy_skin_image();
        let out = apply_beauty(&img, &cfg(true, 0.0, 0.2, 0.0));
        let before = mean_rgb(&img);
        let after = mean_rgb(&out);
        // 提亮 0.2 → 全图 +51（上限 255）：每通道应提升约 51，断言 > 40
        for c in 0..3 {
            assert!(
                after[c] > before[c] + 40.0,
                "通道{c} 提亮后 {:.0} 应高于提亮前 {:.0}",
                after[c],
                before[c]
            );
        }
    }

    #[test]
    fn 美白提升肤色像素亮度() {
        let img = noisy_skin_image();
        let out = apply_beauty(&img, &cfg(true, 0.0, 0.0, 0.6));
        let before = mean_rgb(&img);
        let after = mean_rgb(&out);
        // 美白仅作用肤色区域，图片全部为肤色 → 各通道整体提升
        for c in 0..3 {
            assert!(
                after[c] > before[c] + 30.0,
                "通道{c} 美白后 {:.0} 应高于美白前 {:.0}",
                after[c],
                before[c]
            );
        }
    }

    #[test]
    fn 零强度时输出与原图一致() {
        let img = noisy_skin_image();
        let out = apply_beauty(&img, &cfg(true, 0.0, 0.0, 0.0));
        assert_eq!(img, out);
    }

    #[test]
    fn 非肤色背景不受美白影响() {
        // 蓝色背景 + 中心肤色块：背景色不应被拉白
        let mut img = RgbImage::from_pixel(32, 32, Rgb([67, 142, 219]));
        for y in 8..24 {
            for x in 8..24 {
                img.put_pixel(x, y, Rgb([180, 120, 90]));
            }
        }
        let out = apply_beauty(&img, &cfg(true, 0.0, 0.0, 1.0));
        // 角落背景像素不变
        assert_eq!(out.get_pixel(0, 0), &Rgb([67, 142, 219]));
        // 中心肤色像素提升
        let before = img.get_pixel(16, 16)[0];
        let after = out.get_pixel(16, 16)[0];
        assert!(after > before);
    }

    /// 五官保护掩膜：给定人脸框与 5 关键点，五官中心被保护、背景不受保护
    #[test]
    fn 五官保护掩膜覆盖五官中心() {
        let face = FaceBox {
            x1: 100.0,
            y1: 100.0,
            x2: 200.0,
            y2: 200.0,
            score: 0.99,
        };
        let landmarks = [
            Point2::new(125.0, 140.0),
            Point2::new(175.0, 140.0),
            Point2::new(150.0, 165.0),
            Point2::new(133.0, 185.0),
            Point2::new(167.0, 185.0),
        ];
        let regions = face_feature_regions(&face, &landmarks);
        assert_eq!(regions.len(), 4, "双眼 + 鼻 + 嘴共 4 个保护区");
        let mask = feature_protect_mask(300, 300, &regions);
        assert_eq!(mask.dimensions(), (300, 300));
        for (name, x, y) in [
            ("左眼", 125u32, 140u32),
            ("右眼", 175, 140),
            ("鼻尖", 150, 165),
            ("嘴中心", 150, 185),
        ] {
            assert!(
                mask.get_pixel(x, y)[0] > 200,
                "{name}({x},{y}) 应落入五官保护区，实际 {}",
                mask.get_pixel(x, y)[0]
            );
        }
        // 远离人脸处不受保护
        assert_eq!(mask.get_pixel(5, 5)[0], 0);
        assert_eq!(mask.get_pixel(290, 290)[0], 0);
    }

    /// 分区磨皮：保护区内像素不变，保护区外正常磨皮
    #[test]
    fn 五官保护区不磨皮而皮肤区磨皮() {
        let img = noisy_skin_image();
        // 左半 0..32 为五官保护区，右半为皮肤区
        let mut protect = GrayImage::from_pixel(64, 64, Luma([0u8]));
        for y in 0..64 {
            for x in 0..32 {
                protect.put_pixel(x, y, Luma([255]));
            }
        }
        let out = apply_beauty_protected(&img, &cfg(true, 1.0, 0.0, 0.0), Some(&protect));
        // 保护区内像素逐点不变
        assert_eq!(out.get_pixel(5, 5), img.get_pixel(5, 5));
        assert_eq!(out.get_pixel(31, 40), img.get_pixel(31, 40));
        // 保护区外磨皮生效：局部方差显著下降（避开保护区边界的过渡带）
        let crop = |image: &RgbImage| image::imageops::crop_imm(image, 36, 0, 28, 64).to_image();
        let before = local_variance(&crop(&img));
        let after = local_variance(&crop(&out));
        assert!(
            after < before * 0.5,
            "保护区外磨皮后方差 {after} 应显著低于磨皮前 {before}"
        );
    }
}
