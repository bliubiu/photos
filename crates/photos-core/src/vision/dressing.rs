//! 换装算子：人像解析（LIP 20 类语义分割）→ 衣服区域 mask → 服装贴合 / 程序化正装。
//! 纯 Rust 实现（image + imageproc + 自研算子），虚拟试衣换装（M4 遗留项落地）。

use image::{GrayImage, Luma, Rgb, RgbImage, Rgba, RgbaImage};

use crate::error::{CoreError, CoreResult};
use crate::inference::TensorData;
use crate::preprocess::LetterBox;

/// 换装模型 id（人像解析，LIP 20 类；独立于三模式套件）
pub const PARSING_MODEL_ID: &str = "parsing_lip";

/// 服装语义类别（LIP 20 类）：5 上衣、6 连衣裙、7 外套、10 连体裤
/// （单件贴合语义：上半身 / 连体，避免误覆盖裤装与鞋）
pub const CLOTHING_CLASSES: [u8; 4] = [5, 6, 7, 10];

/// 全身服装类别：上衣类 + 8 裤子、9 短裤、16 左腿、17 右腿、18 左鞋、19 右鞋
/// （全身套装贴合：西装 + 西裤 + 皮鞋一次覆盖全身）
pub const FULL_CLOTHING_CLASSES: [u8; 10] = [5, 6, 7, 8, 9, 10, 16, 17, 18, 19];

/// 上半身服装类（上衣/连衣裙/外套/连体裤）——多图分部位贴合的上衣部位
pub const TOP_CLASSES: [u8; 4] = [5, 6, 7, 10];

/// 下装类（裤子/短裤）——多图分部位贴合的下装部位
pub const BOTTOM_CLASSES: [u8; 2] = [8, 9];

/// 鞋类（左鞋/右鞋）——多图分部位贴合的鞋部位
pub const SHOE_CLASSES: [u8; 2] = [18, 19];

/// 边缘羽化高斯 sigma（与换底色羽化一致）
const GARMENT_FEATHER_SIGMA: f32 = 1.0;

/// 光影合成强度：原图衣服区域明暗起伏的保留比例（0 = 不合成，1 = 完全保留）
const GARMENT_SHADING_STRENGTH: f32 = 0.6;

/// 光影调制系数上下限（原图过暗/过亮时避免服装失真）
const GARMENT_SHADING_RANGE: (f32, f32) = (0.6, 1.4);

/// 光影场模糊 sigma 占贴合区域短边的比例（去衣物纹理噪声、保留大范围光照方向）
const GARMENT_SHADING_SIGMA_RATIO: f32 = 0.05;

/// 自动去背：四角背景色差阈值（欧氏距离；纯色背景通常 < 12）
const PURE_BG_CORNER_TOL: f32 = 12.0;
/// 自动去背：背景判定距离下限（≤ 视为背景，透明）
const PURE_BG_MIN_DIST: f32 = 20.0;
/// 自动去背：背景判定距离上限（≥ 视为服装，不透明）
const PURE_BG_MAX_DIST: f32 = 48.0;

/// 程序化正装样式（无外部素材即可生成）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuitStyle {
    /// 藏青西装 + 白衬衫（上半身）
    Navy,
    /// 黑色西装 + 白衬衫（上半身）
    Black,
    /// 白色衬衫
    White,
    /// 藏青全身套装：西装 + 白衬衫 + 西裤 + 皮鞋
    FullNavy,
    /// 黑色全身套装：西装 + 白衬衫 + 西裤 + 皮鞋
    FullBlack,
}

impl SuitStyle {
    /// 解析样式 id（suit_navy | suit_black | shirt_white | suit_full_navy | suit_full_black）
    pub fn parse(s: &str) -> CoreResult<Self> {
        match s {
            "suit_navy" => Ok(Self::Navy),
            "suit_black" => Ok(Self::Black),
            "shirt_white" => Ok(Self::White),
            "suit_full_navy" => Ok(Self::FullNavy),
            "suit_full_black" => Ok(Self::FullBlack),
            other => Err(CoreError::ConfigValidate(format!(
                "未知正装样式“{other}”，可选：suit_navy、suit_black、shirt_white、suit_full_navy、suit_full_black"
            ))),
        }
    }

