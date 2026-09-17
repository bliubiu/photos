//! 换装算子：人像解析（LIP 20 类语义分割）→ 衣服区域 mask → 服装贴合 / 程序化正装。
//! 纯 Rust 实现（image + imageproc + 自研算子），虚拟试衣换装（M4 遗留项落地）。

use image::{GrayImage, Luma, Rgb, RgbImage};

use crate::error::{CoreError, CoreResult};
use crate::inference::TensorData;
use crate::preprocess::LetterBox;

/// 换装模型 id（人像解析，LIP 20 类；独立于三模式套件）
pub const PARSING_MODEL_ID: &str = "parsing_lip";

/// 服装语义类别（LIP 20 类）：5 上衣、6 连衣裙、7 外套、10 连体裤
pub const CLOTHING_CLASSES: [u8; 4] = [5, 6, 7, 10];

/// 边缘羽化高斯 sigma（与换底色羽化一致）
const GARMENT_FEATHER_SIGMA: f32 = 1.0;

/// 程序化正装样式（无外部素材即可生成）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuitStyle {
    /// 藏青西装 + 白衬衫
    Navy,
    /// 黑色西装 + 白衬衫
    Black,
    /// 白色衬衫
    White,
}

impl SuitStyle {
    /// 解析样式 id（suit_navy | suit_black | shirt_white）
    pub fn parse(s: &str) -> CoreResult<Self> {
        match s {
            "suit_navy" => Ok(Self::Navy),
            "suit_black" => Ok(Self::Black),
            "shirt_white" => Ok(Self::White),
            other => Err(CoreError::ConfigValidate(format!(
                "未知正装样式“{other}”，可选：suit_navy、suit_black、shirt_white"
            ))),
        }
    }
}

/// 解析输出布局（NCHW 或 NHWC）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsingLayout {
    /// NCHW `[1,20,H,W]`：通道维度在 dims[1]
    Nchw,
    /// NHWC `[1,H,W,20]`：通道维度在最后一维
    Nhwc,
}

/// 推断解析输出布局：返回（布局, 类别数, 宽, 高）。
fn parsing_layout(out: &TensorData) -> CoreResult<(ParsingLayout, usize, u32, u32)> {
    let d = &out.shape;
    if d.len() == 4 {
        if d[1] == 20 {
            return Ok((ParsingLayout::Nchw, 20, d[3].max(1) as u32, d[2].max(1) as u32));
        }
        if d[3] == 20 {
            return Ok((ParsingLayout::Nhwc, 20, d[2].max(1) as u32, d[1].max(1) as u32));
        }
    }
    Err(CoreError::Inference(format!(
        "无法识别的人像解析输出布局 {d:?}（需 [1,20,H,W] 或 [1,H,W,20]）"
    )))
}

/// 人像解析输出（[1,20,H,W] NCHW 或 [1,H,W,20] NHWC logits）→ 原图尺寸类别索引图（0..=19）。
/// 逐像素 argmax 得类别（LIP 模型 logits 直接 argmax，无需 softmax）；letterbox 画布
/// 先裁出内容区再最近邻还原，避免类别被插值污染。
pub fn decode_parsing(
    out: &TensorData,
    w: u32,
    h: u32,
    letterbox: Option<&LetterBox>,
) -> CoreResult<GrayImage> {
    let (layout, n_classes, out_w, out_h) = parsing_layout(out)?;
    let hw = (out_w * out_h) as usize;
    let mut vals = vec![0u8; hw];
    for (i, slot) in vals.iter_mut().enumerate() {
        let mut best = 0u8;
        let mut best_v = f32::NEG_INFINITY;
        for c in 0..n_classes {
            let idx = match layout {
                ParsingLayout::Nchw => c * hw + i,
                ParsingLayout::Nhwc => i * n_classes + c,
            };
            let v = out.data.get(idx).copied().unwrap_or(f32::NEG_INFINITY);
            if v > best_v {
                best_v = v;
                best = c as u8;
            }
        }
        *slot = best;
    }
    let cls = GrayImage::from_raw(out_w, out_h, vals)
        .ok_or_else(|| CoreError::Inference("解析图构造失败".into()))?;
    if let Some(lb) = letterbox {
        // 逆 letterbox：裁出等比内容区（含 pad 偏移）再缩放还原
        let cw = (lb.scale * w as f32).round().max(1.0) as u32;
        let ch = (lb.scale * h as f32).round().max(1.0) as u32;
        let cx = lb.pad_x.max(0.0) as u32;
        let cy = lb.pad_y.max(0.0) as u32;
        let sub = image::imageops::crop_imm(&cls, cx, cy, cw.min(out_w - cx), ch.min(out_h - cy))
            .to_image();
        Ok(image::imageops::resize(
            &sub,
            w,
            h,
            image::imageops::FilterType::Nearest,
        ))
    } else if cls.dimensions() != (w, h) {
        Ok(image::imageops::resize(
            &cls,
            w,
            h,
            image::imageops::FilterType::Nearest,
        ))
    } else {
        Ok(cls)
    }
}

