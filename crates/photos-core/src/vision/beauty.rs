//! 美颜算子：磨皮（双边滤波）、提亮、美白（肤色区域向白调整）。
//! 纯 Rust 实现（image + imageproc），参数来自配置 `[beauty]` 段。

use image::{GrayImage, Rgb, RgbImage};
use imageproc::filter::bilateral_filter;

use crate::config::BeautyConfig;

/// 双边滤波窗口边长（像素，越大越糊）
const BILATERAL_WINDOW: u32 = 5;
/// 颜色相似度 sigma（0-255 灰度尺度，越大保留边缘越少）
const BILATERAL_SIGMA_COLOR: f32 = 32.0;
/// 空间相似度 sigma
const BILATERAL_SIGMA_SPATIAL: f32 = 4.0;

/// 对 RGB 图应用美颜：磨皮 → 提亮 → 美白。
/// `params.enabled == false` 时原样返回。
pub fn apply_beauty(img: &RgbImage, params: &BeautyConfig) -> RgbImage {
    if !params.enabled {
        return img.clone();
    }
    let mut out = smooth(img, params.skin_smooth);
    out = brighten(&out, params.brighten);
    whiten(&mut out, params.whiten);
    out
}

/// 磨皮：逐通道双边滤波后与原图按强度混合（保留五官细节）
fn smooth(img: &RgbImage, strength: f64) -> RgbImage {
    if strength <= 0.0 {
        return img.clone();
    }
    let s = strength.min(1.0);
    let filtered = bilateral_rgb(img);
    let mut out = img.clone();
    for (p, q) in out.pixels_mut().zip(filtered.pixels()) {
        for c in 0..3 {
            let v = p[c] as f64 * (1.0 - s) + q[c] as f64 * s;
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
}
