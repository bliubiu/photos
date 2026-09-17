//! pipeline 编排器：`Photo → Request → Result` 最小闭环（架构文档 §3 九步链路）。
//!
//! M1 范围：单底色、无排版、无美颜；推理输入由 `preprocess::build_input` 按模型
//! `input_dims` 真实构造（letterbox/归一化/布局对齐），引擎可为 `OrtEngine`（真实
//! ONNX 推理）或 `FakeEngine`（mock 回放，测试与 `--demo` 演示用）。

use std::path::PathBuf;

use image::{GrayImage, Luma, RgbImage};

use crate::config::{BeautyConfig, Config};
use crate::error::{CoreError, CoreResult};
use crate::inference::{FakeEngine, InferenceEngine, TensorData, ensure_models_ready};
use crate::preprocess::{LetterBox, build_input, probability_map};
use crate::vision::affine::rotate_image_same;
use crate::vision::beauty::apply_beauty;
use crate::vision::blend::composite_feathered;
use crate::vision::crop::{compute_crop, crop_resize};
use crate::vision::dressing::{SuitStyle, self};
use crate::vision::face::{decode_retinaface, retinaface_prior_count};
use crate::vision::geometry::{
    RotationDecision, decide_rotation, fused_angle, head_angle, shoulder_angle,
};
use crate::vision::keypoint::{KeypointSet, decode_movenet};
use crate::vision::matting::{morph_open, threshold_mask};

/// 人脸检测分数阈值
const FACE_SCORE_THRESHOLD: f32 = 0.5;
/// NMS IoU 阈值
const NMS_IOU_THRESHOLD: f32 = 0.4;
/// 抠图概率 mask 阈值（[0,1] 输出按 ×255 后阈值化）
const MASK_THRESHOLD: u8 = 128;
/// 形态学开运算半径
const MASK_MORPH_RADIUS: u32 = 1;
/// 边缘羽化高斯 sigma
const MASK_FEATHER_SIGMA: f32 = 1.0;
/// 头顶留白 = 0.2 × 脸高
const CROP_TOP_RATIO: f64 = 0.2;
/// 下巴余量 = 0.1 × 脸高
const CROP_BOTTOM_RATIO: f64 = 0.1;

/// 处理请求（单图，多产物）
#[derive(Debug, Clone)]
pub struct ProcessRequest {
    /// 输入图片路径
    pub input: PathBuf,
    /// 运行模式 id（speed | balanced | quality）
    pub mode: String,
    /// 尺寸标准 id（如 one_inch）
    pub size: String,
    /// 底色 id 列表（1..N，每底色各出一张证件照）
    pub bgs: Vec<String>,
    /// 手动纠偏角度（度，可选，上限 ±45）
    pub rotate: Option<f64>,
    /// 是否输出通用效果图（每底色各一张，保持旋转后全图尺寸）
    pub effect: bool,
    /// 排版相纸 id（如 6inch | a4，可选；以首个底色证件照铺版）
    pub layout: Option<String>,
    /// 美颜参数（None 不美颜；Some 时未指定的强度取全局配置 `[beauty]` 默认值）
    pub beauty: Option<BeautyParams>,
    /// 换装参数（None 不换装；启用时人像解析 + 服装贴合，作用于纠偏后原图）
    pub dress: Option<DressParams>,
}

/// 美颜请求参数（M4 实现算子）
#[derive(Debug, Clone, PartialEq)]
pub struct BeautyParams {
    /// 是否启用美颜
    pub enabled: bool,
    /// 磨皮强度（可选，默认取全局配置）
    pub skin_smooth: Option<f64>,
    /// 提亮强度（可选，默认取全局配置）
    pub brighten: Option<f64>,
    /// 美白强度（可选，默认取全局配置）
    pub whiten: Option<f64>,
}

