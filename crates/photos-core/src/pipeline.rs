//! pipeline 编排器：`Photo → Request → Result` 最小闭环（架构文档 §3 九步链路）。
//!
//! M1 范围：单底色、无排版、无美颜；推理输出由引擎注入（默认 `FakeEngine` mock 回放，
//! 集成测试用 `balanced_stub` 打通端到端）。真实前处理（letterbox/归一化）与输入形状
//! 对齐、多底色/排版在 M2 实现，故 M1 推理输入为最小占位张量。

use std::path::PathBuf;

use image::{GrayImage, RgbImage};

use crate::config::Config;
use crate::error::{CoreError, CoreResult};
use crate::inference::{FakeEngine, InferenceEngine, TensorData, ensure_models_ready};
use crate::vision::affine::rotate_image_same;
use crate::vision::blend::composite_feathered;
use crate::vision::crop::{compute_crop, crop_resize};
use crate::vision::face::decode_retinaface;
use crate::vision::geometry::{decide_rotation, fused_angle, head_angle, shoulder_angle, RotationDecision};
use crate::vision::keypoint::{decode_movenet, KeypointSet};
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

/// 处理请求（单图）
#[derive(Debug, Clone)]
pub struct ProcessRequest {
    /// 输入图片路径
    pub input: PathBuf,
    /// 运行模式 id（speed | balanced | quality）
    pub mode: String,
    /// 尺寸标准 id（如 one_inch）
    pub size: String,
    /// 底色 id（如 white）
    pub bg: String,
    /// 手动纠偏角度（度，可选）
    pub rotate: Option<f64>,
}

/// 处理结果（最终证件照 + 元数据）
#[derive(Debug)]
pub struct PipelineResult {
    /// 最终证件照（RGB）
    pub image: RgbImage,
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
    // 1. 解析模式/尺寸/底色，并校验该模式模型就绪（缺失给出中文指引）
    let suite = cfg.mode(&req.mode)?;
    let size = cfg.size(&req.size)?;
    let bg = cfg.background(&req.bg)?;
    ensure_models_ready(cfg, engine, &req.mode)?;

    // 2. 读图（统一 RGB）
    let img = image::open(&req.input)
        .map_err(|e| CoreError::Image(format!("读取图片 {} 失败：{e}", req.input.display())))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(CoreError::Image("图片尺寸为零".into()));
    }

    // 3. 推理（M1 占位输入；mock 引擎忽略输入直接回放）
    let face_outs = engine.run(&suite.face, &placeholder())?;
    let kp_outs = engine.run(&suite.keypoint, &placeholder())?;
    let mat_outs = engine.run(&suite.matting, &placeholder())?;

    // 4. 解码：人脸框 + 5 关键点、17 关键点、概率 mask
    let faces = decode_retinaface(
        &face_outs[0],
        &face_outs[1],
        &face_outs[2],
        FACE_SCORE_THRESHOLD,
        NMS_IOU_THRESHOLD,
        1.0,
        1.0,
        0.0,
        0.0,
    )?;
    let face = faces
        .first()
        .ok_or_else(|| CoreError::Image("未检测到人脸".into()))?;
    let kps = decode_movenet(&kp_outs[0], w, h)?;
    let mask = probability_mask(&mat_outs[0], w, h)?;

    // 5. 姿态角度求解（0.6 头部 + 0.4 肩线，缺失降级并告警）
    let mut warnings = Vec::new();
    let measured = fused_measured(&kps, &mut warnings);
    let decision = decide_rotation(measured, req.rotate)?;
    if let Some(warn) = decision.warning() {
        warnings.push(warn);
    }

    // 6. 同步几何纠偏（同一仿射矩阵变换原图与 mask）
    let (rot_img, rot_mask) = rotate_image_same(&img, &mask, decision.correction())?;

    // 7. 换底色（mask 羽化后逐像素 alpha 混合）
    let composed = composite_feathered(&rot_img, &rot_mask, bg.rgb, MASK_FEATHER_SIGMA);

    // 8. 裁剪 + 缩放（人脸框在原图坐标；M1 小角度近似不随旋转变换）
    let crop = compute_crop(
        &face.face,
        w,
        h,
        size.width_px,
        size.height_px,
        CROP_TOP_RATIO,
        CROP_BOTTOM_RATIO,
    )?;
    let final_img = crop_resize(&composed, &crop, size.width_px, size.height_px)?;

    Ok(PipelineResult {
        image: final_img,
        decision,
        warnings,
    })
}

/// M1 占位输入张量（最小形状；真实前处理在 M2 对齐模型 input_dims）
fn placeholder() -> TensorData {
    TensorData {
        shape: vec![1],
        data: vec![0.0],
    }
}

