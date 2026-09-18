//! 工作流编排：把流水线拆成「声明式步骤表」，支持步骤开关、顺序调整与自定义步骤。
//!
//! - 内置 10 步（[`builtin_steps`]），默认顺序与改造前的固定链路完全一致（产物契约不变）；
//! - 步骤名 → 实现函数由 [`resolve_step`] 解析：内置表 → `[pipeline.custom.<名>] op` 映射
//!   → 运行期 [`register_step`] 注册表（进程内，重启失效）；
//! - 每个步骤自带 [`StageTimer`]，可选步骤未启用时不记录阶段耗时（指标口径与既有实现一致）；
//! - 降级：缺「人脸检测」→ 裁切按图像居中；缺「人像抠图」→ 跳过换底合成、仍出图并追加中文告警。

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use image::{GrayImage, RgbImage, RgbaImage};
use serde::Serialize;

use crate::config::{BackgroundSpec, Config, ModeSuite, SizeSpec};
use crate::error::{CoreError, CoreResult};
use crate::inference::{InferenceEngine, TensorData};
use crate::metrics::{StageTimer, TaskMetrics};
use crate::pipeline::{BgOutput, CUSTOM_BG_ID, ProcessRequest};
use crate::preprocess::{LetterBox, build_input_with, probability_map};
use crate::vision::affine::{rotate_image, rotate_image_same, rotation_affine};
use crate::vision::beauty::{apply_beauty_protected, face_feature_regions, feature_protect_mask};
use crate::vision::blend::{composite, composite_with_image, decontaminate, fit_cover, to_rgba};
use crate::vision::crop::{compute_crop, crop_resize, crop_resize_rgba};
use crate::vision::dressing::{self, SuitStyle};
use crate::vision::face::{DecodeTransform, FaceBox, FaceDetection, decode_retinaface};
use crate::vision::geometry::{
    PITCH_FRONTAL_RATIO, PITCH_RATIO_TOLERANCE, Point2, RotationDecision, SIDE_FACE_YAW_DEG,
    decide_rotation, fused_angle_weighted, fused_angle_with_torso_weighted, head_angle,
    pitch_ratio_from_landmarks, shoulder_angle, torso_angle, yaw_from_landmarks,
};
use crate::vision::keypoint::{KeypointSet, decode_movenet};
use crate::vision::matting::{distance_feather, levelset_alpha};
use crate::vision::mtcnn::{CASCADE_FACE_ID, cascade_model_ids, detect_mtcnn_cascade};

/// 内置步骤 id：读图
pub const STEP_READ_IMAGE: &str = "read_image";
/// 内置步骤 id：人体关键点
pub const STEP_KEYPOINT: &str = "keypoint";
/// 内置步骤 id：人像抠图
pub const STEP_MATTING: &str = "matting";
/// 内置步骤 id：人脸检测
pub const STEP_FACE_DETECT: &str = "face_detect";
/// 内置步骤 id：姿态求解
pub const STEP_POSE: &str = "pose";
/// 内置步骤 id：几何纠偏
pub const STEP_ROTATE: &str = "rotate";
/// 内置步骤 id：换装
pub const STEP_DRESS: &str = "dress";
/// 内置步骤 id：美颜
pub const STEP_BEAUTY: &str = "beauty";
/// 内置步骤 id：换底裁切
pub const STEP_BACKGROUND: &str = "background";
/// 内置步骤 id：排版
pub const STEP_LAYOUT: &str = "layout";

/// 阶段中文名：读图（指标与前端展示口径，保持与改造前一致）
const STAGE_READ_IMAGE: &str = "读图";
/// 阶段中文名：人体关键点
const STAGE_KEYPOINT: &str = "人体关键点";
/// 阶段中文名：人像抠图
const STAGE_MATTING: &str = "人像抠图";
/// 阶段中文名：人脸检测
const STAGE_FACE: &str = "人脸检测";
/// 阶段中文名：姿态求解
const STAGE_POSE: &str = "姿态求解";
/// 阶段中文名：几何纠偏
const STAGE_ROTATE: &str = "几何纠偏";
/// 阶段中文名：换装
const STAGE_DRESS: &str = "换装";
/// 阶段中文名：美颜
const STAGE_BEAUTY: &str = "美颜";
/// 阶段中文名：换底裁切
const STAGE_BACKGROUND: &str = "换底裁切";
/// 阶段中文名：排版
const STAGE_LAYOUT: &str = "排版";

/// 人脸检测分数阈值
const FACE_SCORE_THRESHOLD: f32 = 0.5;
/// NMS IoU 阈值
const NMS_IOU_THRESHOLD: f32 = 0.4;
/// 抠图概率 mask 阈值（[0,1] 输出按 ×255 后阈值化）
const MASK_THRESHOLD: u8 = 128;
/// 软阈值（level-set）过渡带宽：保留发丝等亚像素半透明像素
const MASK_SOFT_RANGE: u8 = 48;
/// 距离场羽化过渡带宽度（像素）
const MASK_FEATHER_PX: f32 = 2.0;
/// 头顶留白 = 0.2 × 脸高
const CROP_TOP_RATIO: f64 = 0.2;
/// 下巴余量 = 0.1 × 脸高
const CROP_BOTTOM_RATIO: f64 = 0.1;

/// 步骤实现函数：读写 [`PipelineCtx`] 中的中间态，失败返回中文错误
pub type StepFn = fn(&mut PipelineCtx) -> CoreResult<()>;

/// 内置步骤定义（id / 阶段名 / 展示名 / 依赖 / 实现）
struct StepDef {
    id: &'static str,
    stage: &'static str,
    label: &'static str,
    requires: &'static [&'static str],
    func: StepFn,
}