/// 类别索引图 → 衣服二值 mask（服装类 255，其余 0）
pub fn clothes_mask(parsing: &GrayImage) -> GrayImage {
    GrayImage::from_fn(parsing.width(), parsing.height(), |x, y| {
        let c = parsing.get_pixel(x, y)[0];
        Luma([if CLOTHING_CLASSES.contains(&c) { 255 } else { 0 }])
    })
}

/// 服装贴合：按衣服 mask 包围盒将服装图等比缩放居中贴合，边缘按衣服 mask 羽化合成。
/// 头发/脸/手臂等保留区天然不受覆盖（不属于衣服 mask）；未检出衣服时原样返回。
pub fn fit_garment(
    portrait: &RgbImage,
    garment: &RgbImage,
    clothes: &GrayImage,
) -> CoreResult<RgbImage> {
    if portrait.dimensions() != clothes.dimensions() {
        return Err(CoreError::Image(format!(
            "人像与衣服 mask 尺寸不一致：{}x{} vs {}x{}",
            portrait.width(),
            portrait.height(),
            clothes.width(),
            clothes.height()
        )));
    }
    let Some((x0, y0, x1, y1)) = bounding_box(clothes) else {
        return Ok(portrait.clone());
    };
    let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
    let (gw, gh) = garment.dimensions();
    if gw == 0 || gh == 0 {
        return Err(CoreError::Image("服装图为空".into()));
    }
    // 等比缩放至贴合包围盒并居中（保持服装宽高比，超出部分裁掉）
    let scale = (bw as f32 / gw as f32).min(bh as f32 / gh as f32);
    let tw = (gw as f32 * scale).round().max(1.0) as u32;
    let th = (gh as f32 * scale).round().max(1.0) as u32;
    let gx = x0 + (bw - tw) / 2;
    let gy = y0 + (bh - th) / 2;
    let fit = image::imageops::resize(garment, tw, th, image::imageops::FilterType::Triangle);
    // alpha = 衣服 mask 局部区域，羽化平滑边缘
    let mut alpha = GrayImage::from_pixel(tw, th, Luma([0u8]));
    for y in 0..th {
        for x in 0..tw {
            alpha.put_pixel(x, y, Luma([clothes.get_pixel(gx + x, gy + y)[0]]));
        }
    }
    let alpha = super::matting::feather(&alpha, GARMENT_FEATHER_SIGMA);
    // 合成：out = 服装 × alpha + 原人像 × (1 - alpha)
    let mut out = portrait.clone();
    for y in 0..th {
        for x in 0..tw {
            let (px, py) = (gx + x, gy + y);
            if px >= portrait.width() || py >= portrait.height() {
                continue;
            }
            let a = alpha.get_pixel(x, y)[0] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let fg = fit.get_pixel(x, y);
            let bg = *out.get_pixel(px, py);
            out.put_pixel(
                px,
                py,
                Rgb([
                    (fg[0] as f32 * a + bg[0] as f32 * (1.0 - a)).round() as u8,
                    (fg[1] as f32 * a + bg[1] as f32 * (1.0 - a)).round() as u8,
                    (fg[2] as f32 * a + bg[2] as f32 * (1.0 - a)).round() as u8,
                ]),
            );
        }
    }
    Ok(out)
}

