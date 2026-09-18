//! 抠图后处理：概率 mask 软阈值（level-set）→ 形态学闭运算清理 → 距离场羽化。
//!
//! 相较早期「硬阈值 + 开运算 + 整图高斯羽化」：软阈值保留发丝等亚像素 alpha，
//! 闭运算填孔且不删除细结构，距离场羽化只在过渡带渐降（主体恒 255、外部恒 0）。

use image::{GrayImage, Luma};
use imageproc::distance_transform::{Norm, distance_transform};
use imageproc::morphology::close;

/// 距离场羽化前的形态学闭运算半径（填充 mask 内部小孔）
const CLOSE_RADIUS: u32 = 1;
/// 距离场骨架的 alpha 存在阈值：低于该值的 alpha 视为背景噪声，不多保留；
/// 高于该值（如淡发丝、薄纱的半透明像素）即纳入前景骨架，避免被距离场当背景压灭。
const SKELETON_EXIST_ALPHA: u8 = 16;

/// 填充二值 mask 中不与图像边界连通的内部孔洞（Hivision `hollow_out_fix` 的等价实现：
/// 泛洪标记「与边界连通的外部背景」，其余背景像素即为内部孔洞 → 置为前景）。
/// 闭运算只能填半径内的小孔，手臂与腰间、双臂环抱等大孔洞需本函数兜底。
pub fn fill_holes(bin: &GrayImage) -> GrayImage {
    let (w, h) = bin.dimensions();
    let n = (w * h) as usize;
    // outside：与边界连通的背景像素（4 连通泛洪）
    let mut outside = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let seed = |x: u32, y: u32, outside: &mut [bool], stack: &mut Vec<usize>| {
        let i = (y * w + x) as usize;
        if bin.get_pixel(x, y)[0] == 0 && !outside[i] {
            outside[i] = true;
            stack.push(i);
        }
    };
    for x in 0..w {
        seed(x, 0, &mut outside, &mut stack);
        seed(x, h - 1, &mut outside, &mut stack);
    }
    for y in 0..h {
        seed(0, y, &mut outside, &mut stack);
        seed(w - 1, y, &mut outside, &mut stack);
    }
    while let Some(i) = stack.pop() {
        let x = (i as u32) % w;
        let y = (i as u32) / w;
        let mut push = |nx: u32, ny: u32| {
            if nx < w && ny < h {
                let j = (ny * w + nx) as usize;
                if bin.get_pixel(nx, ny)[0] == 0 && !outside[j] {
                    outside[j] = true;
                    stack.push(j);
                }
            }
        };
        if x > 0 {
            push(x - 1, y);
        }
        push(x + 1, y);
        if y > 0 {
            push(x, y - 1);
        }
        push(x, y + 1);
    }
    let mut out = bin.clone();
    for (x, y, p) in out.enumerate_pixels_mut() {
        let i = (y * w + x) as usize;
        if p[0] == 0 && !outside[i] {
            *p = Luma([255]);
        }
    }
    out
}

/// 概率 mask（[0,255] 灰度）阈值化得到二值 mask
pub fn threshold_mask(mask: &GrayImage, threshold: u8) -> GrayImage {
    let mut out = GrayImage::new(mask.width(), mask.height());
    for (x, y, p) in mask.enumerate_pixels() {
        let v = if p[0] >= threshold { 255u8 } else { 0u8 };
        out.put_pixel(x, y, Luma([v]));
    }
    out
}

/// 形态学闭运算（先膨胀后腐蚀）：填充内部小孔，且不像开运算那样删除细结构（发丝）
pub fn morph_close(img: &GrayImage, radius: u32) -> GrayImage {
    let k = radius.clamp(1, u8::MAX as u32) as u8;
    close(img, Norm::LInf, k)
}

/// 边缘羽化：高斯模糊（sigma > 0 时生效）。
///
/// 仍用于五官保护掩膜、服装掩膜等需要各向同性平滑的场景；抠图 mask 请改用
/// [`distance_feather`]，避免整图高斯把发丝过渡糊化。
pub fn feather(img: &GrayImage, sigma: f32) -> GrayImage {
    if sigma <= 0.0 {
        return img.clone();
    }
    image::imageops::blur(img, sigma)
}

/// 软阈值（level-set）alpha：概率在 `[threshold - soft_range/2, threshold + soft_range/2]`
/// 区间内用 smoothstep 平滑过渡，保留发丝等亚像素半透明像素；`soft_range = 0` 退化为硬阈值。
pub fn levelset_alpha(prob: &GrayImage, threshold: u8, soft_range: u8) -> GrayImage {
    if soft_range == 0 {
        return threshold_mask(prob, threshold);
    }
    let lo = threshold as f32 - soft_range as f32 / 2.0;
    let span = soft_range as f32;
    let mut out = GrayImage::new(prob.width(), prob.height());
    for (x, y, p) in prob.enumerate_pixels() {
        let r = ((p[0] as f32 - lo) / span).clamp(0.0, 1.0);
        // smoothstep：一阶导在两端为 0，过渡带无折角
        let s = r * r * (3.0 - 2.0 * r);
        out.put_pixel(x, y, Luma([(s * 255.0).round() as u8]));
    }
    out
}