/// 内置步骤表（顺序即默认执行顺序）
const STEP_DEFS: &[StepDef] = &[
    StepDef {
        id: STEP_READ_IMAGE,
        stage: STAGE_READ_IMAGE,
        label: "读图",
        requires: &[],
        func: step_read_image,
    },
    StepDef {
        id: STEP_KEYPOINT,
        stage: STAGE_KEYPOINT,
        label: "人体关键点检测",
        requires: &[STEP_READ_IMAGE],
        func: step_keypoint,
    },
    StepDef {
        id: STEP_MATTING,
        stage: STAGE_MATTING,
        label: "人像抠图",
        requires: &[STEP_READ_IMAGE],
        func: step_matting,
    },
    StepDef {
        id: STEP_FACE_DETECT,
        stage: STAGE_FACE,
        label: "人脸检测",
        requires: &[STEP_READ_IMAGE],
        func: step_face_detect,
    },
    StepDef {
        id: STEP_POSE,
        stage: STAGE_POSE,
        label: "姿态求解",
        requires: &[STEP_KEYPOINT],
        func: step_pose,
    },
    StepDef {
        id: STEP_ROTATE,
        stage: STAGE_ROTATE,
        label: "几何纠偏",
        requires: &[STEP_POSE],
        func: step_rotate,
    },
    StepDef {
        id: STEP_DRESS,
        stage: STAGE_DRESS,
        label: "换装",
        requires: &[STEP_ROTATE],
        func: step_dress,
    },
    StepDef {
        id: STEP_BEAUTY,
        stage: STAGE_BEAUTY,
        label: "美颜",
        requires: &[STEP_ROTATE],
        func: step_beauty,
    },
    StepDef {
        id: STEP_BACKGROUND,
        stage: STAGE_BACKGROUND,
        label: "换底裁切",
        requires: &[STEP_ROTATE],
        func: step_background,
    },
    StepDef {
        id: STEP_LAYOUT,
        stage: STAGE_LAYOUT,
        label: "排版",
        requires: &[STEP_BACKGROUND],
        func: step_layout,
    },
];

/// 流水线执行上下文：承载步骤之间的中间态（未执行的步骤对应字段为 None）
pub struct PipelineCtx<'a> {
    /// 全局配置
    pub cfg: &'a Config,
    /// 推理引擎
    pub engine: &'a mut dyn InferenceEngine,
    /// 本次处理请求
    pub req: &'a ProcessRequest,
    /// 分阶段耗时指标容器
    pub metrics: &'a mut TaskMetrics,
    /// 当前模式套件（纠偏后的模型按套件解析，步骤声明可覆盖单个模型 id）
    pub suite: ModeSuite,
    /// 解析后的尺寸规格
    pub size: SizeSpec,
    /// 归一化底色列表（id, 规格）
    pub bgs: Vec<(String, BackgroundSpec)>,
    /// 读图并预缩放后的 RGB 原图
    pub img: Option<RgbImage>,
    /// 人体关键点
    pub kps: Option<KeypointSet>,
    /// 原图坐标系二值抠图掩膜
    pub mask: Option<GrayImage>,
    /// 主脸检测结果
    pub face: Option<FaceDetection>,
    /// 纠偏决策
    pub decision: Option<RotationDecision>,
    /// 纠偏后的原图
    pub rot_img: Option<RgbImage>,
    /// 纠偏后的掩膜
    pub rot_mask: Option<GrayImage>,
    /// 换装后的图像
    pub dressed: Option<RgbImage>,
    /// 美颜后的图像
    pub beautified: Option<RgbImage>,
    /// 羽化后的掩膜
    pub alpha: Option<GrayImage>,
    /// 边缘去色边后的前景
    pub cleaned: Option<RgbImage>,
    /// 裁切框
    pub crop: Option<crate::vision::crop::CropRect>,
    /// 每底色证件照
    pub photos: Vec<BgOutput>,
    /// 每底色效果图
    pub effects: Vec<BgOutput>,
    /// 透明底证件照
    pub transparent: Option<RgbaImage>,
    /// 排版相纸
    pub layout_img: Option<RgbImage>,
    /// 处理告警（降级原因等）
    pub warnings: Vec<String>,
}

impl<'a> PipelineCtx<'a> {
    /// 构造上下文（基础状态由编排器填充，其余中间态由各步骤写入）
    pub fn new(
        cfg: &'a Config,
        engine: &'a mut dyn InferenceEngine,
        req: &'a ProcessRequest,
        metrics: &'a mut TaskMetrics,
        suite: ModeSuite,
        size: SizeSpec,
        bgs: Vec<(String, BackgroundSpec)>,
    ) -> Self {
        Self {
            cfg,
            engine,
            req,
            metrics,
            suite,
            size,
            bgs,
            img: None,
            kps: None,
            mask: None,
            face: None,
            decision: None,
            rot_img: None,
            rot_mask: None,
            dressed: None,
            beautified: None,
            alpha: None,
            cleaned: None,
            crop: None,
            photos: Vec::new(),
            effects: Vec::new(),
            transparent: None,
            layout_img: None,
            warnings: Vec::new(),
        }
    }

    /// 读图尺寸（读图步骤未执行时返回 None）
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.img.as_ref().map(|i| i.dimensions())
    }

    /// 当前主图：美颜 > 换装 > 纠偏 > 原图（未执行读图步骤时返回 None）
    pub fn base_image(&self) -> Option<&RgbImage> {
        self.beautified
            .as_ref()
            .or(self.dressed.as_ref())
            .or(self.rot_img.as_ref())
            .or(self.img.as_ref())
    }

    /// 追加中文告警
    pub fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }

    /// 该步骤使用的模型 id（`[pipeline.params.<step>] model = "..."` 优先于模式套件）
    pub fn step_model(&self, step: &str, fallback: &str) -> String {
        step_model_id(self.cfg, step, fallback)
    }
}

/// 步骤元数据（配置校验、`GET /config` 与前端展示共用）
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepMeta {
    /// 步骤 id（内置算子 id 或自定义步骤名）
    pub id: String,
    /// 展示名
    pub label: String,
    /// 阶段中文名（指标口径）
    pub stage: String,
    /// 依赖的步骤 id
    pub requires: Vec<String>,
}

/// 解析后的步骤：实现函数 + 阶段中文名
#[derive(Debug, Clone, Copy)]
pub struct ResolvedStep {
    /// 步骤实现
    pub func: StepFn,
    /// 阶段中文名
    pub stage: &'static str,
}

/// 内置步骤 id 列表（默认执行顺序）
pub fn builtin_steps() -> Vec<&'static str> {
    STEP_DEFS.iter().map(|d| d.id).collect()
}

/// 是否为内置步骤 id
pub fn is_builtin_step(id: &str) -> bool {
    step_def(id).is_some()
}

/// 内置步骤元数据（含依赖）
pub fn step_defs() -> Vec<StepMeta> {
    STEP_DEFS
        .iter()
        .map(|d| StepMeta {
            id: d.id.to_string(),
            label: d.label.to_string(),
            stage: d.stage.to_string(),
            requires: d.requires.iter().map(|r| r.to_string()).collect(),
        })
        .collect()
}

/// 可用步骤元数据：内置步骤 + 配置中声明的自定义步骤
pub fn step_metas(cfg: &Config) -> Vec<StepMeta> {
    let mut metas = step_defs();
    for (name, custom) in &cfg.pipeline.custom {
        let (label, stage, requires) = match step_def(&custom.op) {
            Some(d) => (
                format!("自定义步骤（{}）", d.label),
                d.stage.to_string(),
                d.requires.iter().map(|r| r.to_string()).collect(),
            ),
            None => (
                format!("自定义步骤（{}）", custom.op),
                "自定义步骤".to_string(),
                Vec::new(),
            ),
        };
        metas.push(StepMeta {
            id: name.clone(),
            label,
            stage,
            requires,
        });
    }
    metas
}