/// 换装请求参数（人像解析 + 服装贴合，M4 遗留项落地）
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DressParams {
    /// 是否启用换装
    pub enabled: bool,
    /// 用户服装图路径（可选，优先于 `style`）
    pub garment: Option<PathBuf>,
    /// 程序化正装样式（suit_navy | suit_black | shirt_white；无 garment 时生效）
    pub style: Option<String>,
}

/// 单底色产物（证件照或效果图）
#[derive(Debug)]
pub struct BgOutput {
    /// 底色 id（与配置一致，用于命名）
    pub bg: String,
    /// 输出图像
    pub image: RgbImage,
}

/// 处理结果（一次请求多产物）
#[derive(Debug)]
pub struct PipelineResult {
    /// 每底色证件照（1..N 张）
    pub photos: Vec<BgOutput>,
    /// 每底色效果图（可选，effect=true 时）
    pub effects: Vec<BgOutput>,
    /// 排版相纸（可选，layout 指定时）
    pub layout: Option<RgbImage>,
    /// 纠偏决策（含校正角与告警）
    pub decision: RotationDecision,
    /// 处理告警（降级原因等）
    pub warnings: Vec<String>,
}

/// 执行单图最小闭环：读图 → 检测/抠图 → 角度决策 → 同步纠偏 → 换底色 → 裁切缩放
pub fn run_pipeline(
    cfg: &Config,
    engine: &mut dyn InferenceEngine,
    req: &ProcessRequest,
) -> CoreResult<PipelineResult> {
    // 1. 解析模式/尺寸/底色列表，并校验该模式模型就绪（缺失给出中文指引）
    let suite = cfg.mode(&req.mode)?;
    let size = cfg.size(&req.size)?;
    if req.bgs.is_empty() {
        return Err(CoreError::ConfigValidate("底色列表不能为空".into()));
    }
    let bgs = req
        .bgs
        .iter()
        .map(|id| cfg.background(id).map(|b| (id.clone(), b)))
        .collect::<CoreResult<Vec<_>>>()?;
    ensure_models_ready(cfg, engine, &req.mode)?;

    // 2. 读图（统一 RGB）
    let img = image::open(&req.input)
        .map_err(|e| CoreError::Image(format!("读取图片 {} 失败：{e}", req.input.display())))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(CoreError::Image("图片尺寸为零".into()));
    }

    // 3. 推理（按模型 input_dims 构造真实输入：letterbox/归一化/布局对齐）
    // RetinaFace 官方预处理为 RGB 减均值 (104,117,123)，其余模型为 RGB 归一化 [0,1]
    let face_spec = cfg.model_spec(&suite.face)?;
    let face_in = build_input(&img, &face_spec.input_dims, true)?;
    let face_outs = engine.run(&suite.face, &face_in.tensor)?;
    let kp_spec = cfg.model_spec(&suite.keypoint)?;
    let kp_in = build_input(&img, &kp_spec.input_dims, false)?;
    let kp_outs = engine.run(&suite.keypoint, &kp_in.tensor)?;
    let mat_spec = cfg.model_spec(&suite.matting)?;
    let mat_in = build_input(&img, &mat_spec.input_dims, false)?;
    let mat_outs = engine.run(&suite.matting, &mat_in.tensor)?;
    // 4. 解码：人脸框 + 5 关键点、17 关键点、概率 mask（mask 输出尺寸与原图不一致时先 resize）
    let (scale_x, scale_y, pad_x, pad_y) = match face_in.letterbox {
        Some(lb) => (lb.scale, lb.scale, lb.pad_x, lb.pad_y),
        None => (1.0, 1.0, 0.0, 0.0),
    };
    // 真实 RetinaFace 输出顺序为 [bbox, confidence, landmark]（Hivision 官方模型），
    // decode_retinaface 期望 [scores, boxes, landmarks]，此处按位置重排
    let faces = decode_retinaface(
        &face_outs[1],
        &face_outs[0],
        &face_outs[2],
        FACE_SCORE_THRESHOLD,
        NMS_IOU_THRESHOLD,
        (
            face_spec.input_dims[2] as u32,
            face_spec.input_dims[3] as u32,
        ),
        scale_x,
        scale_y,
        pad_x,
        pad_y,
    )?;
    let face = faces
        .first()
        .ok_or_else(|| CoreError::Image("未检测到人脸".into()))?;
    let kps = decode_movenet(&kp_outs[0], w, h)?;
    let mask = probability_mask(&mat_outs[0], w, h, mat_in.letterbox.as_ref())?;

    // 5. 姿态角度求解（0.6 头部 + 0.4 肩线，缺失降级并告警）
    let mut warnings = Vec::new();
    let measured = fused_measured(&kps, &mut warnings);
    let decision = decide_rotation(measured, req.rotate)?;
    if let Some(warn) = decision.warning() {
        warnings.push(warn);
    }

    // 6. 同步几何纠偏（同一仿射矩阵变换原图与 mask）
    let (rot_img, rot_mask) = rotate_image_same(&img, &mask, decision.correction())?;

    // 6.5 换装（可选）：人像解析 → 衣服 mask → 服装贴合（作用于旋转后原图，美颜之前）。
    // 解析模型独立于三模式套件，按需惰性装载（失败给出中文指引）。
    let dressed = match &req.dress {
        Some(d) if d.enabled => {
            engine.load(cfg, dressing::PARSING_MODEL_ID, suite.execution_provider)?;
            let p_spec = cfg.model_spec(dressing::PARSING_MODEL_ID)?;
            let p_in = build_input(&rot_img, &p_spec.input_dims, false)?;
            let p_outs = engine.run(dressing::PARSING_MODEL_ID, &p_in.tensor)?;
            let parsing = dressing::decode_parsing(
                &p_outs[0],
                rot_img.width(),
                rot_img.height(),
                p_in.letterbox.as_ref(),
            )?;
            let clothes = dressing::clothes_mask(&parsing);
            let garment = match &d.garment {
                Some(path) => image::open(path)
                    .map_err(|e| {
                        CoreError::Image(format!("读取服装图 {} 失败：{e}", path.display()))
                    })?
                    .to_rgb8(),
                None => dressing::formal_suit(
                    SuitStyle::parse(d.style.as_deref().unwrap_or("suit_navy"))?,
                    240,
                    360,
                ),
            };
            dressing::fit_garment(&rot_img, &garment, &clothes)?
        }
        _ => rot_img.clone(),
    };

    // 6.6 美颜（可选）：换底色前对换装后原图做磨皮/提亮/美白（美颜不改变 mask 与裁剪框）
    let beautified = match &req.beauty {
        Some(p) if p.enabled => apply_beauty(
            &dressed,
            &BeautyConfig {
                enabled: true,
                skin_smooth: p.skin_smooth.unwrap_or(cfg.beauty.skin_smooth),
                brighten: p.brighten.unwrap_or(cfg.beauty.brighten),
                whiten: p.whiten.unwrap_or(cfg.beauty.whiten),
            },
        ),
        _ => dressed,
    };

    // 7. 换底色（mask 羽化后逐像素 alpha 混合，按底色重复；廉价操作只做一次检测/抠图/纠偏）
    // 7.1 裁剪框与底色无关，先算一次
    let crop = compute_crop(
        &face.face,
        w,
        h,
        size.width_px,
        size.height_px,
        CROP_TOP_RATIO,
        CROP_BOTTOM_RATIO,
    )?;
    let mut photos = Vec::with_capacity(bgs.len());
    let mut effects = Vec::new();
    for (bg_id, bg) in &bgs {
        // 效果图：换底后保持旋转全图尺寸
        let composed = composite_feathered(&beautified, &rot_mask, bg.rgb, MASK_FEATHER_SIGMA);
        if req.effect {
            effects.push(BgOutput {
                bg: bg_id.clone(),
                image: composed.clone(),
            });
        }
        // 证件照：按人脸框裁切缩放
        let final_img = crop_resize(&composed, &crop, size.width_px, size.height_px)?;
        photos.push(BgOutput {
            bg: bg_id.clone(),
            image: final_img,
        });
    }

    // 8. 排版相纸（可选）：以首个底色证件照按相纸规格铺版
    let layout_img = match &req.layout {
        Some(layout_id) => {
            let spec = cfg.layout.get(layout_id).ok_or_else(|| {
                CoreError::ConfigValidate(format!(
                    "未知排版“{layout_id}”，可选：{}",
                    cfg.layout.keys().cloned().collect::<Vec<_>>().join("、")
                ))
            })?;
            Some(crate::vision::layout::compose(
                &photos[0].image,
                spec,
                size.dpi,
            )?)
        }
        None => None,
    };

    Ok(PipelineResult {
        photos,
        effects,
        layout: layout_img,
        decision,
        warnings,
    })
}