/// 程序化生成正装纹理图（无外部素材）：纯色西装外套 + 中央 V 领白衬衫。
/// 藏青/黑为深色西装 + 白衬衫领口；白衬衫样式整件为白色。
pub fn formal_suit(style: SuitStyle, w: u32, h: u32) -> RgbImage {
    let (suit_r, suit_g, suit_b) = match style {
        SuitStyle::Navy => (31u8, 56u8, 100u8),
        SuitStyle::Black => (34u8, 34u8, 34u8),
        SuitStyle::White => (245u8, 245u8, 245u8),
    };
    let mut img = RgbImage::from_pixel(w.max(1), h.max(1), Rgb([suit_r, suit_g, suit_b]));
    if w == 0 || h == 0 {
        return img;
    }
    // 中央 V 领白衬衫：领口自顶部中心下延，随深度加宽（深度 0.30h，半宽 0.06w → 0.36w）
    let depth = (0.30 * h as f64) as u32;
    let cx = w as f64 / 2.0;
    for y in 0..depth {
        let t = y as f64 / depth.max(1) as f64;
        let half = (0.06 + 0.30 * t) * w as f64;
        let x0 = (cx - half).max(0.0) as u32;
        let x1 = (cx + half).min(w as f64 - 1.0) as u32;
        for x in x0..=x1 {
            img.put_pixel(x, y, Rgb([245, 245, 245]));
        }
    }
    img
}