/// 可用步骤 id 列表（中文错误提示用）
pub fn available_steps(cfg: &Config) -> Vec<String> {
    step_metas(cfg).into_iter().map(|m| m.id).collect()
}

/// 步骤名对应的内置算子 id（自定义步骤按 `[pipeline.custom.<名>] op` 解析）
pub fn step_op<'a>(cfg: &'a Config, name: &'a str) -> Option<&'a str> {
    if is_builtin_step(name) {
        return Some(name);
    }
    cfg.pipeline.custom.get(name).map(|c| c.op.as_str())
}

/// 内置步骤的依赖（以内置算子 id 表达）
pub fn step_requires(op: &str) -> &'static [&'static str] {
    step_def(op).map(|d| d.requires).unwrap_or(&[])
}

/// 内置步骤的阶段中文名
pub fn step_stage(op: &str) -> Option<&'static str> {
    step_def(op).map(|d| d.stage)
}

/// 注册自定义步骤（进程内注册表，重启失效）；名与内置/已注册步骤冲突时返回 false
pub fn register_step(name: &str, stage: &'static str, func: StepFn) -> bool {
    if is_builtin_step(name) {
        return false;
    }
    let mut map = registry().lock().unwrap();
    if map.contains_key(name) {
        return false;
    }
    map.insert(name.to_string(), ResolvedStep { func, stage });
    true
}

/// 解析步骤名：内置表 → 配置自定义步骤（op 映射）→ 运行期注册表
pub fn resolve_step(cfg: &Config, name: &str) -> Option<ResolvedStep> {
    if let Some(d) = step_def(name) {
        return Some(ResolvedStep {
            func: d.func,
            stage: d.stage,
        });
    }
    if let Some(custom) = cfg.pipeline.custom.get(name) {
        if let Some(d) = step_def(&custom.op) {
            return Some(ResolvedStep {
                func: d.func,
                stage: d.stage,
            });
        }
    }
    registry().lock().ok()?.get(name).copied()
}

/// 生效步骤列表：请求指定（非空）> 配置 `[pipeline] steps` > 内置默认
pub fn effective_steps(cfg: &Config, requested: Option<&[String]>) -> Vec<String> {
    if let Some(list) = requested.filter(|l| !l.is_empty()) {
        return list.to_vec();
    }
    if !cfg.pipeline.steps.is_empty() {
        return cfg.pipeline.steps.clone();
    }
    builtin_steps().iter().map(|s| s.to_string()).collect()
}

/// 按启用步骤推导需装载的模型 id（步骤显式声明的模型 id 优先于模式套件）。
/// 换装的人像解析模型独立于模式套件，由换装步骤在启用时按需惰性装载，此处不预载。
pub fn required_model_ids(cfg: &Config, suite: &ModeSuite, steps: &[String]) -> Vec<String> {
    let ops: Vec<&str> = steps.iter().filter_map(|n| step_op(cfg, n)).collect();
    let mut ids: Vec<String> = Vec::new();
    let push = |ids: &mut Vec<String>, id: String| {
        if !ids.contains(&id) {
            ids.push(id);
        }
    };
    if ops.contains(&STEP_KEYPOINT) {
        push(&mut ids, step_model_id(cfg, STEP_KEYPOINT, &suite.keypoint));
    }
    if ops.contains(&STEP_MATTING) {
        push(&mut ids, step_model_id(cfg, STEP_MATTING, &suite.matting));
    }
    if ops.contains(&STEP_FACE_DETECT) {
        let face_id = step_model_id(cfg, STEP_FACE_DETECT, &suite.face);
        if face_id == CASCADE_FACE_ID {
            for id in cascade_model_ids() {
                push(&mut ids, id.to_string());
            }
        } else {
            push(&mut ids, face_id);
        }
    }
    ids
}

/// 执行步骤序列（顺序即执行顺序；未知步骤返回中文错误）
pub fn run_steps(ctx: &mut PipelineCtx, names: &[String]) -> CoreResult<()> {
    for name in names {
        let step = resolve_step(ctx.cfg, name).ok_or_else(|| {
            CoreError::ConfigValidate(format!(
                "未知工作流步骤“{name}”，可选：{}",
                available_steps(ctx.cfg).join("、")
            ))
        })?;
        (step.func)(ctx)?;
    }
    Ok(())
}

/// 请求了可选功能但对应步骤未在步骤表中启用时给出中文告警（不阻断出图）
pub fn warn_disabled_features(ctx: &mut PipelineCtx, names: &[String]) {
    let ops: Vec<&str> = names.iter().filter_map(|n| step_op(ctx.cfg, n)).collect();
    let ignored: Vec<(&str, bool)> = vec![
        (
            "换装",
            ctx.req.dress.as_ref().is_some_and(|d| d.enabled) && !ops.contains(&STEP_DRESS),
        ),
        (
            "美颜",
            ctx.req.beauty.as_ref().is_some_and(|b| b.enabled) && !ops.contains(&STEP_BEAUTY),
        ),
        (
            "排版",
            ctx.req.layout.is_some() && !ops.contains(&STEP_LAYOUT),
        ),
        (
            "换底（含透明底与自定义背景）",
            (ctx.req.transparent || ctx.req.bg_image.is_some()) && !ops.contains(&STEP_BACKGROUND),
        ),
    ];
    for (name, hit) in ignored {
        if hit {
            ctx.warn(format!("工作流未启用「{name}」步骤，相关请求已忽略"));
        }
    }
}

// ---------- 内置步骤实现 ----------

/// 读图：读取输入图并统一为 RGB，超过最大边长时等比预缩放（限制峰值内存与推理耗时）
fn step_read_image(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let timer = StageTimer::start(STAGE_READ_IMAGE);
    let img = image::open(&ctx.req.input)
        .map_err(|e| CoreError::Image(format!("读取图片 {} 失败：{e}", ctx.req.input.display())))?
        .to_rgb8();
    let img = limit_input_side(img, ctx.cfg.general.max_input_side);
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(CoreError::Image("图片尺寸为零".into()));
    }
    ctx.img = Some(img);
    timer.stop(ctx.metrics);
    Ok(())
}