/// 概率 mask 张量 `[1,1,H,W]`（行主序）→ 原图尺寸二值 mask（letterbox 逆变换 + 阈值化 + 形态学去噪）
fn probability_mask(
    out: &TensorData,
    w: u32,
    h: u32,
    letterbox: Option<&LetterBox>,
) -> CoreResult<GrayImage> {
    let prob = probability_map(out, w, h, letterbox)?;
    let bin = threshold_mask(&prob, MASK_THRESHOLD);
    Ok(morph_open(&bin, MASK_MORPH_RADIUS))
}

/// 演示引擎：balanced 三件套内置 mock 回放（`photos process --demo` 与测试共用）。
/// 人脸框接近全图（候选 idx 16421 = stride32 cell(10,10) min512，loc 全 0 解码 → 中心 336/640、半宽 256）、
/// 双眼/双肩水平（融合角 0）、mask 为中心椭圆（可演示换底色）。
pub fn demo_balanced_engine(w: u32, h: u32) -> FakeEngine {
    let (fw, fh) = (w as f32, h as f32);
    let n = retinaface_prior_count((640, 640));
    let face_idx = 16421usize; // stride32 cell(10,10) min512 → 解码 bbox (80,80,592,592)/640 → 还原后接近全图
    let mut conf = vec![0.0f32; n * 2];
    conf[face_idx * 2] = 0.01;
    conf[face_idx * 2 + 1] = 0.99;
    // 与真实 RetinaFace 输出顺序一致：[bbox(loc), confidence, landmark]；loc/landmark 全 0
    let face_out = vec![
        TensorData::new(vec![1, n as i64, 4], vec![0.0; n * 4]).unwrap(),
        TensorData::new(vec![1, n as i64, 2], conf).unwrap(),
        TensorData::new(vec![1, n as i64, 10], vec![0.0; n * 10]).unwrap(),
    ];
    // 双眼 idx1/2、双肩 idx5/6 均高置信且水平（y 相同 → 角度 0）
    let mut kp = vec![0.0f32; 17 * 3];
    for (i, (x, y)) in [
        (1usize, (0.44f32, 0.40f32)),
        (2, (0.56, 0.40)),
        (5, (0.30, 0.75)),
        (6, (0.70, 0.75)),
    ] {
        kp[i * 3] = y * fh;
        kp[i * 3 + 1] = x * fw;
        kp[i * 3 + 2] = 0.99;
    }
    // mask：中心椭圆不透明（a=0.32w、b=0.38h），边缘透明以演示换底色。
    // 与真实 BiRefNet 输出一致：先按原图生成椭圆，再 letterbox 到 1024x1024 画布，
    // 使 pipeline 的 letterbox 逆变换能还原回原图几何
    let mut el = RgbImage::from_pixel(w, h, image::Rgb([0, 0, 0]));
    let a = 0.32 * fw;
    let b = 0.38 * fh;
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - fw / 2.0;
            let dy = y as f32 - fh / 2.0;
            if dx * dx / (a * a) + dy * dy / (b * b) <= 1.0 {
                el.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
    }
    let (canvas, _) = crate::preprocess::letterbox(&el, 1024, 1024);
    let matting = canvas
        .pixels()
        .map(|p| if p[0] == 255 { 1.0 } else { 0.0 })
        .collect::<Vec<f32>>();
    let matting = TensorData::new(vec![1, 1, 1024, 1024], matting).unwrap();
    // 人像解析 stub：同一人形椭圆，上半（y < 中心）为 13 脸、下半为 5 上衣；
    // letterbox 到 473x473 画布后转 one-hot logits（对应类 +10，其余 -10）
    let mut cls = GrayImage::from_pixel(w, h, Luma([0u8]));
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - fw / 2.0;
            let dy = y as f32 - fh / 2.0;
            if dx * dx / (a * a) + dy * dy / (b * b) <= 1.0 {
                let c = if (y as f32) < fh / 2.0 { 13u8 } else { 5u8 };
                cls.put_pixel(x, y, Luma([c]));
            }
        }
    }
    let cls_rgb = image::ImageBuffer::from_fn(w, h, |x, y| {
        let c = cls.get_pixel(x, y)[0];
        image::Rgb([c, c, c])
    });
    let (p_canvas, _) = crate::preprocess::letterbox(&cls_rgb, 473, 473);
    let hw = 473usize * 473;
    let mut logits = vec![-10.0f32; hw * 20];
    for (i, p) in p_canvas.pixels().enumerate() {
        let c = p[0] as usize;
        if (0..20).contains(&c) {
            logits[c * hw + i] = 10.0;
        }
    }
    let parsing = TensorData::new(vec![1, 20, 473, 473], logits).unwrap();
    FakeEngine::balanced_stub(
        face_out,
        vec![TensorData::new(vec![1, 17, 3], kp).unwrap()],
        vec![matting],
    )
    .stub("parsing_lip", vec![parsing])
}