/// 二值 mask 前景包围盒（含边界）；无前景返回 None
fn bounding_box(mask: &GrayImage) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = mask.dimensions();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0u32, 0u32);
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            if mask.get_pixel(x, y)[0] > 0 {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if found {
        Some((min_x, min_y, max_x, max_y))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析输出argmax得类别() {
        // [1,20,2,2]：像素(0,0)=5上衣、(1,0)=13脸、(0,1)=0背景、(1,1)=10连体裤
        let mut data = vec![-10.0f32; 20 * 4];
        for (i, c) in [(0usize, 5usize), (1, 13), (3, 10)] {
            data[c * 4 + i] = 10.0;
        }
        let t = TensorData::new(vec![1, 20, 2, 2], data).unwrap();
        let m = decode_parsing(&t, 2, 2, None).unwrap();
        assert_eq!(m.get_pixel(0, 0)[0], 5);
        assert_eq!(m.get_pixel(1, 0)[0], 13);
        assert_eq!(m.get_pixel(0, 1)[0], 0);
        assert_eq!(m.get_pixel(1, 1)[0], 10);
    }

    #[test]
    fn 解析输出nhwc布局兼容() {
        // [1,2,2,20] NHWC：像素(0,0) 类别 7 外套
        let mut data = vec![-10.0f32; 2 * 2 * 20];
        data[7] = 10.0;
        let t = TensorData::new(vec![1, 2, 2, 20], data).unwrap();
        let m = decode_parsing(&t, 2, 2, None).unwrap();
        assert_eq!(m.get_pixel(0, 0)[0], 7);
    }

    #[test]
    fn 解析输出letterbox逆变换() {
        // 画布 4x4（scale 0.5, pad 0,0）：左上 2x2 内容区类别 13 → 还原到 4x2
        let mut data = vec![-10.0f32; 20 * 16];
        for i in 0..4 {
            data[13 * 16 + i] = 10.0;
        }
        let t = TensorData::new(vec![1, 20, 4, 4], data).unwrap();
        let lb = LetterBox {
            scale: 0.5,
            pad_x: 0.0,
            pad_y: 0.0,
        };
        let m = decode_parsing(&t, 4, 2, Some(&lb)).unwrap();
        assert_eq!(m.dimensions(), (4, 2));
        assert!(m.pixels().all(|p| p[0] == 13), "内容区类别应铺满还原图");
    }

    #[test]
    fn 衣服mask仅标记服装类() {
        // 4x4 类别图：5上衣 13脸 0背景 10连体裤 / 2头发 6连衣裙 7外套 9裤子 / ... / 3手套 4太阳镜 14左臂 15右臂
        let m = GrayImage::from_raw(4, 4, vec![5, 13, 0, 10, 2, 6, 7, 9, 0, 0, 0, 0, 3, 4, 14, 15])
            .unwrap();
        let c = clothes_mask(&m);
        let vals: Vec<u8> = c.pixels().map(|p| p[0]).collect();
        assert_eq!(
            vals,
            vec![255, 0, 0, 255, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn 服装贴合替换衣服区域() {
        // 人像 10x10 蓝底；衣服 mask：中央 4x4 块（x/y 2..6）
        let portrait = RgbImage::from_pixel(10, 10, Rgb([0, 0, 255]));
        let mut clothes = GrayImage::from_pixel(10, 10, Luma([0u8]));
        for y in 2..6 {
            for x in 2..6 {
                clothes.put_pixel(x, y, Luma([255u8]));
            }
        }
        // 服装图 2x2 全红
        let garment = RgbImage::from_pixel(2, 2, Rgb([255, 0, 0]));
        let out = fit_garment(&portrait, &garment, &clothes).unwrap();
        // 衣服区域被替换为红（羽化混合后仍显著偏红）
        let p = *out.get_pixel(3, 3);
        assert!(p[0] > 200, "衣服区应偏红，实际 {p:?}");
        assert!(p[2] < 100, "蓝色应被覆盖，实际 {p:?}");
        // 区域外保持蓝
        assert_eq!(*out.get_pixel(0, 0), Rgb([0, 0, 255]));
        assert_eq!(*out.get_pixel(9, 9), Rgb([0, 0, 255]));
    }

    #[test]
    fn 空衣服mask原样返回() {
        let portrait = RgbImage::from_pixel(4, 4, Rgb([1, 2, 3]));
        let clothes = GrayImage::from_pixel(4, 4, Luma([0u8]));
        let out = fit_garment(
            &portrait,
            &RgbImage::from_pixel(2, 2, Rgb([0, 0, 0])),
            &clothes,
        )
        .unwrap();
        assert_eq!(out, portrait);
    }

    #[test]
    fn 尺寸不一致报错() {
        let portrait = RgbImage::new(4, 4);
        let clothes = GrayImage::new(5, 5);
        assert!(fit_garment(&portrait, &RgbImage::new(2, 2), &clothes).is_err());
    }

    #[test]
    fn 正装样式生成不同配色() {
        let navy = formal_suit(SuitStyle::Navy, 40, 60);
        let black = formal_suit(SuitStyle::Black, 40, 60);
        let white = formal_suit(SuitStyle::White, 40, 60);
        // 底部（V 领外）为西装色
        assert_eq!(*navy.get_pixel(20, 50), Rgb([31, 56, 100]));
        assert_eq!(*black.get_pixel(20, 50), Rgb([34, 34, 34]));
        assert_eq!(*white.get_pixel(20, 50), Rgb([245, 245, 245]));
        // 顶部中央为白衬衫领口
        assert_eq!(*navy.get_pixel(20, 0), Rgb([245, 245, 245]));
        // 样式解析
        assert_eq!(SuitStyle::parse("suit_navy").unwrap(), SuitStyle::Navy);
        assert_eq!(SuitStyle::parse("shirt_white").unwrap(), SuitStyle::White);
        assert!(SuitStyle::parse("bogus").is_err());
    }

    #[test]
    fn 正装贴合到矩形衣服区() {
        // 人像灰底，衣服区 20..80 x 40..100 → 贴合后区域内出现藏青西装色
        let portrait = RgbImage::from_pixel(100, 140, Rgb([200, 200, 200]));
        let mut clothes = GrayImage::from_pixel(100, 140, Luma([0u8]));
        for y in 40..100 {
            for x in 20..80 {
                clothes.put_pixel(x, y, Luma([255u8]));
            }
        }
        let suit = formal_suit(SuitStyle::Navy, 100, 100);
        let out = fit_garment(&portrait, &suit, &clothes).unwrap();
        let p = *out.get_pixel(50, 90);
        assert!(p[0] < 100 && p[2] > 50, "藏青西装应偏蓝，实际 {p:?}");
        // 衣服区外保持灰底
        assert_eq!(*out.get_pixel(50, 10), Rgb([200, 200, 200]));
    }
}
