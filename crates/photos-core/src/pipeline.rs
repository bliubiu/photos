//! pipeline 编排器：`Photo → Request → Result` 最小闭环（架构文档 §3 九步链路）。
//!
//! M1 范围：单底色、无排版、无美颜；推理输入由 `preprocess::build_input` 按模型
//! `input_dims` 真实构造（letterbox/归一化/布局对齐），引擎可为 `OrtEngine`（真实
//! ONNX 推理）或 `FakeEngine`（mock 回放，测试与 `--demo` 演示用）。

use std::path::PathBuf;

use image::{GrayImage, Luma, RgbImage, RgbaImage};

use crate::config::{BeautyConfig, Config};
use crate::error::{CoreError, CoreResult};
use crate::inference::{FakeEngine, InferenceEngine, TensorData, ensure_models_ready};
use crate::metrics::{StageTimer, TaskMetrics};
use crate::preprocess::{LetterBox, build_input_with, probability_map};
use crate::vision::affine::{rotate_image_same, rotation_affine};
use crate::vision::beauty::{apply_beauty_protected, face_feature_regions, feature_protect_mask};
use crate::vision::blend::{composite, composite_with_image, decontaminate, fit_cover, to_rgba};
use crate::vision::crop::{compute_crop, crop_resize, crop_resize_rgba};
use crate::vision::dressing::{self, SuitStyle};
use crate::vision::face::{
    DecodeTransform, FaceBox, FaceDetection, decode_retinaface, retinaface_prior_count,
};
use crate::vision::geometry::{
    Point2, RotationDecision, SIDE_FACE_YAW_DEG, decide_rotation, fused_angle,
    fused_angle_with_torso, head_angle, shoulder_angle, torso_angle, yaw_from_landmarks,
};
use crate::vision::keypoint::{KeypointSet, decode_movenet};
use crate::vision::matting::{feather, morph_open, threshold_mask};
use crate::vision::mtcnn::{CASCADE_FACE_ID, detect_mtcnn_cascade};

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
/// 自定义背景图产物的底色标识（产物命名 `task_{id}_{size}_custombg.jpg`）
pub const CUSTOM_BG_ID: &str = "custombg";

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
    /// 是否额外输出透明底 PNG（RGBA，alpha 取抠图掩膜）
    pub transparent: bool,
    /// 自定义背景图路径（可选；按证件照尺寸 cover 缩放裁切后与人像合成，额外出一张 `custombg` 产物）
    pub bg_image: Option<PathBuf>,
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
    /// 程序化正装样式（suit_navy | suit_black | shirt_white | suit_full_navy | suit_full_black；无 garment 时生效）
    pub style: Option<String>,
    /// 分部位服装图集合（可选，多图分部位贴合；优先于 `garment`/`style`）
    pub garments: Option<GarmentSet>,
}

/// 分部位服装图集合（上衣/下装/鞋 分别贴合，未提供的部位自动跳过）
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GarmentSet {
    /// 上衣图（覆盖 LIP 5/6/7/10）
    pub top: Option<PathBuf>,
    /// 下装图（覆盖 LIP 8/9）
    pub bottom: Option<PathBuf>,
    /// 鞋图（覆盖 LIP 18/19）
    pub shoes: Option<PathBuf>,
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
    /// 透明底证件照（可选，transparent=true 时；alpha 取羽化掩膜）
    pub transparent: Option<RgbaImage>,
    /// 纠偏决策（含校正角与告警）
    pub decision: RotationDecision,
    /// 处理告警（降级原因等）
    pub warnings: Vec<String>,
    /// 分阶段耗时指标（可观测性；未埋点阶段不记录）
    pub metrics: TaskMetrics,
}

/// 按最大边长等比约束尺寸（`max_side = 0` 表示不限制）；供流水线缩放与调用方构造引擎时对齐
pub fn limited_dimensions(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    if max_side == 0 || w.max(h) <= max_side {
        return (w, h);
    }
    let scale = max_side as f64 / w.max(h) as f64;
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    (nw, nh)
}