/// 人体关键点（MoveNet）：预处理 + 推理 + 解码
fn step_keypoint(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let timer = StageTimer::start(STAGE_KEYPOINT);
    let (id, input) = {
        let img = ctx
            .img
            .as_ref()
            .ok_or_else(|| missing_step(STEP_READ_IMAGE))?;
        let fallback = ctx.suite.keypoint.clone();
        let id = ctx.step_model(STEP_KEYPOINT, &fallback);
        let spec = ctx.cfg.model_spec(&id)?;
        (
            id,
            build_input_with(img, &spec.input_dims, &spec.preprocess)?,
        )
    };
    let outs = ctx.engine.run(&id, &input.tensor)?;
    let (w, h) = ctx
        .dimensions()
        .ok_or_else(|| missing_step(STEP_READ_IMAGE))?;
    ctx.kps = Some(decode_movenet(&outs[0], w, h)?);
    timer.stop(ctx.metrics);
    Ok(())
}

/// 人像抠图：预处理 + 推理 + 概率掩膜
fn step_matting(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let timer = StageTimer::start(STAGE_MATTING);
    let (id, input, w, h) = {
        let img = ctx
            .img
            .as_ref()
            .ok_or_else(|| missing_step(STEP_READ_IMAGE))?;
        let (w, h) = img.dimensions();
        let fallback = ctx.suite.matting.clone();
        let id = ctx.step_model(STEP_MATTING, &fallback);
        let spec = ctx.cfg.model_spec(&id)?;
        (
            id,
            build_input_with(img, &spec.input_dims, &spec.preprocess)?,
            w,
            h,
        )
    };
    let outs = ctx.engine.run(&id, &input.tensor)?;
    ctx.mask = Some(probability_mask(&outs[0], w, h, input.letterbox.as_ref())?);
    timer.stop(ctx.metrics);
    Ok(())
}

/// 人脸检测（RetinaFace 单模型 / MTCNN 三级联），取首个结果为主脸
fn step_face_detect(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let timer = StageTimer::start(STAGE_FACE);
    let fallback = ctx.suite.face.clone();
    let face_id = ctx.step_model(STEP_FACE_DETECT, &fallback);
    let faces = if face_id == CASCADE_FACE_ID {
        let img = ctx
            .img
            .as_ref()
            .ok_or_else(|| missing_step(STEP_READ_IMAGE))?;
        detect_mtcnn_cascade(ctx.engine, img)?
    } else {
        let (input, transform) = {
            let img = ctx
                .img
                .as_ref()
                .ok_or_else(|| missing_step(STEP_READ_IMAGE))?;
            let spec = ctx.cfg.model_spec(&face_id)?;
            let input = build_input_with(img, &spec.input_dims, &spec.preprocess)?;
            let (scale_x, scale_y, pad_x, pad_y) = match input.letterbox {
                Some(lb) => (lb.scale, lb.scale, lb.pad_x, lb.pad_y),
                None => (1.0, 1.0, 0.0, 0.0),
            };
            let transform = DecodeTransform {
                image_size: (spec.input_dims[2] as u32, spec.input_dims[3] as u32),
                scale_x,
                scale_y,
                pad_x,
                pad_y,
            };
            (input, transform)
        };
        let outs = ctx.engine.run(&face_id, &input.tensor)?;
        // 真实 RetinaFace 输出顺序为 [bbox, confidence, landmark]（Hivision 官方模型），
        // decode_retinaface 期望 [scores, boxes, landmarks]，此处按位置重排
        decode_retinaface(
            &outs[1],
            &outs[0],
            &outs[2],
            FACE_SCORE_THRESHOLD,
            NMS_IOU_THRESHOLD,
            transform,
        )?
    };
    // 级联 id 校验（测试/配置完整性）
    if face_id == CASCADE_FACE_ID {
        for id in cascade_model_ids() {
            ctx.cfg.model_spec(id)?;
        }
    }
    let face = faces
        .first()
        .cloned()
        .ok_or_else(|| CoreError::Image("未检测到人脸".into()))?;
    ctx.face = Some(face);
    timer.stop(ctx.metrics);
    Ok(())
}

/// 姿态角度求解（0.6 头部 + 0.4 肩线；髋/膝可用时改用 0.5/0.3/0.2 三路，缺失降级并告警）
fn step_pose(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let timer = StageTimer::start(STAGE_POSE);
    let measured = {
        let kps = ctx
            .kps
            .as_ref()
            .ok_or_else(|| missing_step(STEP_KEYPOINT))?;
        fused_measured(kps, &mut ctx.warnings)
    };
    let decision = decide_rotation(measured, ctx.req.rotate)?;
    if let Some(warn) = decision.warning() {
        ctx.warnings.push(warn);
    }
    if let Some(face) = &ctx.face {
        if let Some(warn) = side_face_warning(face) {
            ctx.warnings.push(warn);
        }
        if let Some(warn) = pitch_warning(face) {
            ctx.warnings.push(warn);
        }
    }
    ctx.decision = Some(decision);
    timer.stop(ctx.metrics);
    Ok(())
}

/// 同步几何纠偏（同一仿射矩阵变换原图与 mask；无 mask 时仅变换原图）
fn step_rotate(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let timer = StageTimer::start(STAGE_ROTATE);
    let decision = ctx.decision.ok_or_else(|| missing_step(STEP_POSE))?;
    let (rot_img, rot_mask) = {
        let img = ctx
            .img
            .as_ref()
            .ok_or_else(|| missing_step(STEP_READ_IMAGE))?;
        match ctx.mask.as_ref() {
            Some(mask) => {
                let (i, m) = rotate_image_same(img, mask, decision.correction())?;
                (i, Some(m))
            }
            None => (rotate_image(img, decision.correction()), None),
        }
    };
    ctx.rot_img = Some(rot_img);
    ctx.rot_mask = rot_mask;
    timer.stop(ctx.metrics);
    Ok(())
}