/// 演示引擎：balanced 三件套内置 mock 回放（`photos process --demo` 与测试共用）。
/// 人脸框居中、双眼/双肩水平（融合角 0）、mask 为中心椭圆（可演示换底色）。
pub fn demo_balanced_engine(w: u32, h: u32) -> FakeEngine {
    let (fw, fh) = (w as f32, h as f32);
    let face_out = vec![
        TensorData::new(vec![1, 1], vec![0.99]).unwrap(),
        TensorData::new(vec![1, 1, 4], vec![0.3 * fw, 0.3 * fh, 0.7 * fw, 0.7 * fh]).unwrap(),
        // 左眼、右眼、鼻尖、左嘴角、右嘴角
        TensorData::new(
            vec![1, 1, 10],
            vec![
                0.44 * fw, 0.40 * fh, 0.56 * fw, 0.40 * fh, 0.50 * fw, 0.45 * fh,
                0.45 * fw, 0.52 * fh, 0.55 * fw, 0.52 * fh,
            ],
        )
        .unwrap(),
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
    // mask：中心椭圆不透明（a=0.32w、b=0.38h），边缘透明以演示换底色
    let cx = fw / 2.0;
    let cy = fh / 2.0;
    let a = 0.32 * fw;
    let b = 0.38 * fh;
    let mut matting = vec![0.0f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if dx * dx / (a * a) + dy * dy / (b * b) <= 1.0 {
                matting[(y * w + x) as usize] = 1.0;
            }
        }
    }
    let matting = TensorData::new(vec![1, 1, h as i64, w as i64], matting).unwrap();
    FakeEngine::balanced_stub(face_out, vec![TensorData::new(vec![1, 17, 3], kp).unwrap()], vec![matting])
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

/// 概率 mask 张量 `[1,1,H,W]`（行主序）→ 灰度图 [H,W]（×255 后阈值化 + 形态学去噪）
fn probability_mask(out: &TensorData, w: u32, h: u32) -> CoreResult<GrayImage> {
    let n = out.shape.len();
    let out_w = out.dim(n - 1);
    let out_h = out.dim(n - 2);
    if out_w != w as i64 || out_h != h as i64 {
        return Err(CoreError::Image(format!(
            "抠图输出尺寸 {out_w}x{out_h} 与原图 {w}x{h} 不一致"
        )));
    }
    let mut mask = GrayImage::new(w, h);
    for (i, p) in mask.pixels_mut().enumerate() {
        let v = out.data.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        p[0] = (v * 255.0).round() as u8;
    }
    let bin = threshold_mask(&mask, MASK_THRESHOLD);
    Ok(morph_open(&bin, MASK_MORPH_RADIUS))
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
            bg: "white".into(),
            rotate: None,
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        // 一寸 295x413
        assert_eq!(result.image.dimensions(), (295, 413));
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
            bg: "white".into(),
            rotate: Some(10.0),
        };
        let result = run_pipeline(&cfg, &mut engine, &req).unwrap();
        assert_eq!(result.decision, RotationDecision::Manual(10.0));
    }

    #[test]
    fn 模型缺失给出中文指引() {
        let cfg = Config::default();
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        RgbImage::from_pixel(10, 10, Rgb([0, 0, 0])).save(&input).unwrap();
        let mut engine = FakeEngine::new(); // 未 stub：load 按磁盘校验，models 目录不存在 → 缺失
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bg: "white".into(),
            rotate: None,
        };
        let err = run_pipeline(&cfg, &mut engine, &req).unwrap_err();
        assert!(err.to_string().contains("缺失"), "实际：{err}");
    }

    #[test]
    fn 抠图尺寸不一致报错() {
        let cfg = Config::default();
        let img = RgbImage::from_pixel(50, 60, Rgb([0, 0, 0]));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.jpg");
        img.save(&input).unwrap();
        let mut engine = FakeEngine::balanced_stub(
            vec![
                TensorData::new(vec![1, 1], vec![0.99]).unwrap(),
                TensorData::new(vec![1, 1, 4], vec![10.0, 10.0, 30.0, 40.0]).unwrap(),
                TensorData::new(vec![1, 1, 10], vec![0.0; 10]).unwrap(),
            ],
            vec![TensorData::new(vec![1, 17, 3], vec![0.0; 17 * 3]).unwrap()],
            // mask 尺寸与原图不符（60x50 vs 50x60）
            vec![TensorData::new(vec![1, 1, 50, 60], vec![0.0; 50 * 60]).unwrap()],
        );
        let req = ProcessRequest {
            input,
            mode: "balanced".into(),
            size: "one_inch".into(),
            bg: "white".into(),
            rotate: None,
        };
        let err = run_pipeline(&cfg, &mut engine, &req).unwrap_err();
        assert!(err.to_string().contains("不一致"), "实际：{err}");
    }
}