/// 输入图等比预缩放（`max_side = 0` 或未超限时原样返回）
fn limit_input_side(img: RgbImage, max_side: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    let (nw, nh) = limited_dimensions(w, h, max_side);
    if (nw, nh) == (w, h) {
        return img;
    }
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

/// 执行单图最小闭环：读图 → 检测/抠图 → 角度决策 → 同步纠偏 → 换底色 → 裁切缩放
pub fn run_pipeline(
    cfg: &Config,
    engine: &mut dyn InferenceEngine,
    req: &ProcessRequest,
) -> CoreResult<PipelineResult> {
    let mut metrics = TaskMetrics::new();
    run_pipeline_with_metrics(cfg, engine, req, &mut metrics)
}

/// 同 [`run_pipeline`]，并额外写入分阶段耗时指标（调用方持有 `metrics` 容器，
/// 失败时仍可读取已记录阶段用于错误上报）
pub fn run_pipeline_with_metrics(
    cfg: &Config,
    engine: &mut dyn InferenceEngine,
    req: &ProcessRequest,
    metrics: &mut TaskMetrics,
) -> CoreResult<PipelineResult> {
    // 1. 解析模式/尺寸/底色列表，并校验该模式模型就绪（缺失给出中文指引）
    // 尺寸与底色支持自定义形式（`px:295x413` / `mm:35x45@300` / `#RRGGBB`），统一归一化为
    // 文件名安全的 id 供落库与产物命名
    let suite = cfg.mode(&req.mode)?;
    let (_, size) = cfg.resolve_size(&req.size)?;
    if req.bgs.is_empty() {
        return Err(CoreError::ConfigValidate("底色列表不能为空".into()));
    }
    let bgs = req
        .bgs
        .iter()
        .map(|id| cfg.resolve_background(id))
        .collect::<CoreResult<Vec<_>>>()?;
    ensure_models_ready(cfg, engine, &req.mode)?;

    // 2. 读图（统一 RGB）；超过最大边长时先等比预缩放，限制峰值内存与推理耗时
    let read_timer = StageTimer::start("读图");
    let img = image::open(&req.input)
        .map_err(|e| CoreError::Image(format!("读取图片 {} 失败：{e}", req.input.display())))?
        .to_rgb8();
    let img = limit_input_side(img, cfg.general.max_input_side);
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(CoreError::Image("图片尺寸为零".into()));
    }
    read_timer.stop(metrics);

    // 3. 推理：RetinaFace 按套件单模型；MTCNN 为完整三级联（p/r/on）
    use crate::vision::mtcnn::cascade_model_ids;
    // 3.1 人体关键点（MoveNet）：预处理 + 推理 + 解码
    let kp_timer = StageTimer::start("人体关键点");
    let kp_spec = cfg.model_spec(&suite.keypoint)?;
    let kp_in = build_input_with(&img, &kp_spec.input_dims, &kp_spec.preprocess)?;
    let kp_outs = engine.run(&suite.keypoint, &kp_in.tensor)?;
    let kps = decode_movenet(&kp_outs[0], w, h)?;
    kp_timer.stop(metrics);
    // 3.2 人像抠图：预处理 + 推理 + 概率掩膜
    let mat_timer = StageTimer::start("人像抠图");
    let mat_spec = cfg.model_spec(&suite.matting)?;
    let mat_in = build_input_with(&img, &mat_spec.input_dims, &mat_spec.preprocess)?;
    let mat_outs = engine.run(&suite.matting, &mat_in.tensor)?;
    let mask = probability_mask(&mat_outs[0], w, h, mat_in.letterbox.as_ref())?;
    mat_timer.stop(metrics);

    // 3.3 人脸检测（RetinaFace 单模型 / MTCNN 三级联）
    let face_timer = StageTimer::start("人脸检测");
    let faces = if suite.face == CASCADE_FACE_ID {
        detect_mtcnn_cascade(engine, &img)?
    } else {
        let face_spec = cfg.model_spec(&suite.face)?;
        let face_in = build_input_with(&img, &face_spec.input_dims, &face_spec.preprocess)?;
        let face_outs = engine.run(&suite.face, &face_in.tensor)?;
        let (scale_x, scale_y, pad_x, pad_y) = match face_in.letterbox {
            Some(lb) => (lb.scale, lb.scale, lb.pad_x, lb.pad_y),
            None => (1.0, 1.0, 0.0, 0.0),
        };
        // 真实 RetinaFace 输出顺序为 [bbox, confidence, landmark]（Hivision 官方模型），
        // decode_retinaface 期望 [scores, boxes, landmarks]，此处按位置重排
        decode_retinaface(
            &face_outs[1],
            &face_outs[0],
            &face_outs[2],
            FACE_SCORE_THRESHOLD,
            NMS_IOU_THRESHOLD,
            DecodeTransform {
                image_size: (
                    face_spec.input_dims[2] as u32,
                    face_spec.input_dims[3] as u32,
                ),
                scale_x,
                scale_y,
                pad_x,
                pad_y,
            },
        )?
    };
    // 级联 id 校验（测试/配置完整性）
    if suite.face == CASCADE_FACE_ID {
        for id in cascade_model_ids() {
            cfg.model_spec(id)?;
        }
    }
    let face = faces
        .first()
        .ok_or_else(|| CoreError::Image("未检测到人脸".into()))?;
    face_timer.stop(metrics);

    // 5. 姿态角度求解（0.6 头部 + 0.4 肩线；髋/膝可用时改用 0.5/0.3/0.2 三路，缺失降级并告警）
    let pose_timer = StageTimer::start("姿态求解");
    let mut warnings = Vec::new();
    let measured = fused_measured(&kps, &mut warnings);
    let decision = decide_rotation(measured, req.rotate)?;
    if let Some(warn) = decision.warning() {
        warnings.push(warn);
    }
    if let Some(warn) = side_face_warning(face) {
        warnings.push(warn);
    }
    pose_timer.stop(metrics);

    // 6. 同步几何纠偏（同一仿射矩阵变换原图与 mask）
    let rotate_timer = StageTimer::start("几何纠偏");
    let (rot_img, rot_mask) = rotate_image_same(&img, &mask, decision.correction())?;
    rotate_timer.stop(metrics);

    // 6.5 换装（可选）：人像解析 → 衣服 mask → 服装贴合（作用于旋转后原图，美颜之前）。
    // 解析模型独立于三模式套件，按需惰性装载（失败给出中文指引）。
    let dressed = match &req.dress {
        Some(d) if d.enabled => {
            let dress_timer = StageTimer::start("换装");
            engine.load(cfg, dressing::PARSING_MODEL_ID, suite.execution_provider)?;
            let p_spec = cfg.model_spec(dressing::PARSING_MODEL_ID)?;
            let p_in = build_input_with(&rot_img, &p_spec.input_dims, &p_spec.preprocess)?;
            let p_outs = engine.run(dressing::PARSING_MODEL_ID, &p_in.tensor)?;
            let parsing = dressing::decode_parsing(
                &p_outs[0],
                rot_img.width(),
                rot_img.height(),
                p_in.letterbox.as_ref(),
            )?;
            let out = if let Some(gs) = &d.garments {
                // 多图分部位贴合：各部位按自身类别独立贴合，未提供部位自动跳过
                let mut images: Vec<RgbImage> = Vec::new();
                let mut specs: Vec<(&[u8], usize)> = Vec::new();
                for (classes, path) in [
                    (dressing::TOP_CLASSES.as_slice(), gs.top.as_ref()),
                    (dressing::BOTTOM_CLASSES.as_slice(), gs.bottom.as_ref()),
                    (dressing::SHOE_CLASSES.as_slice(), gs.shoes.as_ref()),
                ] {
                    if let Some(p) = path {
                        images.push(
                            image::open(p)
                                .map_err(|e| {
                                    CoreError::Image(format!(
                                        "读取服装图 {} 失败：{e}",
                                        p.display()
                                    ))
                                })?
                                .to_rgb8(),
                        );
                        specs.push((classes, images.len() - 1));
                    }
                }
                let parts: Vec<dressing::GarmentPart<'_>> = specs
                    .iter()
                    .map(|(c, i)| dressing::GarmentPart {
                        classes: c,
                        image: &images[*i],
                    })
                    .collect();
                dressing::fit_garment_parts(&rot_img, &parsing, &parts)?
            } else {
                // 程序化全身套装样式（suit_full_*）用全身服装类集（含裤装/腿/鞋），
                // 用户服装图与上半身样式沿用单件语义，避免误覆盖下半身
                let style = d
                    .garment
                    .as_ref()
                    .map(|_| None)
                    .unwrap_or_else(|| {
                        Some(SuitStyle::parse(d.style.as_deref().unwrap_or("suit_navy")))
                    })
                    .transpose()?;
                let clothes = if style.is_some_and(SuitStyle::is_full) {
                    dressing::full_clothes_mask(&parsing)
                } else {
                    dressing::clothes_mask(&parsing)
                };
                let garment = match &d.garment {
                    Some(path) => image::open(path)
                        .map_err(|e| {
                            CoreError::Image(format!("读取服装图 {} 失败：{e}", path.display()))
                        })?
                        .to_rgb8(),
                    None => dressing::formal_suit(style.unwrap_or(SuitStyle::Navy), 240, 360),
                };
                dressing::fit_garment(&rot_img, &garment, &clothes)?
            };
            dress_timer.stop(metrics);
            out
        }
        _ => rot_img.clone(),
    };

    // 6.6 美颜（可选）：分区磨皮——五官（双眼/眉、鼻、嘴）保护区不磨皮，保留五官锐度；
    // 美颜不改变 mask 与裁剪框
    let beautified = match &req.beauty {
        Some(p) if p.enabled => {
            let beauty_timer = StageTimer::start("美颜");
            let protect = beauty_protect_mask(face, w, h, decision.correction());
            let out = apply_beauty_protected(
                &dressed,
                &BeautyConfig {
                    enabled: true,
                    skin_smooth: p.skin_smooth.unwrap_or(cfg.beauty.skin_smooth),
                    brighten: p.brighten.unwrap_or(cfg.beauty.brighten),
                    whiten: p.whiten.unwrap_or(cfg.beauty.whiten),
                },
                Some(&protect),
            );
            beauty_timer.stop(metrics);
            out
        }
        _ => dressed,
    };

    // 7. 换底色（mask 羽化 + 边缘去色边后逐像素 alpha 混合；廉价操作只做一次检测/抠图/纠偏）
    // 7.1 裁剪框与底色无关，先算一次
    let bg_timer = StageTimer::start("换底裁切");
    let crop = compute_crop(
        &face.face,
        w,
        h,
        size.width_px,
        size.height_px,
        CROP_TOP_RATIO,
        CROP_BOTTOM_RATIO,
    )?;
    // 7.2 羽化掩膜 + 边缘去色边（按估计的原始背景色解混半透明边缘，抑制白边/黑边/底色残留）
    let alpha = feather(&rot_mask, MASK_FEATHER_SIGMA);
    let cleaned = decontaminate(&beautified, &alpha);
    let mut photos = Vec::with_capacity(bgs.len() + 1);
    let mut effects = Vec::new();
    for (bg_id, bg) in &bgs {
        // 效果图：换底后保持旋转全图尺寸
        let composed = composite(&cleaned, &alpha, bg.rgb);
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

    // 7.3 自定义背景图（可选）：按原图尺寸 cover 缩放裁切后与人像合成，产物底色标识 custombg
    if let Some(path) = &req.bg_image {
        let bg_img = image::open(path)
            .map_err(|e| CoreError::Image(format!("读取背景图 {} 失败：{e}", path.display())))?
            .to_rgb8();
        let canvas = fit_cover(&bg_img, w, h);
        let composed = composite_with_image(&cleaned, &alpha, &canvas);
        let final_img = crop_resize(&composed, &crop, size.width_px, size.height_px)?;
        photos.push(BgOutput {
            bg: CUSTOM_BG_ID.to_string(),
            image: final_img,
        });
    }

    // 7.4 透明底证件照（可选）：RGB 取去色边后前景，alpha 取羽化掩膜（PNG 输出）
    let transparent = if req.transparent {
        let rgba = to_rgba(&cleaned, &alpha);
        Some(crop_resize_rgba(
            &rgba,
            &crop,
            size.width_px,
            size.height_px,
        )?)
    } else {
        None
    };
    bg_timer.stop(metrics);

    // 8. 排版相纸（可选）：以首个底色证件照按相纸规格铺版
    let layout_timer = StageTimer::start("排版");
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
    // 排版为可选阶段：未指定相纸时不记录该阶段耗时
    if req.layout.is_some() {
        layout_timer.stop(metrics);
    }

    Ok(PipelineResult {
        photos,
        effects,
        layout: layout_img,
        transparent,
        decision,
        warnings,
        metrics: metrics.clone(),
    })
}

/// 五官保护掩膜：检测结果位于原图坐标系，美颜作用于纠偏后图像，故按同一旋转矩阵把
/// 五官关键点变换到纠偏后坐标系再生成保护掩膜（旋转保距，人脸框尺寸不变、中心随变换移动）
fn beauty_protect_mask(face: &FaceDetection, w: u32, h: u32, deg: f64) -> GrayImage {
    let m = rotation_affine(w as f64 / 2.0, h as f64 / 2.0, deg);
    let landmarks = face.landmarks.map(|p| {
        let (x, y) = m * (p.x as f32, p.y as f32);
        Point2::new(x as f64, y as f64)
    });
    let center = face.face.center();
    let (cx, cy) = m * (center.x as f32, center.y as f32);
    let (fw, fh) = (face.face.width(), face.face.height());
    let rotated_face = FaceBox {
        x1: cx - fw / 2.0,
        y1: cy - fh / 2.0,
        x2: cx + fw / 2.0,
        y2: cy + fh / 2.0,
        score: face.face.score,
    };
    feature_protect_mask(w, h, &face_feature_regions(&rotated_face, &landmarks))
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
    // 人像解析 stub：同一人形椭圆，自上而下 0..0.30h 脸(13)、0.30..0.60h 上衣(5)、
    // 0.60..0.85h 裤子(8)、0.85h 以下鞋(19)，可演示上半身与全身套装两种换装。
    // 关键：类别图直接在 473x473 画布坐标系生成（像素反算回原图坐标判定），
    // 避免「灰度编码 + Triangle 插值」在类别边界产生假类别（真实模型为 one-hot logits，
    // argmax 后类别干净，不受插值污染），随后转 one-hot logits（对应类 +10，其余 -10）
    let (tw, th) = (473u32, 473u32);
    let lb_scale = (tw as f32 / w.max(1) as f32)
        .min(th as f32 / h.max(1) as f32)
        .max(1e-6);
    let new_w = (w as f32 * lb_scale).round().max(1.0) as u32;
    let new_h = (h as f32 * lb_scale).round().max(1.0) as u32;
    let pad_x = ((tw - new_w) / 2) as f32;
    let pad_y = ((th - new_h) / 2) as f32;
    let mut cls_canvas = GrayImage::from_pixel(tw, th, Luma([0u8]));
    for y in 0..th {
        for x in 0..tw {
            // 画布坐标 → 原图坐标（内容区反缩放，pad 区落回原图外侧）
            let fx = (x as f32 - pad_x) / lb_scale;
            let fy = (y as f32 - pad_y) / lb_scale;
            let dx = fx - fw / 2.0;
            let dy = fy - fh / 2.0;
            if dx * dx / (a * a) + dy * dy / (b * b) <= 1.0 {
                let c = if fy < 0.30 * fh {
                    13u8
                } else if fy < 0.60 * fh {
                    5u8
                } else if fy < 0.85 * fh {
                    8u8
                } else {
                    19u8
                };
                cls_canvas.put_pixel(x, y, Luma([c]));
            }
        }
    }
    let hw = tw as usize * th as usize;
    let mut logits = vec![-10.0f32; hw * 20];
    for (i, p) in cls_canvas.pixels().enumerate() {
        let c = p[0] as usize;
        if (0..20).contains(&c) {
            logits[c * hw + i] = 10.0;
        }
    }
    let parsing = TensorData::new(vec![1, 20, tw as i64, th as i64], logits).unwrap();
    FakeEngine::balanced_stub(
        face_out,
        vec![TensorData::new(vec![1, 17, 3], kp).unwrap()],
        vec![matting],
    )
    .stub("parsing_lip", vec![parsing])
}

/// 融合测量角：优先 0.6×双眼角 + 0.4×双肩角；髋/膝可用时改用三路融合；缺失时降级并记录告警
fn fused_measured(kps: &KeypointSet, warnings: &mut Vec<String>) -> f64 {
    let head = kps.eyes().map(|(l, r)| head_angle(&l, &r));
    let shoulder = kps.shoulders().map(|(l, r)| shoulder_angle(&l, &r));
    // 躯干垂直度：肩中点 → 髋（缺失退回膝）中点，二者缺一时无法求解
    let torso = match (kps.shoulder_mid(), kps.lower_mid()) {
        (Some(s), Some(lower)) => Some(torso_angle(&s, &lower)),
        _ => None,
    };
    match (head, shoulder, torso) {
        // 髋/膝可用 → 三路融合，额外修复高低肩之外的侧身倾斜
        (Some(h), Some(s), Some(t)) => fused_angle_with_torso(h, s, t),
        (Some(h), Some(s), None) => fused_angle(h, s),
        (Some(h), None, _) => {
            warnings.push("未检测到双肩，仅用头部角度".into());
            h
        }
        (None, Some(s), _) => {
            warnings.push("未检测到双眼，仅用肩线角度".into());
            s
        }
        (None, None, _) => {
            warnings.push("未检测到双眼与双肩，跳过自动纠偏".into());
            0.0
        }
    }
}

/// 侧脸告警：由人脸 5 点关键点（左眼、右眼、鼻尖）估算 yaw，超过阈值时返回中文提示。
/// 关键点退化（双眼重合，如演示/桩数据）时无法判断，返回 None 不告警。
fn side_face_warning(face: &FaceDetection) -> Option<String> {
    let yaw = yaw_from_landmarks(&face.landmarks[0], &face.landmarks[1], &face.landmarks[2])?;
    (yaw.abs() > SIDE_FACE_YAW_DEG)
        .then(|| format!("疑似侧脸（估算偏转 {yaw:.0}°），建议提供正面照"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn 美颜五官保护掩膜随纠偏角同步变换() {
        let face = FaceDetection {
            face: FaceBox {
                x1: 100.0,
                y1: 100.0,
                x2: 200.0,
                y2: 200.0,
                score: 0.99,
            },
            landmarks: [
                Point2::new(125.0, 140.0),
                Point2::new(175.0, 140.0),
                Point2::new(150.0, 165.0),
                Point2::new(133.0, 185.0),
                Point2::new(167.0, 185.0),
            ],
        };
        // 未纠偏：五官保护区落在原坐标
        let mask = beauty_protect_mask(&face, 300, 300, 0.0);
        assert!(mask.get_pixel(125, 140)[0] > 200, "左眼应被保护");
        assert!(mask.get_pixel(150, 185)[0] > 200, "嘴中心应被保护");
        // 绕图像中心 (150,150) 顺时针 90°：(x,y) → (150-(y-150), 150+(x-150))
        // 左眼 (125,140) → (160,125)；嘴中心 (150,185) → (115,150)
        let rotated = beauty_protect_mask(&face, 300, 300, 90.0);
        for (name, x, y) in [("左眼", 160u32, 125u32), ("嘴中心", 115, 150)] {
            assert!(
                rotated.get_pixel(x, y)[0] > 200,
                "纠偏后{name}({x},{y})应被保护，实际 {}",
                rotated.get_pixel(x, y)[0]
            );
        }
        // 远离五官的背景不受保护
        assert_eq!(rotated.get_pixel(280, 20)[0], 0);
    }

    #[test]
    fn 躯干垂直度参与三路姿态融合() {
        use crate::vision::keypoint::{
            LEFT_EYE, LEFT_HIP, LEFT_SHOULDER, RIGHT_EYE, RIGHT_HIP, RIGHT_SHOULDER,
        };
        let mut kps = KeypointSet { points: [None; 17] };
        // 双眼、双肩水平（角度 0）
        kps.points[LEFT_EYE] = Some(Point2::new(40.0, 40.0));
        kps.points[RIGHT_EYE] = Some(Point2::new(60.0, 40.0));
        kps.points[LEFT_SHOULDER] = Some(Point2::new(20.0, 100.0));
        kps.points[RIGHT_SHOULDER] = Some(Point2::new(80.0, 100.0));
        let mut w = Vec::new();
        // 无髋/膝 → 退化为两路（0.6×0 + 0.4×0 = 0），无告警
        assert!(fused_measured(&kps, &mut w).abs() < 1e-9);
        assert!(w.is_empty());
        // 髋中点在肩中点左侧 31.7px（垂直距离 180px）→ 躯干倾斜 atan(31.7/180) ≈ 10° → 0.2×10 = 2.0
        let dy = 180.0f64;
        let dx = dy * 10.0f64.to_radians().tan();
        let hip_mid_x = 50.0 - dx;
        kps.points[LEFT_HIP] = Some(Point2::new(hip_mid_x - 5.0, 280.0));
        kps.points[RIGHT_HIP] = Some(Point2::new(hip_mid_x + 5.0, 280.0));
        let measured = fused_measured(&kps, &mut w);
        assert!((measured - 2.0).abs() < 0.02, "实际 {measured}");
        assert!(w.is_empty(), "髋部可用时不应告警");
    }

    #[test]
    fn 侧脸超阈值告警且退化不告警() {
        let face_box = FaceBox {
            x1: 100.0,
            y1: 100.0,
            x2: 200.0,
            y2: 200.0,
            score: 0.99,
        };
        let frontal = FaceDetection {
            face: face_box,
            landmarks: [
                Point2::new(125.0, 140.0),
                Point2::new(175.0, 140.0),
                Point2::new(150.0, 165.0),
                Point2::new(133.0, 185.0),
                Point2::new(167.0, 185.0),
            ],
        };
        assert!(side_face_warning(&frontal).is_none(), "正面照不应告警");
        // 鼻尖右移 30px（半间距 25px）→ asin(1.2) 钳制 → 90° → 告警
        let side = FaceDetection {
            landmarks: [
                Point2::new(125.0, 140.0),
                Point2::new(175.0, 140.0),
                Point2::new(180.0, 165.0),
                Point2::new(133.0, 185.0),
                Point2::new(167.0, 185.0),
            ],
            ..frontal
        };
        let warn = side_face_warning(&side).unwrap();
        assert!(
            warn.contains("疑似侧脸") && warn.contains("正面照"),
            "{warn}"
        );
        // 双眼重合（演示/桩数据退化）→ 无法判断，不告警
        let degenerate = FaceDetection {
            landmarks: [
                Point2::new(150.0, 140.0),
                Point2::new(150.0, 140.0),
                Point2::new(150.0, 165.0),
                Point2::new(133.0, 185.0),
                Point2::new(167.0, 185.0),
            ],
            ..frontal
        };
        assert!(side_face_warning(&degenerate).is_none());
    }

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
            transparent: false,
            bg_image: None,
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
    fn 分阶段耗时指标随流水线采集() {
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
            transparent: false,
            bg_image: None,
            rotate: None,
        };
        let mut metrics = TaskMetrics::new();
        let result = run_pipeline_with_metrics(&cfg, &mut engine, &req, &mut metrics).unwrap();
        // 必经阶段全部记录，且与结果内指标一致
        let stages: Vec<&str> = metrics.stages.iter().map(|s| s.stage.as_str()).collect();
        for expect in [
            "读图",
            "人体关键点",
            "人像抠图",
            "人脸检测",
            "姿态求解",
            "几何纠偏",
            "换底裁切",
        ] {
            assert!(stages.contains(&expect), "缺少阶段「{expect}」：{stages:?}");
        }
        // 可选阶段未启用时不记录
        assert!(!stages.contains(&"排版"), "未指定相纸不应记录排版阶段");
        assert!(!stages.contains(&"换装"), "未启用换装不应记录换装阶段");
        assert!(!stages.contains(&"美颜"), "未启用美颜不应记录美颜阶段");
        assert!(metrics.total_ms() > 0.0, "总耗时应为正");
        assert!(result.metrics.summary().contains("人脸检测"));
    }

    #[test]
    fn speed模式mtcnn解码闭环() {
        // speed 套件 face=mtcnn（级联）：三级 P/R/O stub，验证按套件走 MTCNN 级联分支
        let cfg = Config::default();
        // 30x30 短边 < 40 → 金字塔仅 1 层，P-Net 只调用一次，便于构造固定输出
        let img = RgbImage::from_pixel(30, 30, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();

        let fh = 54usize;
        let fw = 54usize;
        let mut hm = vec![0.0f32; 2 * fh * fw];
        let reg = vec![0.0f32; 4 * fh * fw];
        // 单个高置信 cell：解码得框 (9,9)-(20,20)，落在 30x30 图内
        hm[fh * fw + 4 * fw + 4] = 0.99;
        let pnet_out = vec![
            TensorData::new(vec![1, 2, fh as i64, fw as i64], hm).unwrap(),
            TensorData::new(vec![1, 4, fh as i64, fw as i64], reg).unwrap(),
        ];
        // R-Net 单框：score 过阈值，回归 0
        let rnet_out = vec![
            TensorData::new(vec![1], vec![0.9]).unwrap(),
            TensorData::new(vec![1, 4], vec![0.0; 4]).unwrap(),
        ];
        // O-Net 单框：score + 回归 0 + 5 点 landmark（相对归一化）
        let onet_out = vec![
            TensorData::new(vec![1], vec![0.9]).unwrap(),
            TensorData::new(vec![1, 4], vec![0.0; 4]).unwrap(),
            TensorData::new(vec![1, 10], vec![0.5; 10]).unwrap(),
        ];
        let mut kp_v = vec![0.0f32; 17 * 3];
        for (i, (x, y)) in [
            (2usize, (12.0f32, 12.0)),
            (3, (18.0, 12.0)),
            (5, (9.0, 22.0)),
            (6, (21.0, 22.0)),
        ] {
            // MoveNet 布局 (y, x, score)，归一化 [0,1] 相对 30x30
            kp_v[i * 3] = y / 30.0;
            kp_v[i * 3 + 1] = x / 30.0;
            kp_v[i * 3 + 2] = 0.95;
        }
        // 与 demo 一致：先在原图画前景，再 letterbox 到 1024（rmbg input_dims）
        let mut el = RgbImage::from_pixel(30, 30, Rgb([0, 0, 0]));
        for y in 5..25 {
            for x in 5..25 {
                el.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        let (canvas, _) = crate::preprocess::letterbox(&el, 1024, 1024);
        let matting = canvas
            .pixels()
            .map(|p| if p[0] == 255 { 1.0 } else { 0.0 })
            .collect::<Vec<f32>>();
        let mut engine = FakeEngine::new()
            .stub("mtcnn_pnet", pnet_out)
            .stub("mtcnn_rnet", rnet_out)
            .stub("mtcnn_onet", onet_out)
            .stub(
                "movnet_light",
                vec![TensorData::new(vec![1, 17, 3], kp_v).unwrap()],
            )
            .stub(
                "rmbg",
                vec![TensorData::new(vec![1, 1, 1024, 1024], matting).unwrap()],
            );

        let req = ProcessRequest {
            input,
            mode: "speed".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            transparent: false,
            bg_image: None,
            rotate: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        assert_eq!(result.photos[0].image.dimensions(), (295, 413));
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
    fn 尺寸约束按最大边长等比计算() {
        // 0 或未超限：原样返回
        assert_eq!(limited_dimensions(400, 600, 0), (400, 600));
        assert_eq!(limited_dimensions(400, 600, 600), (400, 600));
        assert_eq!(limited_dimensions(400, 600, 1000), (400, 600));
        // 长边超限：等比缩到最大边长
        assert_eq!(limited_dimensions(400, 600, 100), (67, 100));
        assert_eq!(limited_dimensions(600, 400, 300), (300, 200));
    }

    #[test]
    fn 输入图超过最大边长时预缩放后再处理() {
        let mut cfg = Config::default();
        cfg.general.max_input_side = 100;
        let img = RgbImage::from_pixel(400, 600, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        // demo 引擎按缩放后尺寸构造（与流水线内部缩放保持一致）
        let (w, h) = limited_dimensions(400, 600, cfg.general.max_input_side);
        let mut engine = demo_balanced_engine(w, h);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: true,
            layout: None,
            beauty: None,
            dress: None,
            transparent: false,
            bg_image: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        // 效果图与全链路均基于缩放后尺寸
        assert_eq!(result.effects[0].image.dimensions(), (67, 100));
        assert_eq!(result.photos[0].image.dimensions(), (295, 413));
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
            transparent: false,
            bg_image: None,
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
                garments: None,
            }),
            transparent: false,
            bg_image: None,
        };
        let result = run_pipeline(&cfg, &mut engine2, &req).unwrap();

        // 上衣区（人形 0.30h..0.60h 区间内 (50,60)）：换装后为藏青 (31,56,100)，原图为 (10,20,30)
        let pb = *base.effects[0].image.get_pixel(50, 60);
        let pa = *result.effects[0].image.get_pixel(50, 60);
        assert!(pb[2] < 60, "基准上衣区不应为藏青：{pb:?}");
        assert!(
            pa[2] > 60 && pa[0] < 80,
            "换装后上衣区应偏藏青，实际 {pa:?}"
        );
        // 效果图尺寸保持全图
        assert_eq!(result.effects[0].image.dimensions(), (100, 140));
    }

    #[test]
    fn 全身套装覆盖裤装与鞋区() {
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
            transparent: false,
            bg_image: None,
        };
        let base = run_pipeline(&cfg, &mut engine, &base).unwrap();

        // 换装：程序化藏青全身套装
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
                style: Some("suit_full_navy".into()),
                garments: None,
            }),
            transparent: false,
            bg_image: None,
        };
        let result = run_pipeline(&cfg, &mut engine2, &req).unwrap();

        // 裤区（人形 0.60h..0.85h 区间 (50,105)）：深藏青西裤，区别于原图灰
        let pb = *base.effects[0].image.get_pixel(50, 105);
        let pa = *result.effects[0].image.get_pixel(50, 105);
        assert!(pa[2] >= 25 && pa[2] <= 60, "裤区应深藏青，实际 {pa:?}");
        assert!(pa != pb, "裤区应被西裤覆盖：{pb:?} → {pa:?}");
        // 鞋区（椭圆下缘 (50,121)）：黑皮鞋，接近全黑
        let sa = *result.effects[0].image.get_pixel(50, 121);
        assert!(
            sa[0] < 60 && sa[1] < 60 && sa[2] < 60,
            "鞋区应偏黑，实际 {sa:?}"
        );
        // 效果图尺寸保持全图
        assert_eq!(result.effects[0].image.dimensions(), (100, 140));
    }

    #[test]
    fn 多图分部位换装分别覆盖上下身() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        // 上衣图纯红、下装图纯蓝
        let top = RgbImage::from_pixel(120, 120, Rgb([200, 30, 30]));
        let bottom = RgbImage::from_pixel(120, 120, Rgb([30, 30, 200]));
        let top_path = dir.path().join("top.jpg");
        let bottom_path = dir.path().join("bottom.jpg");
        top.save(&top_path).unwrap();
        bottom.save(&bottom_path).unwrap();

        let mut engine = demo_balanced_engine(100, 140);
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
                style: None,
                garments: Some(GarmentSet {
                    top: Some(top_path),
                    bottom: Some(bottom_path),
                    shoes: None,
                }),
            }),
            transparent: false,
            bg_image: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();

        // 上衣区 (50,60) 偏红、裤区 (50,105) 偏蓝，互不串位
        let pa = *result.effects[0].image.get_pixel(50, 60);
        assert!(pa[0] > 150 && pa[2] < 80, "上衣应偏红，实际 {pa:?}");
        let pb = *result.effects[0].image.get_pixel(50, 105);
        assert!(pb[2] > 150 && pb[0] < 80, "下装应偏蓝，实际 {pb:?}");
        assert_eq!(result.effects[0].image.dimensions(), (100, 140));
    }

    #[test]
    fn 透明底输出携带alpha通道() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();

        let mut engine = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input: input.clone(),
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            transparent: true,
            bg_image: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        let rgba = result.transparent.expect("应输出透明底证件照");
        assert_eq!(rgba.dimensions(), (295, 413));
        // 人像中心不透明、画布角落全透明
        assert_eq!(rgba.get_pixel(147, 206)[3], 255);
        assert_eq!(rgba.get_pixel(0, 0)[3], 0);

        // 未请求时不产生透明底产物
        let mut engine2 = demo_balanced_engine(100, 140);
        let req2 = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            transparent: false,
            bg_image: None,
        };
        assert!(
            run_pipeline(&cfg, &mut engine2, &req2)
                .unwrap()
                .transparent
                .is_none()
        );
    }

    #[test]
    fn 自定义背景图替换额外出图() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        // 纯红背景图（200x200，cover 缩放到证件照尺寸后仍为纯色）
        let bg_path = dir.path().join("bg.png");
        RgbImage::from_pixel(200, 200, Rgb([200, 30, 30]))
            .save(&bg_path)
            .unwrap();

        let mut engine = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            transparent: false,
            bg_image: Some(bg_path),
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        // 纯色底色 1 张 + 自定义背景 1 张
        assert_eq!(result.photos.len(), 2);
        assert_eq!(result.photos[0].bg, "white");
        assert_eq!(result.photos[1].bg, CUSTOM_BG_ID);
        let custom = &result.photos[1].image;
        assert_eq!(custom.dimensions(), (295, 413));
        // 画布角落为自定义背景图颜色，人像区保留原图前景
        assert_eq!(custom.get_pixel(0, 0), &Rgb([200, 30, 30]));
        assert_ne!(custom.get_pixel(147, 206), &Rgb([200, 30, 30]));
    }

    #[test]
    fn 背景图路径非法报错() {
        let cfg = Config::default();
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]))
            .save(&input)
            .unwrap();
        let mut engine = demo_balanced_engine(100, 140);
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bgs: vec!["white".into()],
            rotate: None,
            effect: false,
            layout: None,
            beauty: None,
            dress: None,
            transparent: false,
            bg_image: Some(dir.path().join("不存在.png")),
        };
        let err = run_pipeline(&cfg, &mut engine, &req).unwrap_err();
        assert!(err.to_string().contains("读取背景图"), "实际：{err}");
    }
}