    /// 是否为全身套装（决定贴合时使用全身服装类集）
    pub fn is_full(self) -> bool {
        matches!(self, Self::FullNavy | Self::FullBlack)
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
            return Ok((
                ParsingLayout::Nchw,
                20,
                d[3].max(1) as u32,
                d[2].max(1) as u32,
            ));
        }
        if d[3] == 20 {
            return Ok((
                ParsingLayout::Nhwc,
                20,
                d[2].max(1) as u32,
                d[1].max(1) as u32,
            ));
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

/// 类别索引图 → 指定类别二值 mask（255，其余 0）
pub fn part_mask(parsing: &GrayImage, classes: &[u8]) -> GrayImage {
    GrayImage::from_fn(parsing.width(), parsing.height(), |x, y| {
        let c = parsing.get_pixel(x, y)[0];
        Luma([if classes.contains(&c) { 255 } else { 0 }])
    })
}

/// 类别索引图 → 衣服二值 mask（服装类 5/6/7/10，255，其余 0；上半身/连体语义）
pub fn clothes_mask(parsing: &GrayImage) -> GrayImage {
    part_mask(parsing, &CLOTHING_CLASSES)
}

/// 类别索引图 → 全身衣服二值 mask（服装类含裤装/腿/鞋，255，其余 0；全身套装语义）
pub fn full_clothes_mask(parsing: &GrayImage) -> GrayImage {
    part_mask(parsing, &FULL_CLOTHING_CLASSES)
}

/// 单部位贴合项：部位类别集 + 该部位服装图
pub struct GarmentPart<'a> {
    /// 该部位覆盖的 LIP 类别（如 `TOP_CLASSES`）
    pub classes: &'a [u8],
    /// 该部位服装图
    pub image: &'a RgbImage,
}

/// 多图分部位贴合：各部位按自身类别 mask 包围盒分别等比缩放居中贴合，依次叠加到人像上。
/// 未检出的部位（该类别无前景）自动跳过；解析图与人像尺寸须一致。
pub fn fit_garment_parts(
    portrait: &RgbImage,
    parsing: &GrayImage,
    parts: &[GarmentPart<'_>],
) -> CoreResult<RgbImage> {
    if portrait.dimensions() != parsing.dimensions() {
        return Err(CoreError::Image(format!(
            "人像与解析图尺寸不一致：{}x{} vs {}x{}",
            portrait.width(),
            portrait.height(),
            parsing.width(),
            parsing.height()
        )));
    }
    let mut out = portrait.clone();
    for part in parts {
        if part.image.dimensions().0 == 0 || part.image.dimensions().1 == 0 {
            return Err(CoreError::Image("分部位服装图为空".into()));
        }
        let mask = part_mask(parsing, part.classes);
        out = fit_garment(&out, part.image, &mask)?;
    }
    Ok(out)
}

/// 服装贴合：按衣服 mask 包围盒将服装图等比缩放居中贴合，边缘按衣服 mask 羽化合成。
/// 头发/脸/手臂等保留区天然不受覆盖（不属于衣服 mask）；未检出衣服时原样返回。
/// 服装图为不透明（无透明通道）时走此入口；透明服装图请使用 `fit_garment_alpha`。
/// 程序化纯色正装使用原图明暗调制（`apply_shading = true`）增加光影质感。
pub fn fit_garment(
    portrait: &RgbImage,
    garment: &RgbImage,
    clothes: &GrayImage,
) -> CoreResult<RgbImage> {
    let (gw, gh) = garment.dimensions();
    let opaque = GrayImage::from_pixel(gw, gh, Luma([255u8]));
    fit_garment_impl(portrait, garment, &opaque, clothes, true)
}

/// 服装贴合（带透明通道）：服装图 alpha 通道雕刻贴合形状，透明区不覆盖原人像。
/// 适用于真实服装照片抠底图（如 PNG 透明背景），杜绝「整张贴图含背景块」；
/// 用法与 `fit_garment` 一致，服装图为 `RgbaImage`。真实服装自带光影与明暗，
/// 不再用原图衣服区调制亮度（避免色相被带偏）。
pub fn fit_garment_alpha(
    portrait: &RgbImage,
    garment: &RgbaImage,
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
    let (gw, gh) = garment.dimensions();
    let mut rgb = RgbImage::new(gw, gh);
    let mut alpha = GrayImage::new(gw, gh);
    for (p, (r, a)) in garment.pixels().zip(rgb.pixels_mut().zip(alpha.pixels_mut())) {
        *r = Rgb([p[0], p[1], p[2]]);
        *a = Luma([p[3]]);
    }
    fit_garment_impl(portrait, &rgb, &alpha, clothes, false)
}

/// 服装贴合共享内核：按衣服 mask 包围盒将服装图等比缩放居中贴合，边缘按衣服 mask 羽化合成。
/// 服装图 alpha 面具雕刻形状（透明区不覆盖人像，保留内外轮廓）；头发/脸/手臂等保留区
/// 天然不受覆盖（不属于衣服 mask）；未检出衣服时原样返回。
fn fit_garment_impl(
    portrait: &RgbImage,
    garment: &RgbImage,
    garment_alpha: &GrayImage,
    clothes: &GrayImage,
    apply_shading: bool,
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
    let (gw, gh) = garment.dimensions();
    let (aw, ah) = garment_alpha.dimensions();
    if (gw, gh) != (aw, ah) {
        return Err(CoreError::Image(format!(
            "服装图与 alpha 面具尺寸不一致：{}x{} vs {}x{}",
            gw, gh, aw, ah
        )));
    }
    let Some((x0, y0, x1, y1)) = bounding_box(clothes) else {
        return Ok(portrait.clone());
    };
    let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
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
    let fit_alpha =
        image::imageops::resize(garment_alpha, tw, th, image::imageops::FilterType::Triangle);
    // alpha = 衣服 mask 局部 × 服装图 alpha（塑造服装自身形状）
    let mut alpha = GrayImage::from_pixel(tw, th, Luma([0u8]));
    for y in 0..th {
        for x in 0..tw {
            let mask = clothes.get_pixel(gx + x, gy + y)[0];
            let ga = fit_alpha.get_pixel(x, y)[0];
            alpha.put_pixel(
                x,
                y,
                Luma([((mask as f32 * ga as f32 / 255.0).round() as u16).min(255) as u8]),
            );
        }
    }
    let alpha = super::matting::feather(&alpha, GARMENT_FEATHER_SIGMA);
    // 光影合成：程序化正装以原图衣服区域明暗起伏调制亮度；真实服装自带光影，跳过调制
    let shade = if apply_shading {
        shading_factors(portrait, clothes, gx, gy, tw, th)
    } else {
        vec![1.0; (tw * th) as usize]
    };
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
            let fg = shade_pixel(*fit.get_pixel(x, y), shade[(y * tw + x) as usize]);
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

/// 按系数调制服装像素亮度（逐通道，钳制到 0..=255）
fn shade_pixel(px: Rgb<u8>, factor: f32) -> Rgb<u8> {
    Rgb([
        (px[0] as f32 * factor).round().clamp(0.0, 255.0) as u8,
        (px[1] as f32 * factor).round().clamp(0.0, 255.0) as u8,
        (px[2] as f32 * factor).round().clamp(0.0, 255.0) as u8,
    ])
}

/// 保守纯色背景自动去背：检测服装图四角颜色一致则判定为纯色背景，将接近背景色的
/// 像素置透明返回 `RgbaImage`；背景不统一（四角色差过大）时返回 None 保持原图。
/// 用于真实服装图（干净纯色底）无需手工抠底即可贴合。
pub fn auto_cutout_pure_background(img: &RgbImage) -> Option<RgbaImage> {
    let (w, h) = img.dimensions();
    if w < 8 || h < 8 {
        return None;
    }
    // 采样四角中心小块的均值作为背景参考色
    let corner = |cx: u32, cy: u32| -> [f32; 3] {
        let (mut s, mut n) = ([0f64; 3], 0usize);
        for dy in 0..4u32 {
            for dx in 0..4u32 {
                let x = (cx * 7 + dx).min(w - 1);
                let y = (cy * 7 + dy).min(h - 1);
                let p = img.get_pixel(x, y);
                for (i, c) in [p[0], p[1], p[2]].iter().enumerate() {
                    s[i] += *c as f64;
                }
                n += 1;
            }
        }
        [
            (s[0] / n as f64) as f32,
            (s[1] / n as f64) as f32,
            (s[2] / n as f64) as f32,
        ]
    };
    let corners = [corner(0, 0), corner(1, 0), corner(0, 1), corner(1, 1)];
    // 背景色差阈值：四角两两最大欧氏距离（纯色背景通常 < 12）
    let max_diff = (0..corners.len())
        .flat_map(|i| (i + 1..corners.len()).map(move |j| (i, j)))
        .map(|(i, j)| {
            let (a, b) = (corners[i], corners[j]);
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        })
        .fold(0.0f32, f32::max);
    if max_diff > PURE_BG_CORNER_TOL {
        return None;
    }
    let bg = [
        (corners[0][0] + corners[1][0] + corners[2][0] + corners[3][0]) / 4.0,
        (corners[0][1] + corners[1][1] + corners[2][1] + corners[3][1]) / 4.0,
        (corners[0][2] + corners[1][2] + corners[2][2] + corners[3][2]) / 4.0,
    ];
    // 接近背景色阈值：低于该值视为背景（透明），高于该值视为服装（不透明），之间渐变过渡
    let mut out = RgbaImage::new(w, h);
    for (p, o) in img.pixels().zip(out.pixels_mut()) {
        let d = ((p[0] as f32 - bg[0]).powi(2)
            + (p[1] as f32 - bg[1]).powi(2)
            + (p[2] as f32 - bg[2]).powi(2))
        .sqrt();
        let a = if d <= PURE_BG_MIN_DIST {
            0.0
        } else if d >= PURE_BG_MAX_DIST {
            255.0
        } else {
            let t = (d - PURE_BG_MIN_DIST) / (PURE_BG_MAX_DIST - PURE_BG_MIN_DIST);
            t * 255.0
        };
        *o = Rgba([p[0], p[1], p[2], a.round().clamp(0.0, 255.0) as u8]);
    }
    Some(out)
}

/// 服装贴合区域的光影场：输出逐像素亮度调制系数（1.0 = 与原图衣服区平均亮度一致）。
/// 取原图衣服像素（mask > 0）亮度 → 相对区域均值归一化 → 高斯模糊平滑（去纹理噪声、
/// 保留大范围光照方向）→ 强度加权并钳制。无有效衣服像素或区域亮度均值近 0 时返回全 1.0。
fn shading_factors(
    portrait: &RgbImage,
    clothes: &GrayImage,
    gx: u32,
    gy: u32,
    tw: u32,
    th: u32,
) -> Vec<f32> {
    let n = (tw * th) as usize;
    let in_mask = |x: u32, y: u32| -> bool {
        let (px, py) = (gx + x, gy + y);
        px < clothes.width() && py < clothes.height() && clothes.get_pixel(px, py)[0] > 0
    };
    let luminance = |x: u32, y: u32| -> f32 {
        let p = portrait.get_pixel(gx + x, gy + y);
        0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
    };
    let (mut sum, mut count) = (0f64, 0usize);
    for y in 0..th {
        for x in 0..tw {
            if in_mask(x, y) {
                sum += luminance(x, y) as f64;
                count += 1;
            }
        }
    }
    if count == 0 {
        return vec![1.0; n];
    }
    let mean = (sum / count as f64) as f32;
    if mean < 1.0 {
        // 原图衣服区几乎全黑（如黑色上衣），无可提取的明暗信息
        return vec![1.0; n];
    }
    // 非衣服像素以区域均值填充，避免模糊时把 0 带入污染边缘
    let filled = GrayImage::from_fn(tw, th, |x, y| {
        let v = if in_mask(x, y) { luminance(x, y) } else { mean };
        Luma([v.round().clamp(0.0, 255.0) as u8])
    });
    let sigma = (tw.min(th) as f32 * GARMENT_SHADING_SIGMA_RATIO).max(1.0);
    let smooth = image::imageops::blur(&filled, sigma);
    let (lo, hi) = GARMENT_SHADING_RANGE;
    smooth
        .as_raw()
        .iter()
        .map(|v| (1.0 + GARMENT_SHADING_STRENGTH * (*v as f32 / mean - 1.0)).clamp(lo, hi))
        .collect()
}

/// 程序化生成正装纹理图（无外部素材）：纯色西装外套 + 中央 V 领白衬衫。
/// 藏青/黑为深色西装 + 白衬衫领口；白衬衫样式整件为白色。
/// 全身套装（FullNavy/FullBlack）另绘制下半身西裤与底部黑皮鞋，一次覆盖全身。
pub fn formal_suit(style: SuitStyle, w: u32, h: u32) -> RgbImage {
    let (suit_r, suit_g, suit_b) = match style {
        SuitStyle::Navy => (31u8, 56u8, 100u8),
        SuitStyle::Black => (34u8, 34u8, 34u8),
        SuitStyle::White => (245u8, 245u8, 245u8),
        SuitStyle::FullNavy => (31u8, 56u8, 100u8),
        SuitStyle::FullBlack => (34u8, 34u8, 34u8),
    };
    let mut img = RgbImage::from_pixel(w.max(1), h.max(1), Rgb([suit_r, suit_g, suit_b]));
    if w == 0 || h == 0 {
        return img;
    }
    // 中央 V 领白衬衫：领口自顶部中心下延，随深度加宽（深度 0.28h，半宽 0.06w → 0.36w）
    let depth = (0.28 * h as f64) as u32;
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
    // 全身套装：0.50h..0.92h 为西裤（深色略深于西装），0.92h 以下为黑皮鞋
    if style.is_full() {
        let (pant_r, pant_g, pant_b) = match style {
            SuitStyle::FullNavy => (24u8, 26u8, 40u8),
            SuitStyle::FullBlack => (18u8, 18u8, 18u8),
            _ => unreachable!(),
        };
        let y0 = (0.50 * h as f64) as u32;
        let y1 = (0.92 * h as f64).max((y0 + 1) as f64) as u32;
        for y in y0..y1 {
            for x in 0..w {
                img.put_pixel(x, y, Rgb([pant_r, pant_g, pant_b]));
            }
        }
        for y in y1..h {
            for x in 0..w {
                img.put_pixel(x, y, Rgb([12, 12, 12]));
            }
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
        let m = GrayImage::from_raw(
            4,
            4,
            vec![5, 13, 0, 10, 2, 6, 7, 9, 0, 0, 0, 0, 3, 4, 14, 15],
        )
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
    fn 光影合成保留原图明暗起伏() {
        // 人像 40x20：衣服 mask 铺满；左半暗（60）右半亮（180），均值 120
        let mut portrait = RgbImage::from_pixel(40, 20, Rgb([60, 60, 60]));
        for y in 0..20 {
            for x in 20..40 {
                portrait.put_pixel(x, y, Rgb([180, 180, 180]));
            }
        }
        let clothes = GrayImage::from_pixel(40, 20, Luma([255u8]));
        // 服装图统一中灰：贴合后应呈现左侧压暗、右侧提亮的明暗起伏
        let garment = RgbImage::from_pixel(40, 20, Rgb([128, 128, 128]));
        let out = fit_garment(&portrait, &garment, &clothes).unwrap();
        let dark = out.get_pixel(2, 10)[0];
        let bright = out.get_pixel(37, 10)[0];
        assert!(dark < 110, "原图暗侧应压暗服装，实际 {dark}");
        assert!(bright > 145, "原图亮侧应提亮服装，实际 {bright}");
        assert!(bright > dark + 40, "应保留原图明暗起伏 {dark} → {bright}");
    }

    #[test]
    fn 光照均匀时服装不变化() {
        // 人像亮暗均匀（无明暗起伏）→ 光影系数恒为 1.0，服装原色输出
        let portrait = RgbImage::from_pixel(10, 10, Rgb([0, 0, 255]));
        let mut clothes = GrayImage::from_pixel(10, 10, Luma([0u8]));
        for y in 2..6 {
            for x in 2..6 {
                clothes.put_pixel(x, y, Luma([255u8]));
            }
        }
        let out = fit_garment(
            &portrait,
            &RgbImage::from_pixel(2, 2, Rgb([255, 0, 0])),
            &clothes,
        )
        .unwrap();
        assert_eq!(
            *out.get_pixel(3, 3),
            Rgb([255, 0, 0]),
            "均匀光照下服装不应被调制"
        );
    }

    #[test]
    fn 原图衣服区全黑不调制() {
        // 均值近 0（无明暗信息可提取）→ 返回全 1.0，服装保持原色
        let portrait = RgbImage::from_pixel(6, 6, Rgb([0, 0, 0]));
        let clothes = GrayImage::from_pixel(6, 6, Luma([255u8]));
        let factors = shading_factors(&portrait, &clothes, 0, 0, 6, 6);
        assert!(factors.iter().all(|f| (*f - 1.0).abs() < 1e-6));
    }

    #[test]
    fn 无衣服像素光影系数为一() {
        let portrait = RgbImage::from_pixel(4, 4, Rgb([200, 200, 200]));
        let clothes = GrayImage::from_pixel(4, 4, Luma([0u8]));
        let factors = shading_factors(&portrait, &clothes, 0, 0, 4, 4);
        assert!(factors.iter().all(|f| (*f - 1.0).abs() < 1e-6));
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

    #[test]
    fn 全身mask包含裤装与鞋() {
        // 4 像素：5 上衣、8 裤子、18 左鞋、13 脸
        let m = GrayImage::from_raw(4, 1, vec![5, 8, 18, 13]).unwrap();
        let f = full_clothes_mask(&m);
        let vals: Vec<u8> = f.pixels().map(|p| p[0]).collect();
        assert_eq!(vals, vec![255, 255, 255, 0]);
        // 单件语义不含裤/鞋（避免误覆盖下半身）
        let c = clothes_mask(&m);
        let vals_c: Vec<u8> = c.pixels().map(|p| p[0]).collect();
        assert_eq!(vals_c, vec![255, 0, 0, 0]);
    }

    #[test]
    fn 全身正装样式解析() {
        assert_eq!(
            SuitStyle::parse("suit_full_navy").unwrap(),
            SuitStyle::FullNavy
        );
        assert_eq!(
            SuitStyle::parse("suit_full_black").unwrap(),
            SuitStyle::FullBlack
        );
        assert!(SuitStyle::FullNavy.is_full() && SuitStyle::FullBlack.is_full());
        assert!(!SuitStyle::Navy.is_full());
    }

    #[test]
    fn 全身正装生成裤装与鞋区() {
        // 100x200：0..56 上身（V 领 0..28、西装 28..100）、100..184 西裤、184..200 黑皮鞋
        let suit = formal_suit(SuitStyle::FullNavy, 100, 200);
        assert_eq!(*suit.get_pixel(50, 0), Rgb([245, 245, 245]), "V 领白衬衫");
        assert_eq!(*suit.get_pixel(50, 60), Rgb([31, 56, 100]), "西装色");
        assert_eq!(*suit.get_pixel(50, 150), Rgb([24, 26, 40]), "西裤色");
        assert_eq!(*suit.get_pixel(50, 192), Rgb([12, 12, 12]), "黑皮鞋");
    }

    #[test]
    fn 全身正装贴合覆盖上下身() {
        // 人像 100x200 灰底；全身 mask：上衣+裤子 20..80 x 0..180（脸区天然 0）
        let portrait = RgbImage::from_pixel(100, 200, Rgb([200, 200, 200]));
        let mut clothes = GrayImage::from_pixel(100, 200, Luma([0u8]));
        for y in 0..180 {
            for x in 20..80 {
                clothes.put_pixel(x, y, Luma([255u8]));
            }
        }
        let suit = formal_suit(SuitStyle::FullNavy, 100, 200);
        let out = fit_garment(&portrait, &suit, &clothes).unwrap();
        // 上身（贴合后对应服装图西装区，避开 V 领）：藏青偏蓝
        let p = *out.get_pixel(50, 72);
        assert!(p[0] < 100 && p[2] > 50, "上身应藏青，实际 {p:?}");
        // 下半身（贴合后对应服装图裤区）：深藏青裤
        let q = *out.get_pixel(50, 100);
        assert!(q[2] > 30 && q[2] < 60, "裤子应深藏青，实际 {q:?}");
        // 衣服区外保持灰底
        assert_eq!(*out.get_pixel(0, 100), Rgb([200, 200, 200]));
    }

    #[test]
    fn 分部位贴合各自区域() {
        // 人像 100x200 灰底；解析图：上半 5 上衣、下半 8 裤子
        let portrait = RgbImage::from_pixel(100, 200, Rgb([200, 200, 200]));
        let mut parsing = GrayImage::from_pixel(100, 200, Luma([0u8]));
        for y in 0..100 {
            for x in 0..100 {
                parsing.put_pixel(x, y, Luma([5u8]));
            }
        }
        for y in 100..200 {
            for x in 0..100 {
                parsing.put_pixel(x, y, Luma([8u8]));
            }
        }
        // 上衣图纯红、下装图纯蓝，各贴合到对应部位
        let top = RgbImage::from_pixel(100, 100, Rgb([200, 30, 30]));
        let bottom = RgbImage::from_pixel(100, 100, Rgb([30, 30, 200]));
        let parts = [
            GarmentPart {
                classes: &TOP_CLASSES,
                image: &top,
            },
            GarmentPart {
                classes: &BOTTOM_CLASSES,
                image: &bottom,
            },
        ];
        let out = fit_garment_parts(&portrait, &parsing, &parts).unwrap();
        let p = *out.get_pixel(50, 50);
        assert!(p[0] > 150 && p[2] < 80, "上身应偏红，实际 {p:?}");
        let q = *out.get_pixel(50, 150);
        assert!(q[2] > 150 && q[0] < 80, "下身应偏蓝，实际 {q:?}");
    }

    #[test]
    fn 分部位未检出部位自动跳过() {
        // 解析图全为背景（0），任一部位无前景 → 原样返回
        let portrait = RgbImage::from_pixel(10, 10, Rgb([9, 9, 9]));
        let parsing = GrayImage::from_pixel(10, 10, Luma([0u8]));
        let top = RgbImage::from_pixel(10, 10, Rgb([200, 30, 30]));
        let parts = [GarmentPart {
            classes: &TOP_CLASSES,
            image: &top,
        }];
        let out = fit_garment_parts(&portrait, &parsing, &parts).unwrap();
        assert_eq!(out, portrait);
    }

    #[test]
    fn 分部位尺寸不一致报错() {
        let portrait = RgbImage::new(10, 10);
        let parsing = GrayImage::new(11, 10);
        let top = RgbImage::new(5, 5);
        let parts = [GarmentPart {
            classes: &TOP_CLASSES,
            image: &top,
        }];
        assert!(fit_garment_parts(&portrait, &parsing, &parts).is_err());
    }

    #[test]
    fn 透明通道服装仅覆盖不透明区域() {
        // 人像 40x40 灰底；衣服 mask 全图矩形式；服装图为 4x4 RGBA：仅中央 2x2 不透明红，
        // 四周透明。贴合后四周应保持原灰底，中央非透明区应出现服装红。
        let portrait = RgbImage::from_pixel(40, 40, Rgb([200, 200, 200]));
        let clothes = GrayImage::from_pixel(40, 40, Luma([255u8]));
        let mut garment = RgbaImage::new(4, 4);
        for (p, o) in garment.pixels_mut().enumerate() {
            let (x, y) = ((p % 4) as u32, (p / 4) as u32);
            let a = if (1..3).contains(&x) && (1..3).contains(&y) { 255 } else { 0 };
            *o = Rgba([200, 30, 30, a]);
        }
        let out = fit_garment_alpha(&portrait, &garment, &clothes).unwrap();
        // 包围盒为全图 (40x40)，服装图被等比放大至填满；透明边角应回落到原灰底
        assert_eq!(*out.get_pixel(3, 3), Rgb([200, 200, 200]));
        assert_eq!(*out.get_pixel(36, 3), Rgb([200, 200, 200]));
        // 中央服装区应为偏红
        let c = *out.get_pixel(20, 20);
        assert!(c[0] > 150 && c[2] < 100, "中央应偏红，实际 {c:?}");
    }

    #[test]
    fn 无透明通道服装行为与原贴合一致() {
        // 全不透明服装图经 alpha 路径应等价于原 RGB 贴合
        let portrait = RgbImage::from_pixel(40, 40, Rgb([200, 200, 200]));
        let clothes = GrayImage::from_pixel(40, 40, Luma([255u8]));
        let rgb = RgbImage::from_pixel(4, 4, Rgb([200, 30, 30]));
        let rgba = RgbaImage::from_pixel(4, 4, Rgba([200, 30, 30, 255]));
        let a = fit_garment(&portrait, &rgb, &clothes).unwrap();
        let b = fit_garment_alpha(&portrait, &rgba, &clothes).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn 纯色背景自动去背为透明() {
        // 40x40 白底，中央 20x20 藏青色（避开四角采样区） → 四角一致白 → 去背后四角透明、中央保留
        let mut img = RgbImage::from_pixel(40, 40, Rgb([255, 255, 255]));
        for y in 14..26 {
            for x in 14..26 {
                img.put_pixel(x, y, Rgb([31, 56, 100]));
            }
        }
        let cut = auto_cutout_pure_background(&img).expect("应识别纯色背景");
        assert_eq!(cut.get_pixel(2, 2)[3], 0, "左下角应为背景透明");
        assert_eq!(cut.get_pixel(38, 38)[3], 0, "右下角应为背景透明");
        assert_eq!(cut.get_pixel(20, 20)[3], 255, "中央服装应不透明");
    }

    #[test]
    fn 非统一背景不去背() {
        // 四角色差异大（渐变/复杂背景）→ 保守策略返回 None
        let mut img = RgbImage::new(20, 20);
        for y in 0..20 {
            for x in 0..20 {
                img.put_pixel(x, y, Rgb([(x * 12) as u8, (y * 12) as u8, 0]));
            }
        }
        assert!(auto_cutout_pure_background(&img).is_none());
    }

    #[test]
    fn 过小图像不去背() {
        let img = RgbImage::new(4, 4);
        assert!(auto_cutout_pure_background(&img).is_none());
    }

    #[test]
    fn 真实服装不受原图明暗调制() {
        // 服装图全不透明纯藏青；原图衣服区分别用暗色与亮色。
        // alpha 路径（真实服装）应保持藏青本色；RGB 路径（程序化正装）受明暗调制。
        let clothes = GrayImage::from_pixel(40, 40, Luma([255u8]));
        let dark = RgbImage::from_pixel(40, 40, Rgb([10, 10, 30]));
        let light = RgbImage::from_pixel(40, 40, Rgb([230, 230, 240]));
        let gar_rgb = RgbImage::from_pixel(4, 4, Rgb([31, 56, 100]));
        let gar_rgba = RgbaImage::from_pixel(4, 4, Rgba([31, 56, 100, 255]));
        let a_dark = fit_garment(&dark, &gar_rgb, &clothes).unwrap();
        let a_light = fit_garment(&light, &gar_rgb, &clothes).unwrap();
        let b_dark = fit_garment_alpha(&dark, &gar_rgba, &clothes).unwrap();
        let b_light = fit_garment_alpha(&light, &gar_rgba, &clothes).unwrap();
        // 程序化正装：明暗不同的原图应产生不同亮度
        let dc = *a_dark.get_pixel(20, 20);
        let lc = *a_light.get_pixel(20, 20);
        assert_ne!(dc, lc, "程序化正装应因原图明暗而变化：{dc:?} vs {lc:?}");
        // 真实服装：保持本色，不受原图明暗影响
        let bc = *b_dark.get_pixel(20, 20);
        let bc2 = *b_light.get_pixel(20, 20);
        assert_eq!(bc, bc2, "真实服装不应受原图明暗调制：{bc:?} vs {bc2:?}");
        assert!(bc[0] >= 31 && bc[2] > 80, "真实服装应接近藏青本色，实际 {bc:?}");
    }
}