/// 融合测量角：优先 0.6×双眼角 + 0.4×双肩角；缺失时降级并记录告警
fn fused_measured(kps: &KeypointSet, warnings: &mut Vec<String>) -> f64 {
    let head = kps.eyes().map(|(l, r)| head_angle(&l, &r));
    let shoulder = kps.shoulders().map(|(l, r)| shoulder_angle(&l, &r));
    match (head, shoulder) {
        (Some(h), Some(s)) => fused_angle(h, s),
        (Some(h), None) => {
            warnings.push("未检测到双肩，仅用头部角度".into());
            h
        }
        (None, Some(s)) => {
            warnings.push("未检测到双眼，仅用肩线角度".into());
            s
        }
        (None, None) => {
            warnings.push("未检测到双眼与双肩，跳过自动纠偏".into());
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn 最小闭环输出标准证件照() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        let mut engine = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            rotate: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        // 一寸 295x413
        assert_eq!(result.photos[0].image.dimensions(), (295, 413));
        // 双眼/双肩水平 → 融合角 0 → Auto(0)，无告警
        assert_eq!(result.decision, RotationDecision::Auto(0.0));
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn 手动角度覆盖参与纠偏() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        let mut engine = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            rotate: Some(10.0),
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        assert_eq!(result.decision, RotationDecision::Manual(10.0));
    }

    #[test]
    fn 模型缺失给出中文指引() {
        let mut cfg = Config::default();
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        RgbImage::from_pixel(10, 10, Rgb([0, 0, 0]))
            .save(&input)
            .unwrap();
        // balanced 三件套指向不存在路径且无下载地址 → 自动下载失败给出中文指引（不触发真实网络）
        for id in ["retinaface", "movnet_light", "birefnet_lite"] {
            let spec = cfg.models.get_mut(id).unwrap();
            spec.path = dir.path().join(format!("{id}.onnx")).display().to_string();
            spec.download = None;
        }
        let mut engine = FakeEngine::new(); // 未 stub：load 先自动下载，无地址 → 缺失指引
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            rotate: None,
        };
        let err = run_pipeline(&cfg, &mut engine, &req).unwrap_err();
        assert!(err.to_string().contains("缺失"), "实际：{err}");
    }

    #[test]
    fn 抠图输出尺寸不一致自动对齐() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(50, 60, Rgb([0, 0, 0]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        let mut engine = FakeEngine::balanced_stub(
            // 与真实 RetinaFace 输出顺序一致：[bbox(loc), confidence, landmark]；
            // 完整 16800 候选，仅候选 16001 高分（解码后框接近全图）
            {
                let n = retinaface_prior_count((640, 640));
                let mut conf = vec![0.0f32; n * 2];
                conf[16001 * 2] = 0.01;
                conf[16001 * 2 + 1] = 0.99;
                vec![
                    TensorData::new(vec![1, n as i64, 4], vec![0.0; n * 4]).unwrap(),
                    TensorData::new(vec![1, n as i64, 2], conf).unwrap(),
                    TensorData::new(vec![1, n as i64, 10], vec![0.0; n * 10]).unwrap(),
                ]
            },
            vec![TensorData::new(vec![1, 17, 3], vec![0.0; 17 * 3]).unwrap()],
            // mask 为 1024x1024 letterbox 画布布局（模拟 BiRefNet 输出 ≠ 原图尺寸）→ 逆变换对齐
            vec![TensorData::new(vec![1, 1, 1024, 1024], vec![0.0; 1024 * 1024]).unwrap()],
        );
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            rotate: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        // 对齐后正常出图；mask 全 0（全透明）→ 换底色为纯白
        assert_eq!(result.photos[0].image.dimensions(), (295, 413));
        assert!(
            result.photos[0]
                .image
                .pixels()
                .all(|p| p[0] == 255 && p[1] == 255 && p[2] == 255)
        );
    }

    #[test]
    fn 多底色各出一张且颜色互不相同() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        let mut engine = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into(), "blue".into(), "red".into()],
            rotate: None,
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        assert_eq!(result.photos.len(), 3);
        for p in &result.photos {
            assert_eq!(p.image.dimensions(), (295, 413));
        }
        // 底色顺序与请求一致，画布外区域（左上角）为对应底色
        assert_eq!(result.photos[0].bg, "white");
        assert_eq!(result.photos[1].bg, "blue");
        assert_eq!(result.photos[2].bg, "red");
        assert_eq!(result.photos[0].image.get_pixel(0, 0)[0], 255);
        assert_eq!(result.photos[1].image.get_pixel(0, 0), &Rgb([67, 142, 219]));
        assert_eq!(result.photos[2].image.get_pixel(0, 0), &Rgb([184, 45, 50]));
        // 未请求效果图/排版 → 无多余产物
        assert!(result.effects.is_empty());
        assert!(result.layout.is_none());
    }

    #[test]
    fn 效果图保持全图尺寸并逐底色输出() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        let mut engine = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into(), "blue".into()],
            rotate: None,
            effect: true,
            layout: None,
            beauty: None,
            dress: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        // 效果图 = 旋转后全图尺寸（纠偏角 0 → 原图 100x140）
        assert_eq!(result.effects.len(), 2);
        assert_eq!(result.effects[0].bg, "white");
        assert_eq!(result.effects[0].image.dimensions(), (100, 140));
        assert_eq!(result.effects[1].bg, "blue");
        assert_eq!(result.effects[1].image.dimensions(), (100, 140));
    }

    #[test]
    fn 排版相纸输出() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        let mut engine = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: false,
            layout: Some("6inch".into()),
            beauty: None,
            dress: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        let canvas = result.layout.expect("应有排版相纸");
        assert_eq!(canvas.dimensions(), (1205, 1795));
    }

    #[test]
    fn 空底色列表与未知排版报错() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([0, 0, 0]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        let mut engine = demo_balanced_engine(100, 140);
        // 空底色列表 → 中文报错
        let req = ProcessRequest {
            input: input.clone(),
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec![],
            rotate: None,
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
        };
        let err = run_pipeline(&cfg, &mut engine, &req).unwrap_err();
        assert!(err.to_string().contains("底色"), "实际：{err}");
        // 未知排版 id → 中文报错
        let req2 = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: false,
            layout: Some("b5".into()),
            beauty: None,
            dress: None,
        };
        let err2 = run_pipeline(&cfg, &mut engine, &req2).unwrap_err();
        assert!(err2.to_string().contains("未知排版"), "实际：{err2}");
    }

    #[test]
    fn 美颜开启时证件照与效果图均提亮() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();

        // 基准：不美颜
        let mut engine = demo_balanced_engine(100, 140);
        let base = ProcessRequest {
            input: input.clone(),
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: true,
            layout: None,
            beauty: None,
            dress: None,
        };
        let base = run_pipeline(&cfg, &mut engine, &base).unwrap();

        // 美颜：强磨皮 + 提亮 0.3（全局默认 0.2 被覆盖）
        let mut engine2 = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: true,
            layout: None,
            beauty: Some(BeautyParams {
                enabled: true,
                skin_smooth: Some(1.0),
                brighten: Some(0.3),
                whiten: None, // 未指定 → 取全局配置 0.1
            }),
            dress: None,
        };
        let result = run_pipeline(&cfg, &mut engine2, &req).unwrap();

        // 人像区（椭圆内非纯白背景）像素提亮后更亮：R 10 → 10+76.5 ≈ 86
        let (bx, by) = (150u32, 200u32);
        let b = base.photos[0].image.get_pixel(bx, by);
        let a = result.photos[0].image.get_pixel(bx, by);
        assert!(a[0] > b[0], "证件照人像区美颜后 {a:?} 应亮于基准 {b:?}");
        assert!(a[0] < 255, "人像区不应被提白成背景色：{a:?}");
        // 效果图同样美颜（磨皮/提亮作用于全图换底色前）；效果图为全图尺寸 100x140
        let eb = base.effects[0].image.get_pixel(50, 70);
        let ea = result.effects[0].image.get_pixel(50, 70);
        assert!(ea[0] > eb[0], "效果图人像区美颜后 {ea:?} 应亮于基准 {eb:?}");
    }

    #[test]
    fn 换装正装覆盖衣服区域() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();

        // 基准：不换装
        let mut engine = demo_balanced_engine(100, 140);
        let base = ProcessRequest {
            input: input.clone(),
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: true,
            layout: None,
            beauty: None,
            dress: None,
        };
        let base = run_pipeline(&cfg, &mut engine, &base).unwrap();

        // 换装：程序化藏青正装
        let mut engine2 = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: true,
            layout: None,
            beauty: None,
            dress: Some(DressParams {
                enabled: true,
                garment: None,
                style: Some("suit_navy".into()),
            }),
        };
        let result = run_pipeline(&cfg, &mut engine2, &req).unwrap();

        // 衣服区（下半人形椭圆中心 (50,105)）：换装后为藏青 (31,56,100)，原图为 (10,20,30)
        let pb = *base.effects[0].image.get_pixel(50, 105);
        let pa = *result.effects[0].image.get_pixel(50, 105);
        assert!(pb[2] < 60, "基准衣服区不应为藏青：{pb:?}");
        assert!(pa[2] > 60 && pa[0] < 80, "换装后衣服区应偏藏青，实际 {pa:?}");
        // 效果图尺寸保持全图
        assert_eq!(result.effects[0].image.dimensions(), (100, 140));
    }
}