/// 距离场羽化：对二值骨架（alpha ≥ threshold）做闭运算清理后求有符号距离，
/// 主体内部恒 255、外部恒 0，仅过渡带（约 ±`feather_px`）内渐降并与输入软 alpha 取较小值。
///
/// 过渡带形状贴合骨架（各向异性），不会像整图高斯那样把细发丝糊穿。
/// 骨架阈值必须取「存在阈值」而非主阈值：软阈值过渡带产出的淡发丝 alpha 常低于
/// 主阈值（如 128），若以此划骨架会被当背景压灭（发丝细节丢失）。
pub fn distance_feather(alpha: &GrayImage, feather_px: f32) -> GrayImage {
    let (w, h) = alpha.dimensions();
    // 骨架清理：闭运算填小孔 + 泛洪填内部大孔（不与边界连通的背景）
    let bin = fill_holes(&morph_close(
        &threshold_mask(alpha, SKELETON_EXIST_ALPHA),
        CLOSE_RADIUS,
    ));
    // 补图：非零像素为源，故原前景像素得到「到最近背景像素的距离」
    let mut inv = GrayImage::new(w, h);
    for (x, y, p) in bin.enumerate_pixels() {
        inv.put_pixel(x, y, Luma([if p[0] == 0 { 255 } else { 0 }]));
    }
    let d_in = distance_transform(&inv, Norm::L2);
    let d_out = distance_transform(&bin, Norm::L2);
    let f = feather_px.max(1.0);
    let mut out = GrayImage::new(w, h);
    for (x, y, p) in alpha.enumerate_pixels() {
        let inside = bin.get_pixel(x, y)[0] > 0;
        let ramp = if inside {
            (d_in.get_pixel(x, y)[0] as f32 / f).clamp(0.0, 1.0)
        } else {
            1.0 - (d_out.get_pixel(x, y)[0] as f32 / f).clamp(0.0, 1.0)
        };
        let da = (ramp * 255.0).round() as u8;
        let v = if da == 0 {
            0
        } else if da == 255 {
            255
        } else {
            // 过渡带：保留输入软 alpha 的亚像素信息，同时受距离场上限约束
            p[0].min(da)
        };
        out.put_pixel(x, y, Luma([v]));
    }
    out
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
    fn 闭运算填孔且保留细结构() {
        // 5x5 全前景、中心为孔
        let mut px = vec![255u8; 25];
        px[12] = 0;
        let m = gray(&px, 5, 5);
        let c = morph_close(&m, 1);
        assert_eq!(c.get_pixel(2, 2)[0], 255, "内部孔洞应被填充");

        // 5x5 中的 1px 宽竖线（发丝）：闭运算后仍保留（开运算会腐蚀掉）
        let mut line = vec![0u8; 25];
        for y in 0..5 {
            line[y * 5 + 2] = 255;
        }
        let l = morph_close(&gray(&line, 5, 5), 1);
        assert_eq!(l.get_pixel(2, 2)[0], 255, "1px 细结构不应被删除");
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

    #[test]
    fn 软阈值过渡带单调且端点饱和() {
        let px: Vec<u8> = (0..=255u8).collect();
        let m = gray(&px, 256, 1);
        let a = levelset_alpha(&m, 128, 48);
        let vals: Vec<u8> = a.pixels().map(|p| p[0]).collect();
        // 过渡带下界 128-24=104：104 之前为 0，152 之后为 255
        assert_eq!(vals[0], 0);
        assert_eq!(vals[103], 0);
        assert_eq!(vals[152], 255);
        assert_eq!(vals[255], 255);
        // 过渡带内单调不减，且中点约为半透明
        for i in 104..152 {
            assert!(vals[i] <= vals[i + 1], "过渡带应单调不减：{i}");
        }
        assert!(vals[128] > 100 && vals[128] < 160, "中点应接近半透明");
    }

    #[test]
    fn 软阈值零范围退化为硬阈值() {
        let m = gray(&[0, 127, 128, 255], 4, 1);
        assert_eq!(levelset_alpha(&m, 128, 0), threshold_mask(&m, 128));
    }

    #[test]
    fn 孔洞填补() {
        // 环形：中心 2x2 孔洞不与边界连通 → 填充
        let mut px = vec![255u8; 36]; // 6x6
        for (x, y) in [(2usize, 2), (3, 2), (2, 3), (3, 3)] {
            px[y * 6 + x] = 0;
        }
        let f = fill_holes(&gray(&px, 6, 6));
        assert_eq!(f.get_pixel(2, 2)[0], 255, "内部孔洞应被填充");
        assert_eq!(f.get_pixel(0, 0)[0], 255);

        // C 形前景（开口朝右）：开口处背景与边界连通 → 不填充
        let mut px = [0u8; 25]; // 5x5 全背景
        for (x, y) in [(1usize, 1), (2, 1), (1, 2), (1, 3), (2, 3)] {
            px[y * 5 + x] = 255;
        }
        let f3 = fill_holes(&gray(&px, 5, 5));
        assert_eq!(f3.get_pixel(1, 1)[0], 255, "前景应保持不变");
        assert_eq!(f3.get_pixel(2, 2)[0], 0, "经开口连到边界的背景不应被填充");
        assert_eq!(f3.get_pixel(4, 4)[0], 0, "边界背景不应被填充");
    }

    #[test]
    fn 距离场羽化填充内部大孔() {
        // 16x16：8x8 前景环（中心 4x4 孔）→ 羽化后内部应被填充为前景
        let mut px = vec![0u8; 256];
        for y in 4..12 {
            for x in 4..12 {
                let hole = (6..10).contains(&x) && (6..10).contains(&y);
                if !hole {
                    px[y * 16 + x] = 255;
                }
            }
        }
        let a = distance_feather(&gray(&px, 16, 16), 2.0);
        assert_eq!(a.get_pixel(8, 8)[0], 255, "内部大孔应被填充");
        assert_eq!(a.get_pixel(0, 0)[0], 0, "外部背景不受影响");
    }

    #[test]
    fn 距离场羽化主体恒255外部恒0() {
        // 16x16：中心 8x8 为前景
        let mut px = vec![0u8; 256];
        for y in 4..12 {
            for x in 4..12 {
                px[y * 16 + x] = 255;
            }
        }
        let a = distance_feather(&gray(&px, 16, 16), 2.0);
        assert_eq!(a.get_pixel(8, 8)[0], 255, "主体内部应恒为 255");
        assert_eq!(a.get_pixel(0, 0)[0], 0, "外部应恒为 0");
        // 沿边界法线：由外向内单调不减（0 → 过渡带 → 主体 255）
        let row: Vec<u8> = (0..16).map(|x| a.get_pixel(x, 8)[0]).collect();
        for i in 0..4 {
            assert!(row[i] <= row[i + 1], "边界外侧应向外单调不增：{i}");
        }
        for i in 4..10 {
            assert!(row[i] <= row[i + 1], "边界内侧应向中心单调不减：{i}");
        }
        assert!(row[4] > 0 && row[4] < 255, "骨架边界应位于过渡带内");
    }

    #[test]
    fn 距离场羽化保留细发丝() {
        // 16x16 中的 1px 宽竖线：旧链路「开运算」会把整条线腐蚀掉，新链路应保留半透明
        let mut px = vec![0u8; 256];
        for y in 2..14 {
            px[y * 16 + 8] = 255;
        }
        let a = distance_feather(&gray(&px, 16, 16), 2.0);
        let v = a.get_pixel(8, 8)[0];
        assert!(v > 0, "细发丝不应被删除");
        assert!(v < 255, "1px 细结构应呈半透明过渡");
    }

    #[test]
    fn 距离场保留淡发丝低alpha() {
        // 主体 + 距边界 2px 外的 1px 淡发丝（prob≈0.47 → soft alpha 约 66）：
        // 淡发丝 alpha 低于骨架阈值，若用 128 标记背景会被距离场压灭。
        // 先经软阈值，再距离场羽化，淡发丝应保留为半透明而非归零。
        let mut px = vec![0u8; 640]; // 16x40
        for y in 0..16 {
            for x in 0..28 {
                px[y * 40 + x] = 255; // 主体
            }
        }
        for y in 0..16 {
            px[y * 40 + 30] = 120; // 淡发丝，距主体边界 2px
        }
        let soft = levelset_alpha(&gray(&px, 40, 16), 128, 48);
        let a = distance_feather(&soft, 2.0);
        let v = a.get_pixel(30, 8)[0];
        assert!(v > 30, "淡发丝不应被距离场压灭，实际 {v}");
        assert!(v < 255, "淡发丝应保留半透明，实际 {v}");
    }

    #[test]
    fn 软阈值过渡带下界保留淡发丝() {
        // prob 120（≈0.47）处于软阈值过渡带下缘：应产出非零半透明 alpha，
        // 而不是被硬阈值直接清零（此为发丝/半透明衣物丢细节的根因之一）。
        let m = gray(&[120, 111, 105, 104], 4, 1);
        let a = levelset_alpha(&m, 128, 48);
        let vals: Vec<u8> = a.pixels().map(|p| p[0]).collect();
        assert!(vals[0] > 40, "prob=120 应保留可见 alpha，实际 {}", vals[0]);
        assert!(vals[1] > 0, "prob=111 应保留非零 alpha，实际 {}", vals[1]);
        assert_eq!(vals[3], 0, "prob=104（过渡带下界）应恰好为零");
    }
}