/// 换装（可选）：人像解析 → 衣服 mask → 服装贴合（作用于纠偏后原图，美颜之前）
fn step_dress(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let params = match &ctx.req.dress {
        Some(d) if d.enabled => d.clone(),
        _ => return Ok(()),
    };
    let timer = StageTimer::start(STAGE_DRESS);
    let fallback = dressing::PARSING_MODEL_ID.to_string();
    let parsing_id = ctx.step_model(STEP_DRESS, &fallback);
    let provider = ctx.suite.execution_provider;
    ctx.engine.load(ctx.cfg, &parsing_id, provider)?;
    let parsing = {
        let rot_img = ctx
            .rot_img
            .as_ref()
            .ok_or_else(|| missing_step(STEP_ROTATE))?;
        let spec = ctx.cfg.model_spec(&parsing_id)?;
        let input = build_input_with(rot_img, &spec.input_dims, &spec.preprocess)?;
        let outs = ctx.engine.run(&parsing_id, &input.tensor)?;
        dressing::decode_parsing(
            &outs[0],
            rot_img.width(),
            rot_img.height(),
            input.letterbox.as_ref(),
        )?
    };
    let rot_img = ctx
        .rot_img
        .as_ref()
        .ok_or_else(|| missing_step(STEP_ROTATE))?;
    let out = if let Some(gs) = &params.garments {
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
                            CoreError::Image(format!("读取服装图 {} 失败：{e}", p.display()))
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
        dressing::fit_garment_parts(rot_img, &parsing, &parts)?
    } else {
        // 程序化全身套装样式（suit_full_*）用全身服装类集（含裤装/腿/鞋），
        // 用户服装图与上半身样式沿用单件语义，避免误覆盖下半身
        let style = params
            .garment
            .as_ref()
            .map(|_| None)
            .unwrap_or_else(|| {
                Some(SuitStyle::parse(
                    params.style.as_deref().unwrap_or("suit_navy"),
                ))
            })
            .transpose()?;
        let clothes = if style.is_some_and(SuitStyle::is_full) {
            dressing::full_clothes_mask(&parsing)
        } else {
            dressing::clothes_mask(&parsing)
        };
        let garment = match &params.garment {
            Some(path) => image::open(path)
                .map_err(|e| CoreError::Image(format!("读取服装图 {} 失败：{e}", path.display())))?
                .to_rgb8(),
            None => dressing::formal_suit(style.unwrap_or(SuitStyle::Navy), 240, 360),
        };
        dressing::fit_garment(rot_img, &garment, &clothes)?
    };
    ctx.dressed = Some(out);
    timer.stop(ctx.metrics);
    Ok(())
}

/// 美颜（可选）：分区磨皮——五官（双眼/眉、鼻、嘴）保护区不磨皮，保留五官锐度；
/// 美颜不改变 mask 与裁剪框
fn step_beauty(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let params = match &ctx.req.beauty {
        Some(p) if p.enabled => p.clone(),
        _ => return Ok(()),
    };
    let timer = StageTimer::start(STAGE_BEAUTY);
    let decision = ctx.decision.ok_or_else(|| missing_step(STEP_POSE))?;
    let protect = match (&ctx.face, ctx.dimensions()) {
        (Some(face), Some((w, h))) => Some(beauty_protect_mask(face, w, h, decision.correction())),
        // 未启用人脸检测时无五官保护区，整体美颜
        _ => None,
    };
    let out = {
        let base = ctx.base_image().ok_or_else(|| missing_step(STEP_ROTATE))?;
        apply_beauty_protected(
            base,
            &crate::config::BeautyConfig {
                enabled: true,
                skin_smooth: params.skin_smooth.unwrap_or(ctx.cfg.beauty.skin_smooth),
                brighten: params.brighten.unwrap_or(ctx.cfg.beauty.brighten),
                whiten: params.whiten.unwrap_or(ctx.cfg.beauty.whiten),
            },
            protect.as_ref(),
        )
    };
    ctx.beautified = Some(out);
    timer.stop(ctx.metrics);
    Ok(())
}

/// 换底色与裁切：掩膜羽化 + 边缘去色边后逐像素 alpha 混合，按人脸框裁切缩放
/// （缺失人脸检测时按图像居中裁切；缺失抠图时跳过合成、直接裁切当前主图）
fn step_background(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let timer = StageTimer::start(STAGE_BACKGROUND);
    let (w, h) = ctx
        .dimensions()
        .ok_or_else(|| missing_step(STEP_READ_IMAGE))?;
    let size = ctx.size.clone();
    // 裁剪框与底色无关，先算一次；无人脸框时退化为居中裁切
    let crop = match ctx.face.as_ref().map(|f| f.face) {
        Some(face_box) => compute_crop(
            &face_box,
            w,
            h,
            size.width_px,
            size.height_px,
            CROP_TOP_RATIO,
            CROP_BOTTOM_RATIO,
        )?,
        None => {
            ctx.warn("未启用人脸检测步骤（或未检出人脸），裁切按图像居中处理");
            compute_crop(
                &FaceBox {
                    x1: 0.0,
                    y1: 0.0,
                    x2: w as f32,
                    y2: h as f32,
                    score: 0.0,
                },
                w,
                h,
                size.width_px,
                size.height_px,
                CROP_TOP_RATIO,
                CROP_BOTTOM_RATIO,
            )?
        }
    };
    let alpha = ctx
        .rot_mask
        .as_ref()
        .map(|m| distance_feather(m, MASK_THRESHOLD, MASK_FEATHER_PX));
    if alpha.is_none() {
        ctx.warn("未启用人像抠图步骤，跳过换底合成并直接裁切当前图像");
    }
    let cleaned = {
        let base = ctx.base_image().ok_or_else(|| missing_step(STEP_ROTATE))?;
        match &alpha {
            Some(a) => decontaminate(base, a, MASK_FEATHER_PX as u32),
            None => base.clone(),
        }
    };
    let mut photos = Vec::with_capacity(ctx.bgs.len() + 1);
    let mut effects = Vec::new();
    for (bg_id, bg) in &ctx.bgs {
        // 效果图：换底后保持旋转全图尺寸
        let composed = match &alpha {
            Some(a) => composite(&cleaned, a, bg.rgb),
            None => cleaned.clone(),
        };
        if ctx.req.effect {
            effects.push(BgOutput {
                bg: bg_id.clone(),
                image: composed.clone(),
            });
        }
        // 证件照：按人脸框裁切缩放
        photos.push(BgOutput {
            bg: bg_id.clone(),
            image: crop_resize(&composed, &crop, size.width_px, size.height_px)?,
        });
    }

    // 自定义背景图（可选）：按原图尺寸 cover 缩放裁切后与人像合成，产物底色标识 custombg
    if let Some(path) = &ctx.req.bg_image {
        let bg_img = image::open(path)
            .map_err(|e| CoreError::Image(format!("读取背景图 {} 失败：{e}", path.display())))?
            .to_rgb8();
        let canvas = fit_cover(&bg_img, w, h);
        let composed = match &alpha {
            Some(a) => composite_with_image(&cleaned, a, &canvas),
            None => canvas,
        };
        photos.push(BgOutput {
            bg: CUSTOM_BG_ID.to_string(),
            image: crop_resize(&composed, &crop, size.width_px, size.height_px)?,
        });
    }

    // 透明底证件照（可选）：RGB 取去色边后前景，alpha 取羽化掩膜（PNG 输出）
    let transparent = match (&alpha, ctx.req.transparent) {
        (Some(a), true) => {
            let rgba = to_rgba(&cleaned, a);
            Some(crop_resize_rgba(
                &rgba,
                &crop,
                size.width_px,
                size.height_px,
            )?)
        }
        (None, true) => {
            ctx.warn("未启用人像抠图步骤，无法输出透明底证件照");
            None
        }
        _ => None,
    };
    ctx.crop = Some(crop);
    ctx.alpha = alpha;
    ctx.cleaned = Some(cleaned);
    ctx.photos = photos;
    ctx.effects = effects;
    ctx.transparent = transparent;
    timer.stop(ctx.metrics);
    Ok(())
}

/// 排版相纸（可选）：以首个底色证件照按相纸规格铺版；未指定相纸时不执行、不记录阶段
fn step_layout(ctx: &mut PipelineCtx) -> CoreResult<()> {
    let Some(layout_id) = ctx.req.layout.clone() else {
        return Ok(());
    };
    let timer = StageTimer::start(STAGE_LAYOUT);
    let spec = ctx.cfg.layout.get(&layout_id).ok_or_else(|| {
        CoreError::ConfigValidate(format!(
            "未知排版“{layout_id}”，可选：{}",
            ctx.cfg
                .layout
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join("、")
        ))
    })?;
    let photo = ctx
        .photos
        .first()
        .ok_or_else(|| CoreError::Image("证件照产物为空，无法排版".into()))?;
    let composed = crate::vision::layout::compose(&photo.image, spec, ctx.size.dpi)?;
    ctx.layout_img = Some(composed);
    timer.stop(ctx.metrics);
    Ok(())
}

// ---------- 内部工具 ----------

/// 内置步骤定义查询
fn step_def(id: &str) -> Option<&'static StepDef> {
    STEP_DEFS.iter().find(|d| d.id == id)
}

/// 运行期注册表（自定义步骤名 → 实现）
fn registry() -> &'static Mutex<BTreeMap<String, ResolvedStep>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, ResolvedStep>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 步骤实际使用的模型 id（`[pipeline.params.<step>] model = "..."` 优先）
fn step_model_id(cfg: &Config, step: &str, fallback: &str) -> String {
    cfg.pipeline
        .params
        .get(step)
        .and_then(|v| v.get("model"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

/// 「前置步骤缺失」的中文错误（配置期校验已拦截，此处为兜底）
fn missing_step(op: &str) -> CoreError {
    let label = step_def(op).map(|d| d.label).unwrap_or(op);
    CoreError::ConfigValidate(format!(
        "工作流缺少前置步骤「{label}」，请在步骤表中启用该步骤"
    ))
}

/// 输入图等比预缩放（`max_side = 0` 或未超限时原样返回）
fn limit_input_side(img: RgbImage, max_side: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    let (nw, nh) = crate::pipeline::limited_dimensions(w, h, max_side);
    if (nw, nh) == (w, h) {
        return img;
    }
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

/// 概率 mask 张量 `[1,1,H,W]`（行主序）→ 原图尺寸软阈值 alpha（letterbox 逆变换 +
/// 软阈值 + 距离场羽化）
fn probability_mask(
    out: &TensorData,
    w: u32,
    h: u32,
    letterbox: Option<&LetterBox>,
) -> CoreResult<GrayImage> {
    let prob = probability_map(out, w, h, letterbox)?;
    let soft = levelset_alpha(&prob, MASK_THRESHOLD, MASK_SOFT_RANGE);
    Ok(distance_feather(&soft, MASK_THRESHOLD, MASK_FEATHER_PX))
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

/// 融合测量角：优先 0.6×双眼角 + 0.4×双肩角；髋/膝可用时改用三路融合；缺失时降级并记录告警。
/// 各路置信度参与加权：低置信路自动降权、其余路归一补足，防噪声点主导（两路/三路口径一致）。
fn fused_measured(kps: &KeypointSet, warnings: &mut Vec<String>) -> f64 {
    let head = kps.eyes().map(|(l, r)| head_angle(&l, &r));
    let head_conf = kps.eyes_conf();
    let shoulder = kps.shoulders().map(|(l, r)| shoulder_angle(&l, &r));
    let shoulder_conf = kps.shoulders_conf();
    // 躯干垂直度：肩中点 → 髋（缺失退回膝）中点，二者缺一时无法求解
    let torso_data = (shoulder_mid_with_conf(kps), kps.lower_ref());
    let torso = match (&torso_data.0, &torso_data.1) {
        (Some((s, _)), Some((lower, _))) => Some(torso_angle(s, lower)),
        _ => None,
    };
    match (head, shoulder, torso) {
        // 髋/膝可用 → 三路置信度加权融合，额外修复高低肩之外的侧身倾斜
        (Some(h), Some(s), Some(t)) => {
            // 躯干轴由肩中点与下半身中点构成：取两路置信度较低者（瓶颈为准）
            let torso_conf = torso_data
                .0
                .map(|(_, c)| c)
                .unwrap_or(0.0)
                .min(torso_data.1.map(|(_, c)| c).unwrap_or(0.0));
            fused_angle_with_torso_weighted(h, head_conf, s, shoulder_conf, t, torso_conf)
        }
        (Some(h), Some(s), None) => fused_angle_weighted(h, head_conf, s, shoulder_conf),
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

/// 双肩中点及置信度（双肩缺失返回 None）
fn shoulder_mid_with_conf(kps: &KeypointSet) -> Option<(Point2, f64)> {
    kps.shoulders().map(|(l, r)| {
        (
            Point2::new((l.x + r.x) / 2.0, (l.y + r.y) / 2.0),
            kps.shoulders_conf(),
        )
    })
}

/// 侧脸告警：由人脸 5 点关键点（左眼、右眼、鼻尖）估算 yaw，超过阈值时返回中文提示。
/// 关键点退化（双眼重合，如演示/桩数据）时无法判断，返回 None 不告警。
fn side_face_warning(face: &FaceDetection) -> Option<String> {
    let yaw = yaw_from_landmarks(&face.landmarks[0], &face.landmarks[1], &face.landmarks[2])?;
    (yaw.abs() > SIDE_FACE_YAW_DEG)
        .then(|| format!("疑似侧脸（估算偏转 {yaw:.0}°），建议提供正面照"))
}

/// 俯仰告警：由人脸 5 点关键点估算鼻尖相对眼线的垂直占比，明显偏离正面平视基准时返回
/// 中文提示（低头 / 仰头）。俯仰无法通过旋转纠偏，故仅告警提示重拍，不阻断出图。
/// 关键点退化（演示/桩数据）时无法判断，返回 None 不告警。
fn pitch_warning(face: &FaceDetection) -> Option<String> {
    let ratio = pitch_ratio_from_landmarks(
        &face.landmarks[0],
        &face.landmarks[1],
        &face.landmarks[2],
        &face.landmarks[3],
        &face.landmarks[4],
    )?;
    let deviation = ratio - PITCH_FRONTAL_RATIO;
    if deviation.abs() <= PITCH_RATIO_TOLERANCE {
        return None;
    }
    let direction = if deviation > 0.0 { "低头" } else { "仰头" };
    Some(format!("疑似{direction}，建议提供平视正面照"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PipelineConfig;
    use crate::vision::crop::CropRect;
    use image::Rgb;

    /// 测试用步骤：把读图结果整体提亮固定值（验证自定义步骤注册与执行）
    fn 提亮步骤(ctx: &mut PipelineCtx) -> CoreResult<()> {
        if let Some(img) = ctx.img.as_mut() {
            for p in img.pixels_mut() {
                p[0] = p[0].saturating_add(10);
            }
        }
        Ok(())
    }

    #[test]
    fn 默认步骤表为内置十步且顺序稳定() {
        assert_eq!(
            builtin_steps(),
            vec![
                "read_image",
                "keypoint",
                "matting",
                "face_detect",
                "pose",
                "rotate",
                "dress",
                "beauty",
                "background",
                "layout",
            ]
        );
        // 缺省配置 → 内置默认；请求指定 → 请求优先
        let cfg = Config::default();
        assert_eq!(effective_steps(&cfg, None).len(), 10);
        let req = vec!["read_image".to_string(), "background".to_string()];
        assert_eq!(effective_steps(&cfg, Some(&req)), req);
        // 空请求列表视为未指定
        assert_eq!(effective_steps(&cfg, Some(&[])).len(), 10);
    }

    #[test]
    fn 步骤元数据含阶段名与依赖() {
        let metas = step_defs();
        let keypoint = metas.iter().find(|m| m.id == STEP_KEYPOINT).unwrap();
        assert_eq!(keypoint.stage, "人体关键点");
        assert_eq!(keypoint.requires, vec!["read_image".to_string()]);
        let layout = metas.iter().find(|m| m.id == STEP_LAYOUT).unwrap();
        assert_eq!(layout.requires, vec!["background".to_string()]);
        assert_eq!(step_stage(STEP_BACKGROUND), Some("换底裁切"));
        assert!(is_builtin_step(STEP_MATTING));
        assert!(!is_builtin_step("不存在的步骤"));
    }

    #[test]
    fn 自定义步骤按配置算子解析且注册表可覆盖阶段名() {
        let mut cfg = Config::default();
        cfg.pipeline.custom.insert(
            "我的抠图".into(),
            crate::config::CustomStep {
                op: STEP_MATTING.into(),
            },
        );
        let resolved = resolve_step(&cfg, "我的抠图").expect("自定义步骤应可解析");
        assert_eq!(resolved.stage, "人像抠图");
        assert!(step_metas(&cfg).iter().any(|m| m.id == "我的抠图"));
        assert_eq!(step_op(&cfg, "我的抠图"), Some(STEP_MATTING));
        // 未注册的名字无法解析
        assert!(resolve_step(&cfg, "未注册步骤").is_none());
        // 运行期注册：自定义名 → 自定义实现与阶段名
        assert!(register_step("提亮步骤", "自定义提亮", 提亮步骤));
        assert!(
            !register_step("提亮步骤", "自定义提亮", 提亮步骤),
            "重复注册应失败"
        );
        assert!(
            !register_step(STEP_MATTING, "人像抠图", 提亮步骤),
            "不得覆盖内置步骤"
        );
        let resolved = resolve_step(&cfg, "提亮步骤").unwrap();
        assert_eq!(resolved.stage, "自定义提亮");
    }

    #[test]
    fn 按步骤推导需装载的模型() {
        let cfg = Config::default();
        let suite = cfg.mode("balanced").unwrap().clone();
        let steps: Vec<String> = builtin_steps().iter().map(|s| s.to_string()).collect();
        let ids = required_model_ids(&cfg, &suite, &steps);
        assert_eq!(
            ids,
            vec![
                suite.keypoint.clone(),
                suite.matting.clone(),
                suite.face.clone(),
            ]
        );
        // 只留读图 → 不装载任何模型；speed 套件的人脸为级联 → 展开子模型
        let only_read: Vec<String> = vec![STEP_READ_IMAGE.to_string()];
        assert!(required_model_ids(&cfg, &suite, &only_read).is_empty());
        let speed = cfg.mode("speed").unwrap().clone();
        let ids = required_model_ids(&cfg, &speed, &steps);
        for sub in cascade_model_ids() {
            assert!(ids.contains(&sub.to_string()), "缺少级联子模型 {sub}");
        }
        // 步骤声明覆盖模型 id（[pipeline.params.keypoint] model = "..."）
        let mut cfg2 = Config::default();
        let params: toml::Value = toml::from_str("[keypoint]\nmodel = \"自定义关键点\"\n").unwrap();
        cfg2.pipeline.params = params
            .as_table()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let ids = required_model_ids(&cfg2, &suite, &steps);
        assert_eq!(ids[0], "自定义关键点");
    }

    #[test]
    fn 未知步骤给出中文错误() {
        let cfg = Config::default();
        let mut metrics = TaskMetrics::new();
        let req = ProcessRequest {
            input: std::path::PathBuf::from("不存在.jpg"),
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
            steps: None,
        };
        let mut engine = crate::inference::FakeEngine::new();
        let suite = cfg.mode("balanced").unwrap().clone();
        let (_, size) = cfg.resolve_size("one_inch").unwrap();
        let mut ctx = PipelineCtx::new(
            &cfg,
            &mut engine,
            &req,
            &mut metrics,
            suite,
            size,
            vec![("white".into(), cfg.background("white").unwrap().clone())],
        );
        let err = run_steps(&mut ctx, &["不存在".to_string()]).unwrap_err();
        assert!(err.to_string().contains("未知工作流步骤"), "实际：{err}");
    }

    #[test]
    fn 缺前置步骤执行报中文错误() {
        let cfg = Config::default();
        let mut metrics = TaskMetrics::new();
        let req = ProcessRequest {
            input: std::path::PathBuf::from("不存在.jpg"),
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
            steps: None,
        };
        let mut engine = crate::inference::FakeEngine::new();
        let suite = cfg.mode("balanced").unwrap().clone();
        let (_, size) = cfg.resolve_size("one_inch").unwrap();
        let mut ctx = PipelineCtx::new(
            &cfg,
            &mut engine,
            &req,
            &mut metrics,
            suite,
            size,
            vec![("white".into(), cfg.background("white").unwrap().clone())],
        );
        // 直接执行「换底裁切」而无读图/纠偏前置 → 中文错误
        let err = run_steps(&mut ctx, &[STEP_BACKGROUND.to_string()]).unwrap_err();
        assert!(err.to_string().contains("缺少前置步骤"), "实际：{err}");
        // 未指定相纸 → 排版步骤直接跳过（不记录阶段、不出错）
        assert!(run_steps(&mut ctx, &[STEP_LAYOUT.to_string()]).is_ok());
        assert!(metrics.is_empty());
    }

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
        let mut kps = KeypointSet {
            points: [None; 17],
            scores: [0.0; 17],
        };
        // 双眼、双肩水平（角度 0）
        kps.points[LEFT_EYE] = Some(Point2::new(40.0, 40.0));
        kps.points[RIGHT_EYE] = Some(Point2::new(60.0, 40.0));
        kps.scores[LEFT_EYE] = 0.99;
        kps.scores[RIGHT_EYE] = 0.99;
        kps.points[LEFT_SHOULDER] = Some(Point2::new(20.0, 100.0));
        kps.points[RIGHT_SHOULDER] = Some(Point2::new(80.0, 100.0));
        kps.scores[LEFT_SHOULDER] = 0.99;
        kps.scores[RIGHT_SHOULDER] = 0.99;
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
        kps.scores[LEFT_HIP] = 0.9;
        kps.scores[RIGHT_HIP] = 0.9;
        let measured = fused_measured(&kps, &mut w);
        assert!((measured - 2.0).abs() < 0.02, "实际 {measured}");
        assert!(w.is_empty(), "髋部可用时不应告警");
    }

    #[test]
    fn 低置信肩线在姿态融合中自动降权() {
        use crate::vision::keypoint::{LEFT_EYE, LEFT_SHOULDER, RIGHT_EYE, RIGHT_SHOULDER};
        let mut kps = KeypointSet {
            points: [None; 17],
            scores: [0.0; 17],
        };
        // 双眼水平（角度 0，高置信）；双肩连线相对水平 -20°（右端更低）但有肩低置信（0.2）
        let ry = 100.0 + 60.0 * (-20.0f64).to_radians().tan(); // ≈ 78.16
        kps.points[LEFT_EYE] = Some(Point2::new(40.0, 40.0));
        kps.points[RIGHT_EYE] = Some(Point2::new(60.0, 40.0));
        kps.scores[LEFT_EYE] = 0.98;
        kps.scores[RIGHT_EYE] = 0.98;
        kps.points[LEFT_SHOULDER] = Some(Point2::new(20.0, 100.0));
        kps.points[RIGHT_SHOULDER] = Some(Point2::new(80.0, ry));
        kps.scores[LEFT_SHOULDER] = 0.2;
        kps.scores[RIGHT_SHOULDER] = 0.99;
        let mut w = Vec::new();
        let measured = fused_measured(&kps, &mut w);
        // 肩线角 = atan2(-21.84, 60) ≈ -20°；置信 0.2 → confidence_scale 0.4 → 权重 0.4×0.4=0.16
        // 头 0°(0.6) + 肩 -20°(0.16)：融合 = 0.16×(-20)/0.76 ≈ -4.21°（远小于固定权重 0.4×(-20)=-8°）
        assert!((measured + 3.0).abs() < 2.0, "低置信肩线应被降权，实际 {measured}");
        assert!(
            (measured + 8.0).abs() > 3.0,
            "不得接近固定权重结果 -8°，实际 {measured}"
        );
        assert!(w.is_empty(), "点存在只降权，不应告警");
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
    fn 俯仰超容差告警且正面与退化不告警() {
        let face_box = FaceBox {
            x1: 100.0,
            y1: 100.0,
            x2: 200.0,
            y2: 200.0,
            score: 0.99,
        };
        // 眼线 y=140、嘴线 y=185（跨度 45）：鼻尖 y=165 → 比例 0.556 ≈ 正面基准
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
        assert!(pitch_warning(&frontal).is_none(), "正面平视不应告警");
        // 低头：鼻尖下移到 y=185 → 比例 1.0，偏离基准 0.45 > 容差
        let down = FaceDetection {
            landmarks: [
                Point2::new(125.0, 140.0),
                Point2::new(175.0, 140.0),
                Point2::new(150.0, 185.0),
                Point2::new(133.0, 185.0),
                Point2::new(167.0, 185.0),
            ],
            ..frontal
        };
        let warn = pitch_warning(&down).unwrap();
        assert!(warn.contains("低头") && warn.contains("平视"), "{warn}");
        // 仰头：鼻尖上移到 y=145 → 比例 0.111，偏离基准 0.44
        let up = FaceDetection {
            landmarks: [
                Point2::new(125.0, 140.0),
                Point2::new(175.0, 140.0),
                Point2::new(150.0, 145.0),
                Point2::new(133.0, 185.0),
                Point2::new(167.0, 185.0),
            ],
            ..frontal
        };
        let warn = pitch_warning(&up).unwrap();
        assert!(warn.contains("仰头") && warn.contains("平视"), "{warn}");
        // 关键点重合（演示/桩数据退化）→ 无法判断，不告警
        let degenerate = FaceDetection {
            landmarks: [
                Point2::new(150.0, 140.0),
                Point2::new(150.0, 140.0),
                Point2::new(150.0, 140.0),
                Point2::new(150.0, 140.0),
                Point2::new(150.0, 140.0),
            ],
            ..frontal
        };
        assert!(pitch_warning(&degenerate).is_none());
    }

    #[test]
    fn 居中裁切回退与人脸框裁切同尺寸() {
        // 复用 compute_crop：人脸框退化为全图时仍产出目标尺寸
        let crop: CropRect = compute_crop(
            &FaceBox {
                x1: 0.0,
                y1: 0.0,
                x2: 100.0,
                y2: 140.0,
                score: 0.0,
            },
            100,
            140,
            295,
            413,
            CROP_TOP_RATIO,
            CROP_BOTTOM_RATIO,
        )
        .unwrap();
        assert!(crop.width > 0 && crop.height > 0);
        let img = RgbImage::from_pixel(100, 140, Rgb([10, 20, 30]));
        let out = crop_resize(&img, &crop, 295, 413).unwrap();
        assert_eq!(out.dimensions(), (295, 413));
    }

    #[test]
    fn 步骤参数缺省时取配置默认列表() {
        let cfg = Config {
            pipeline: PipelineConfig {
                steps: vec![STEP_READ_IMAGE.to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            effective_steps(&cfg, None),
            vec![STEP_READ_IMAGE.to_string()]
        );
    }
}
