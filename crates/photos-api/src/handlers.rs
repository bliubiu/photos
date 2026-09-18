//! 端点处理器（契约 docs/05-API契约.md §2）：
//! POST /tasks、GET /tasks、DELETE /tasks、GET /tasks/{id}、DELETE /tasks/{id}、
//! GET /tasks/{id}/output、GET /tasks/{id}/input、
//! GET /models、POST /models/download、GET /models/market、POST /models/register、
//! GET /models/versions、POST /models/activate、GET /config、GET /ping、
//! GET /metrics、GET /errors（可观测性）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Multipart, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use photos_core::config::{Config, ModelRole, ModelSpec, Preprocess};
use photos_core::market;
use photos_core::metrics::TaskMetrics;
use photos_core::model::{
    CheckStatus, active_version, check_models, download_model, download_version, list_versions,
    model_version_dir, resolve_model_path, sha256_file, version_path,
};
use photos_core::output::{OutputFormat, save_task_outputs};
use photos_core::pipeline::{ProcessRequest, run_pipeline_with_metrics};
use photos_core::storage::{NewTask, Store, TaskFilter, TaskRecord};

use crate::artifact;
use crate::engine_pool::EngineLease;
use crate::error::{ApiError, model_missing};

/// 引擎工厂：按输入图片尺寸借出推理引擎（真实 OrtEngine 复用进程级池中的引擎 / 演示
/// FakeEngine 每次新建；测试注入 stub）。尺寸参数用于演示引擎（需按图宽高回放），真实引擎忽略。
pub type EngineFactory = Arc<dyn Fn(u32, u32) -> EngineLease + Send + Sync>;

/// 应用状态（Config 只读共享；Store 由 Mutex 串行化 sqlite 访问）
pub struct AppState {
    pub cfg: Arc<Config>,
    pub store: Mutex<Store>,
    pub engine_factory: EngineFactory,
    /// 输出目录（data/out）
    pub out_dir: PathBuf,
    /// 上传暂存目录（data/tmp）
    pub upload_dir: PathBuf,
    /// POST /tasks 时是否预检模型就绪（契约 503 MODEL_MISSING；测试可关闭）
    pub model_precheck: bool,
    /// 推理并发上限（信号量；批量提交时排队执行，避免挤爆 CPU）
    pub slots: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    /// 构建状态；打开 data_dir/photos.db，创建 out/tmp 目录
    pub fn new(
        cfg: Config,
        engine_factory: EngineFactory,
        model_precheck: bool,
    ) -> photos_core::error::CoreResult<Self> {
        let store = Store::open(Path::new(&cfg.general.data_dir).join("photos.db").as_path())?;
        let out_dir = PathBuf::from(&cfg.general.data_dir).join("out");
        let upload_dir = PathBuf::from(&cfg.general.data_dir).join("tmp");
        std::fs::create_dir_all(&out_dir)?;
        std::fs::create_dir_all(&upload_dir)?;
        // 推理并发上限来自 [server]（校验保证 ≥1）
        let slots = Arc::new(tokio::sync::Semaphore::new(cfg.server.max_concurrent_tasks));
        Ok(Self {
            cfg: Arc::new(cfg),
            store: Mutex::new(store),
            engine_factory,
            out_dir,
            upload_dir,
            model_precheck,
            slots,
        })
    }

    /// 错误上报：结构化错误日志 + 落库 error_log（写库失败仅告警，不阻断主流程）
    pub fn report_error(&self, code: &str, stage: &str, message: &str, task_id: Option<i64>) {
        photos_core::logging::log_error(code, stage, message, task_id);
        let store = match self.store.lock() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("错误上报写库失败（存储锁异常）：{e}");
                return;
            }
        };
        if let Err(e) = store.record_error(code, stage, message, task_id) {
            tracing::warn!("错误上报写库失败：{e}");
        }
    }
}

/// 指标聚合窗口：`GET /metrics` 取最近 N 条任务做统计
const METRICS_WINDOW: i64 = 200;

/// 单文件上传大小上限（20MB）
const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// 请求参数（multipart 的 params 字段，JSON）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskParams {
    pub mode: Option<String>,
    pub size: Option<String>,
    pub backgrounds: Option<Vec<String>>,
    pub beauty: Option<BeautyParams>,
    pub dress: Option<DressParams>,
    pub rotate: Option<f64>,
    pub layout: Option<String>,
    pub effect_image: Option<bool>,
    /// 是否额外输出透明底 PNG（RGBA）
    pub transparent: Option<bool>,
    /// 自定义背景图路径（服务端本地路径，cover 缩放裁切后与人像合成）
    pub bg_image: Option<String>,
    /// 图片输出格式（jpg | webp；缺省取全局配置 `[output].format`）
    pub output_format: Option<String>,
    /// JPG 压缩质量 1..=100（缺省取全局配置 `[output].jpg_quality`；WebP 为无损不受影响）
    pub jpg_quality: Option<u8>,
    /// 排版相纸是否额外输出 PDF（缺省取全局配置 `[output].pdf`）
    pub pdf: Option<bool>,
    /// 工作流步骤表（可选；缺省取全局配置 `[pipeline] steps`，为空时取内置默认十步）
    pub steps: Option<Vec<String>>,
}

/// 美颜参数（enabled 开关；强度缺省取全局配置 `[beauty]` 默认值）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeautyParams {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub skin_smooth: Option<f64>,
    #[serde(default)]
    pub brighten: Option<f64>,
    #[serde(default)]
    pub whiten: Option<f64>,
}

/// 换装参数（enabled 开关；garment_path 为服务端已有服装图路径，style 为程序化正装；
/// garments 为分部位服装图集合，多图分部位贴合，优先于 garment_path/style）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DressParams {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub garment_path: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub garments: Option<GarmentSet>,
}

/// 分部位服装图集合（上衣/下装/鞋 分别贴合，未提供的部位自动跳过）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GarmentSet {
    #[serde(default)]
    pub top: Option<String>,
    #[serde(default)]
    pub bottom: Option<String>,
    #[serde(default)]
    pub shoes: Option<String>,
}

/// 校验美颜强度取值（0..=1，越界返回中文错误）
fn validate_beauty(beauty: &Option<BeautyParams>) -> Result<(), ApiError> {
    if let Some(b) = beauty {
        for (name, v) in [
            ("磨皮强度", b.skin_smooth),
            ("提亮强度", b.brighten),
            ("美白强度", b.whiten),
        ] {
            if let Some(x) = v {
                if !(0.0..=1.0).contains(&x) {
                    return Err(ApiError::InvalidParams(format!(
                        "{name}需在 0..=1 内，收到 {x}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// 校验输出参数（格式合法、JPG 压缩质量 1..=100）
fn validate_output(params: &TaskParams) -> Result<(), ApiError> {
    if let Some(f) = params.output_format.as_deref() {
        OutputFormat::parse(f).map_err(|e| ApiError::InvalidParams(e.to_string()))?;
    }
    if let Some(q) = params.jpg_quality {
        if !(1..=100).contains(&q) {
            return Err(ApiError::InvalidParams(format!(
                "JPG 压缩质量需在 1..=100 内，收到 {q}"
            )));
        }
    }
    Ok(())
}

/// 校验换装参数（启用时需提供服装图或合法正装样式）
fn validate_dress(dress: &Option<DressParams>) -> Result<(), ApiError> {
    if let Some(d) = dress {
        if !d.enabled {
            return Ok(());
        }
        // 分部位集合至少提供一个部位图时即视为有效（优先于 garment_path/style）
        let has_parts = d
            .garments
            .as_ref()
            .is_some_and(|g| g.top.is_some() || g.bottom.is_some() || g.shoes.is_some());
        if d.garment_path.is_none() && !has_parts {
            match d.style.as_deref() {
                Some(
                    "suit_navy" | "suit_black" | "shirt_white" | "suit_full_navy"
                    | "suit_full_black",
                ) => {}
                Some(other) => {
                    return Err(ApiError::InvalidParams(format!(
                        "未知正装样式“{other}”，可选：suit_navy、suit_black、shirt_white、suit_full_navy、suit_full_black"
                    )));
                }
                None => {
                    return Err(ApiError::InvalidParams(
                        "换装需提供 garment_path（服装图路径）、garments（分部位服装图）或 style（正装样式）"
                            .into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 校验工作流步骤表（未知步骤 / 重复步骤 / 依赖缺失均返回中文 400）
fn validate_steps(cfg: &Config, steps: &Option<Vec<String>>) -> Result<(), ApiError> {
    let Some(names) = steps.as_ref().filter(|l| !l.is_empty()) else {
        return Ok(());
    };
    let mut ops: Vec<&str> = Vec::with_capacity(names.len());
    for name in names {
        let op = photos_core::workflow::step_op(cfg, name).ok_or_else(|| {
            ApiError::InvalidParams(format!(
                "未知工作流步骤“{name}”，可选：{}",
                photos_core::workflow::available_steps(cfg).join("、")
            ))
        })?;
        if ops.contains(&op) {
            return Err(ApiError::InvalidParams(format!(
                "工作流步骤重复：{}",
                photos_core::workflow::step_stage(op).unwrap_or(op)
            )));
        }
        ops.push(op);
    }
    for op in &ops {
        for need in photos_core::workflow::step_requires(op) {
            if !ops.contains(need) {
                return Err(ApiError::InvalidParams(format!(
                    "工作流步骤「{}」依赖「{}」，请在步骤表中一并启用",
                    photos_core::workflow::step_stage(op).unwrap_or(op),
                    photos_core::workflow::step_stage(need).unwrap_or(need)
                )));
            }
        }
    }
    Ok(())
}

// ---------- 工具 ----------

/// 对外任务 id（task_{自增id}）
pub fn public_task_id(id: i64) -> String {
    format!("task_{id}")
}

/// 解析对外任务 id（兼容 "task_123" 与 "123"）
pub fn parse_task_id(public: &str) -> Option<i64> {
    let s = public.strip_prefix("task_").unwrap_or(public);
    s.parse::<i64>().ok()
}

/// 模式中文名（驱动前端下拉）
pub fn mode_label(id: &str) -> String {
    match id {
        "speed" => "极速".to_string(),
        "balanced" => "CPU 高性能".to_string(),
        "quality" => "GPU 高质量".to_string(),
        other => other.to_string(),
    }
}

fn is_supported_image(filename: &str) -> bool {
    matches!(
        Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "bmp")
    )
}

/// 上传文件名安全化（仅保留基名，防止路径穿越）
fn safe_upload_name(filename: &str) -> String {
    let base = Path::new(filename)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "upload".into());
    base.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------- 处理器 ----------

/// GET /ping
pub async fn ping() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

/// GET /config：驱动前端下拉的选项集
pub async fn get_config(State(state): State<Arc<AppState>>) -> Response {
    let cfg = &state.cfg;
    Json(json!({
        "default_mode": cfg.general.default_mode,
        "modes": cfg.modes.keys().map(|id| json!({ "id": id, "label": mode_label(id) })).collect::<Vec<_>>(),
        "sizes": cfg.sizes.iter().map(|(id, s)| json!({ "id": id, "name": s.name, "width_px": s.width_px, "height_px": s.height_px })).collect::<Vec<_>>(),
        "backgrounds": cfg.backgrounds.iter().map(|(id, b)| json!({ "id": id, "name": b.name, "rgb": b.rgb })).collect::<Vec<_>>(),
        "layouts": cfg.layout.iter().map(|(id, l)| json!({ "id": id, "name": l.name })).collect::<Vec<_>>(),
        "output": {
            "format": cfg.output.format,
            "jpg_quality": cfg.output.jpg_quality,
            "pdf": cfg.output.pdf,
        },
        "pipeline": {
            "steps": photos_core::workflow::step_metas(cfg).iter().map(|m| json!({
                "id": m.id,
                "label": m.label,
                "stage": m.stage,
                "requires": m.requires,
            })).collect::<Vec<_>>(),
            "effective": photos_core::workflow::effective_steps(cfg, None),
        },
    }))
    .into_response()
}

/// GET /models：模型注册表与校验状态（含角色、版本与内置标记）
pub async fn list_models(State(state): State<Arc<AppState>>) -> Response {
    let store = state.store.lock().unwrap();
    let statuses = match check_models(&state.cfg, &store) {
        Ok(s) => s,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let builtin = Config::default().models;
    Json(json!({
        "items": statuses.iter().map(|s| {
            let spec = state.cfg.models.get(&s.id);
            let versions = list_versions(&state.cfg, &s.id);
            let active = active_version(&state.cfg, &store, &s.id).ok().flatten();
            json!({
                "id": s.id,
                "path": s.path.display().to_string(),
                "ready": matches!(s.check_status, CheckStatus::Ready | CheckStatus::CachedOk),
                "check_status": s.check_status.code(),
                "message": s.message,
                "role": spec.map(|sp| sp.role.as_str()).unwrap_or("auto"),
                "version": path_version(&state.cfg, &s.id, &s.path),
                "versions": versions,
                "active_version": active,
                "builtin": builtin.contains_key(&s.id),
            })
        }).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// 从解析出的模型路径反推版本目录名（旧布局 / 注册表直连路径返回 None）
fn path_version(cfg: &Config, id: &str, path: &Path) -> Option<String> {
    let sub = path
        .parent()?
        .strip_prefix(model_version_dir(cfg, id))
        .ok()?;
    let text = sub.to_string_lossy().to_string();
    (!text.is_empty()).then_some(text)
}

/// GET /models/market：模型市场清单（内置 + 用户覆盖），标注可下载与本地已下载/激活状态
pub async fn list_market(State(state): State<Arc<AppState>>) -> Response {
    let cfg = &state.cfg;
    let entries = match market::load(Some(&market::overlay_path(cfg))) {
        Ok(e) => e,
        Err(e) => {
            let msg = e.to_string();
            state.report_error("MODEL_MARKET_INVALID", "模型市场", &msg, None);
            return ApiError::Internal(msg).into_response();
        }
    };
    let store = state.store.lock().unwrap();
    let items = entries
        .iter()
        .map(|e| {
            let versions = list_versions(cfg, &e.id);
            json!({
                "id": e.id,
                "version": e.version,
                "role": e.role.as_str(),
                "url": e.url,
                "sha256": e.sha256,
                "size": e.size,
                "license": e.license,
                "source": e.source,
                "enabled": e.enabled,
                "downloadable": e.downloadable(),
                "registered": cfg.models.contains_key(&e.id),
                "downloaded": versions.contains(&e.version),
                "active_version": active_version(cfg, &store, &e.id).ok().flatten(),
            })
        })
        .collect::<Vec<_>>();
    Json(json!({ "items": items })).into_response()
}

/// POST /models/register 请求体（插件化注册：任意 ONNX + 声明式预处理）
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    /// 模型 id（注册表键名，仅允许字母数字与 `_ - .`）
    pub id: String,
    /// onnx 文件路径（相对项目根或绝对路径）
    pub path: String,
    /// 输入张量约定
    pub input_dims: Vec<i64>,
    /// 模型角色（缺省 auto）
    #[serde(default)]
    pub role: Option<String>,
    /// 文件 sha256（缺省时若文件已存在则自动计算，否则报错）
    #[serde(default)]
    pub sha256: Option<String>,
    /// 预处理声明（缺省沿用内置约定）
    #[serde(default)]
    pub preprocess: Option<Preprocess>,
}

/// 模型 id 合法性：非空且不含路径分隔符（版本目录由其拼接，避免路径穿越）
fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// POST /models/register：注册自定义模型到 `<data_dir>/models.custom.toml`（重启后生效）
pub async fn register_model(
    State(state): State<Arc<AppState>>,
    body: Json<serde_json::Value>,
) -> Response {
    let cfg = state.cfg.clone();
    let req: RegisterRequest = match serde_json::from_value(body.0) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("注册请求体非法：{e}");
            state.report_error("MODEL_REGISTER_INVALID", "模型注册", &msg, None);
            return ApiError::ModelRegisterInvalid(msg).into_response();
        }
    };
    if !valid_model_id(&req.id) {
        return ApiError::ModelRegisterInvalid(format!(
            "模型 id“{}”非法：需非空且仅含字母、数字、下划线、连字符与点",
            req.id
        ))
        .into_response();
    }
    if req.path.trim().is_empty() {
        return ApiError::ModelRegisterInvalid("模型文件路径不能为空".into()).into_response();
    }
    if req.input_dims.is_empty() {
        return ApiError::ModelRegisterInvalid("input_dims 不能为空".into()).into_response();
    }
    let role = match ModelRole::parse(req.role.as_deref().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => return ApiError::ModelRegisterInvalid(e.to_string()).into_response(),
    };

    // sha256：显式提供则校验格式；缺省时文件已存在则自动计算，否则拒绝（无法校验完整性）
    let target = resolve_model_path(&cfg, Path::new(&req.path));
    let sha256 = match req
        .sha256
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => {
            let s = s.to_ascii_lowercase();
            if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
                return ApiError::ModelRegisterInvalid(
                    "sha256 非法：需 64 位十六进制字符串".into(),
                )
                .into_response();
            }
            s
        }
        None => {
            if !target.exists() {
                return ApiError::ModelRegisterInvalid(format!(
                    "未提供 sha256 且模型文件不存在：{}；请先放入文件或显式提供 sha256",
                    target.display()
                ))
                .into_response();
            }
            match sha256_file(&target) {
                Ok(v) => v,
                Err(e) => return ApiError::from(e).into_response(),
            }
        }
    };

    let spec = ModelSpec {
        path: req.path.clone(),
        sha256,
        input_dims: req.input_dims.clone(),
        enabled: true,
        download: None,
        role,
        preprocess: req.preprocess.unwrap_or_default(),
    };
    match market::register_custom(&cfg, &req.id, &spec) {
        Ok(replaced) => Json(json!({
            "id": req.id,
            "registered": true,
            "replaced": replaced,
            "restart_required": true,
            "message": format!(
                "模型“{}”已写入 {}，重启服务后生效",
                req.id,
                market::custom_registry_path(&cfg).display()
            ),
        }))
        .into_response(),
        Err(e) => {
            let msg = e.to_string();
            state.report_error("MODEL_REGISTER_INVALID", "模型注册", &msg, None);
            ApiError::ModelRegisterInvalid(msg).into_response()
        }
    }
}

/// GET /models/versions 查询参数
#[derive(Debug, Deserialize)]
pub struct VersionsQuery {
    pub id: String,
}

/// GET /models/versions：指定模型已下载版本与当前激活版本
pub async fn list_model_versions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<VersionsQuery>,
) -> Response {
    let cfg = &state.cfg;
    if !cfg.models.contains_key(&q.id) {
        return ApiError::ModelVersionUnknown(format!("未知模型 id“{}”", q.id)).into_response();
    }
    let store = state.store.lock().unwrap();
    Json(json!({
        "id": q.id,
        "versions": list_versions(cfg, &q.id),
        "active_version": active_version(cfg, &store, &q.id).ok().flatten(),
    }))
    .into_response()
}

/// POST /models/activate 请求体
#[derive(Debug, Deserialize)]
pub struct ActivateRequest {
    pub id: String,
    pub version: String,
}

/// POST /models/activate：切换 / 回滚到指定已下载版本（校验文件存在与 sha256）
pub async fn activate_model(
    State(state): State<Arc<AppState>>,
    body: Json<ActivateRequest>,
) -> Response {
    let cfg = state.cfg.clone();
    if !cfg.models.contains_key(&body.id) {
        return ApiError::ModelVersionUnknown(format!("未知模型 id“{}”", body.id)).into_response();
    }
    // 先判定版本是否存在：否则 CoreError::Model 会被映射为 503，语义不符
    if !list_versions(&cfg, &body.id).contains(&body.version) {
        let msg = format!(
            "模型“{}”未下载版本“{}”；已下载版本：{}",
            body.id,
            body.version,
            if list_versions(&cfg, &body.id).is_empty() {
                "无".to_string()
            } else {
                list_versions(&cfg, &body.id).join("、")
            }
        );
        state.report_error("MODEL_VERSION_UNKNOWN", "模型版本切换", &msg, None);
        return ApiError::ModelVersionUnknown(msg).into_response();
    }
    let result = {
        let store = state.store.lock().unwrap();
        photos_core::model::activate_version(&cfg, &store, &body.id, &body.version)
    };
    match result {
        Ok(()) => Json(json!({
            "id": body.id,
            "active_version": body.version,
            "message": "已切换激活版本（校验通过）",
        }))
        .into_response(),
        Err(e) => {
            let msg = e.to_string();
            state.report_error("MODEL_VERSION_UNKNOWN", "模型版本切换", &msg, None);
            ApiError::ModelMissing(msg).into_response()
        }
    }
}

/// POST `/models/download` 请求体：`ids` 缺省（或空数组）表示下载全部「缺失」模型；
/// `version` 有值时按市场清单条目下载到 `models/<id>/<版本>/`（需显式指定 `ids`）
#[derive(Debug, Default, Deserialize)]
pub struct DownloadParams {
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    #[serde(default)]
    pub version: Option<String>,
}

/// POST /models/download：一键下载模型到注册表路径（指定 `version` 时走市场清单版本化下载）。
/// 未指定 `ids` 时下载全部缺失（文件不存在）的模型；已有文件视为成功，单个失败不阻断其余。
/// 下载为阻塞 IO，放入 `spawn_blocking` 执行，避免占用异步运行时线程。
pub async fn download_models(
    State(state): State<Arc<AppState>>,
    body: Option<Json<DownloadParams>>,
) -> Response {
    let cfg = state.cfg.clone();
    let params = body.map(|Json(p)| p).unwrap_or_default();
    let requested = params.ids.unwrap_or_default();
    let version = params.version.filter(|v| !v.trim().is_empty());
    if version.is_some() && requested.is_empty() {
        return ApiError::InvalidParams(
            "指定 version 时需同时提供 ids（市场清单下载需明确模型）".into(),
        )
        .into_response();
    }
    let ids: Vec<String> = if requested.is_empty() {
        let store = state.store.lock().unwrap();
        match check_models(&cfg, &store) {
            Ok(s) => s
                .into_iter()
                .filter(|s| s.check_status == CheckStatus::Missing)
                .map(|s| s.id)
                .collect(),
            Err(e) => return ApiError::from(e).into_response(),
        }
    } else {
        if let Some(bad) = requested.iter().find(|id| !cfg.models.contains_key(*id)) {
            return ApiError::InvalidParams(format!("未知模型 id“{bad}”")).into_response();
        }
        requested
    };

    let joined = match version {
        Some(v) => tokio::task::spawn_blocking(move || download_version_each(&cfg, ids, v)).await,
        None => tokio::task::spawn_blocking(move || download_each(&cfg, ids)).await,
    };
    let items = match joined {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("下载线程异常：{e}");
            state.report_error("INTERNAL", "模型下载", &msg, None);
            return ApiError::Internal(msg).into_response();
        }
    };
    // 失败项统一上报（结构化日志 + 落库），不阻断其余下载结果
    for it in &items {
        if it["ok"] == false {
            let id = it["id"].as_str().unwrap_or("");
            let msg = it["message"].as_str().unwrap_or("下载失败");
            state.report_error(
                "MODEL_MISSING",
                "模型下载",
                &format!("模型“{id}”下载失败：{msg}"),
                None,
            );
        }
    }
    Json(json!({ "items": items })).into_response()
}

/// 逐个下载模型：已有文件直接成功；其余调用下载器，单个失败不阻断（返回中文原因）
fn download_each(cfg: &Config, ids: Vec<String>) -> Vec<serde_json::Value> {
    ids.into_iter()
        .map(|id| {
            let existing = cfg
                .model_spec(&id)
                .map(|s| resolve_model_path(cfg, Path::new(&s.path)).exists())
                .unwrap_or(false);
            if existing {
                return json!({ "id": id, "ok": true, "message": "模型文件已存在，无需下载" });
            }
            tracing::info!("开始下载模型“{id}”…");
            match download_model(cfg, &id) {
                Ok(()) => {
                    tracing::info!("模型“{id}”下载完成");
                    json!({ "id": id, "ok": true, "message": "下载完成" })
                }
                Err(e) => {
                    let msg = e.to_string();
                    tracing::error!("模型“{id}”下载失败：{msg}");
                    json!({ "id": id, "ok": false, "message": msg })
                }
            }
        })
        .collect()
}

/// 按市场清单版本逐个下载：条目需 `downloadable()`（已启用且直链与 sha256 齐备）。
/// 版本化下载需要 `prefs` 记录激活版本，故在阻塞线程内独立打开 sqlite 连接（避免跨 await 持锁）。
fn download_version_each(
    cfg: &Config,
    ids: Vec<String>,
    version: String,
) -> Vec<serde_json::Value> {
    let entries = match market::load(Some(&market::overlay_path(cfg))) {
        Ok(e) => e,
        Err(e) => {
            return vec![json!({ "id": "", "ok": false, "message": e.to_string() })];
        }
    };
    let db = Path::new(&cfg.general.data_dir).join("photos.db");
    let store = match Store::open(&db) {
        Ok(s) => s,
        Err(e) => {
            return vec![json!({
                "id": "",
                "ok": false,
                "message": format!("打开数据库 {} 失败：{e}", db.display()),
            })];
        }
    };
    ids.into_iter()
        .map(|id| {
            let spec = match cfg.model_spec(&id) {
                Ok(s) => s,
                Err(e) => {
                    return json!({ "id": id, "ok": false, "message": e.to_string() });
                }
            };
            if version_path(cfg, &id, &version, Path::new(&spec.path)).exists() {
                return json!({
                    "id": id,
                    "version": version,
                    "ok": true,
                    "message": "该版本已存在，无需下载",
                });
            }
            let Some(entry) = entries.iter().find(|e| e.id == id && e.version == version) else {
                return json!({
                    "id": id,
                    "version": version,
                    "ok": false,
                    "message": format!("市场清单中未找到模型“{id}”的版本“{version}”"),
                });
            };
            if !entry.downloadable() {
                return json!({
                    "id": id,
                    "version": version,
                    "ok": false,
                    "message": format!(
                        "条目未启用下载或缺少直链/sha256（可在 {} 中补齐后重试）",
                        market::overlay_path(cfg).display()
                    ),
                });
            }
            tracing::info!("开始下载模型“{id}”版本“{version}”…");
            match download_version(cfg, &store, &id, &version, &entry.url, &entry.sha256) {
                Ok(path) => json!({
                    "id": id,
                    "version": version,
                    "ok": true,
                    "message": "下载完成并已激活该版本",
                    "path": path.display().to_string(),
                }),
                Err(e) => {
                    let msg = e.to_string();
                    tracing::error!("模型“{id}”版本“{version}”下载失败：{msg}");
                    json!({ "id": id, "version": version, "ok": false, "message": msg })
                }
            }
        })
        .collect()
}

/// POST /tasks：multipart 提交（file + params JSON）→ 202 任务 id
pub async fn create_task(State(state): State<Arc<AppState>>, mut multipart: Multipart) -> Response {
    match create_task_inner(&state, &mut multipart).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn create_task_inner(
    state: &Arc<AppState>,
    multipart: &mut Multipart,
) -> Result<Response, ApiError> {
    // 1. 收集 multipart 字段
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;
    let mut params_text: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::InvalidParams(format!("读取上传字段失败：{e}")))?
    {
        match field.name().unwrap_or("") {
            "file" => {
                file_name = field.file_name().map(|s| s.to_string());
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::InvalidParams(format!("读取文件内容失败：{e}")))?;
                file_bytes = Some(bytes.to_vec());
            }
            "params" => {
                params_text = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| ApiError::InvalidParams(format!("读取参数失败：{e}")))?,
                );
            }
            _ => {}
        }
    }

    // 2. 文件校验（存在 / 大小 / 类型）
    let file_bytes =
        file_bytes.ok_or_else(|| ApiError::InvalidParams("缺少上传文件字段 file".into()))?;
    if file_bytes.is_empty() {
        return Err(ApiError::InvalidParams("上传文件为空".into()));
    }
    if file_bytes.len() > MAX_UPLOAD_BYTES {
        return Err(ApiError::FileTooLarge(format!(
            "文件超过上限 {}MB",
            MAX_UPLOAD_BYTES / 1024 / 1024
        )));
    }
    let raw_name = file_name.unwrap_or_default();
    if !is_supported_image(&raw_name) {
        return Err(ApiError::UnsupportedMedia(format!(
            "不支持的图片格式“{raw_name}”（仅支持 jpg/jpeg/png/bmp）"
        )));
    }

    // 3. 解析参数
    let params: TaskParams = match &params_text {
        Some(t) if !t.trim().is_empty() => serde_json::from_str(t)
            .map_err(|e| ApiError::InvalidParams(format!("params 解析失败：{e}")))?,
        _ => TaskParams::default(),
    };
    let cfg = &state.cfg;
    let mode = params
        .mode
        .clone()
        .unwrap_or_else(|| cfg.general.default_mode.clone());
    cfg.mode(&mode)
        .map_err(|e| ApiError::InvalidParams(e.to_string()))?;
    let size = params.size.clone().unwrap_or_else(|| "one_inch".into());
    // 尺寸支持自定义形式（`px:295x413` / `mm:35x45@300`），落库与产物命名使用归一化 id
    let (size, _) = cfg
        .resolve_size(&size)
        .map_err(|e| ApiError::InvalidParams(e.to_string()))?;
    let bgs = params
        .backgrounds
        .clone()
        .unwrap_or_else(|| vec!["white".into()]);
    if bgs.is_empty() {
        return Err(ApiError::InvalidParams("底色列表不能为空".into()));
    }
    // 底色支持自定义形式（`#RRGGBB` / `rgb:R,G,B`），落库与产物命名使用归一化 id
    let bgs = bgs
        .iter()
        .map(|bg| {
            cfg.resolve_background(bg)
                .map(|(id, _)| id)
                .map_err(|e| ApiError::InvalidParams(e.to_string()))
        })
        .collect::<Result<Vec<String>, ApiError>>()?;
    if let Some(layout) = &params.layout {
        if !cfg.layout.contains_key(layout) {
            return Err(ApiError::InvalidParams(format!("未知排版“{layout}”")));
        }
    }
    if let Some(r) = params.rotate {
        if !(-45.0..=45.0).contains(&r) {
            return Err(ApiError::InvalidParams(format!(
                "手动纠偏角度需在 ±45° 内，收到 {r}°"
            )));
        }
    }
    let effect = params.effect_image.unwrap_or(false);
    validate_beauty(&params.beauty)?;
    validate_dress(&params.dress)?;
    validate_output(&params)?;
    validate_steps(&state.cfg, &params.steps)?;

    // 4. 模型预检（就绪才受理；缺失返回 503，不自动下载以免阻塞）
    if state.model_precheck {
        let store = state.store.lock().unwrap();
        let statuses = check_models(cfg, &store).map_err(ApiError::from)?;
        let suite = cfg
            .mode(&mode)
            .map_err(|e| ApiError::InvalidParams(e.to_string()))?;
        let suite_ids = if suite.face == photos_core::vision::mtcnn::CASCADE_FACE_ID {
            let mut ids: Vec<String> = photos_core::vision::mtcnn::cascade_model_ids()
                .iter()
                .map(|s| s.to_string())
                .collect();
            ids.push(suite.keypoint.clone());
            ids.push(suite.matting.clone());
            ids
        } else {
            vec![
                suite.face.clone(),
                suite.keypoint.clone(),
                suite.matting.clone(),
            ]
        };
        // 仅“文件缺失”视为未就绪（503）；hash 占位/不一致不拦截，推理可继续
        if let Some(s) = statuses
            .iter()
            .find(|s| suite_ids.contains(&s.id) && s.check_status == CheckStatus::Missing)
        {
            return Err(model_missing(&s.id, &s.message));
        }
    }

    // 5. 保存上传文件 + 落库 queued
    // 提交参数快照（尺寸/底色为归一化 id，前端可直接回传复用）
    let params_json = json!({
        "mode": mode,
        "size": size,
        "backgrounds": bgs,
        "layout": params.layout,
        "effect_image": effect,
        "rotate": params.rotate,
        "transparent": params.transparent.unwrap_or(false),
        "bg_image": params.bg_image,
        "output_format": params.output_format,
        "jpg_quality": params.jpg_quality,
        "pdf": params.pdf,
        "steps": params.steps,
    })
    .to_string();
    let upload_name = format!(
        "{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        safe_upload_name(&raw_name)
    );
    let input_path = state.upload_dir.join(upload_name);
    std::fs::write(&input_path, &file_bytes)
        .map_err(|e| ApiError::Internal(format!("保存上传文件失败：{e}")))?;

    let beauty_json = match &params.beauty {
        Some(b) if b.enabled => serde_json::json!({
            "enabled": true,
            "skin_smooth": b.skin_smooth,
            "brighten": b.brighten,
            "whiten": b.whiten,
        })
        .to_string(),
        _ => "{}".to_string(),
    };
    let dress_json = match &params.dress {
        Some(d) if d.enabled => serde_json::json!({
            "enabled": true,
            "garment_path": d.garment_path,
            "style": d.style,
            "garments": d.garments.as_ref().map(|g| serde_json::json!({
                "top": g.top,
                "bottom": g.bottom,
                "shoes": g.shoes,
            })),
        })
        .to_string(),
        _ => "{}".to_string(),
    };
    let (task_id, created_at) = {
        let store = state.store.lock().unwrap();
        let id = store
            .insert_task(&NewTask {
                input_path: input_path.display().to_string(),
                mode: mode.clone(),
                size: size.clone(),
                backgrounds: bgs.join(","),
                beauty: beauty_json,
                dress: dress_json,
                rotate: params.rotate,
                params: params_json,
                outputs: String::new(),
                status: "queued".into(),
                message: "已入队，等待处理".into(),
                warnings: String::new(),
                elapsed_ms: None,
            })
            .map_err(ApiError::from)?;
        let rec = store
            .get_task(id)
            .map_err(ApiError::from)?
            .ok_or(ApiError::Internal("任务入库后查询失败".into()))?;
        (id, rec.created_at)
    };

    // 6. 后台异步处理（状态机 queued → running → succeeded | failed）
    spawn_task(
        state.clone(),
        task_id,
        TaskParams {
            mode: Some(mode),
            size: Some(size),
            backgrounds: Some(bgs),
            rotate: params.rotate,
            layout: params.layout,
            effect_image: Some(effect),
            beauty: params.beauty,
            dress: params.dress,
            transparent: params.transparent,
            bg_image: params.bg_image,
            output_format: params.output_format,
            jpg_quality: params.jpg_quality,
            pdf: params.pdf,
            steps: params.steps,
        },
        input_path,
    );

    // 7. 202 返回
    let mut resp = Json(json!({
        "id": public_task_id(task_id),
        "status": "queued",
        "created_at": created_at,
    }))
    .into_response();
    *resp.status_mut() = StatusCode::ACCEPTED;
    Ok(resp)
}

/// 后台任务：running → 推理 → 产物落盘 → succeeded / failed
fn spawn_task(state: Arc<AppState>, task_id: i64, params: TaskParams, input: PathBuf) {
    tokio::spawn(async move {
        // 等待并发许可（排队），获得后开始处理
        let _permit = match state.slots.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::error!("信号量关闭，任务 {task_id} 无法执行");
                return;
            }
        };
        {
            let store = state.store.lock().unwrap();
            if let Err(e) =
                store.update_task(task_id, "running", "开始处理", "[]", "[]", None, None)
            {
                tracing::error!("更新任务运行状态失败：{e}");
                return;
            }
        }

        let mode = params.mode.clone().unwrap_or_default();
        let size = params.size.clone().unwrap_or_else(|| "one_inch".into());
        let bgs = params.backgrounds.clone().unwrap_or_default();
        let effect = params.effect_image.unwrap_or(false);
        let layout = params.layout.clone();
        let rotate = params.rotate;
        // 落盘选项：请求参数覆盖配置 `[output]` 默认值（格式与质量已在受理处校验）
        let mut out_opts = photos_core::output::OutputOptions::from_config(&state.cfg);
        if let Some(f) = params.output_format.as_deref() {
            out_opts.format = OutputFormat::parse(f).unwrap_or(out_opts.format);
        }
        if let Some(q) = params.jpg_quality {
            out_opts.jpg_quality = q;
        }
        if let Some(p) = params.pdf {
            out_opts.pdf = p;
        }
        let beauty = params
            .beauty
            .clone()
            .map(|b| photos_core::pipeline::BeautyParams {
                enabled: b.enabled,
                skin_smooth: b.skin_smooth,
                brighten: b.brighten,
                whiten: b.whiten,
            });
        let dress = params
            .dress
            .clone()
            .map(|d| photos_core::pipeline::DressParams {
                enabled: d.enabled,
                garment: d.garment_path.map(PathBuf::from),
                style: d.style,
                garments: d.garments.map(|g| photos_core::pipeline::GarmentSet {
                    top: g.top.map(PathBuf::from),
                    bottom: g.bottom.map(PathBuf::from),
                    shoes: g.shoes.map(PathBuf::from),
                }),
            });

        let started = std::time::Instant::now();
        let state2 = state.clone();
        let req = ProcessRequest {
            input,
            mode,
            size: size.clone(),
            bgs: bgs.clone(),
            rotate,
            effect,
            layout: layout.clone(),
            beauty,
            dress,
            transparent: params.transparent.unwrap_or(false),
            bg_image: params.bg_image.clone().map(PathBuf::from),
            steps: params.steps.clone(),
        };
        let result = tokio::task::spawn_blocking(move || {
            let (w, h) = image::image_dimensions(&req.input).unwrap_or((640, 640));
            // 与流水线内部预缩放对齐：引擎按缩放后尺寸构造
            let (w, h) =
                photos_core::pipeline::limited_dimensions(w, h, state2.cfg.general.max_input_side);
            // 借出引擎（生产模式来自进程级池，复用已装载模型的引擎；用完自动归还）
            let mut lease = (state2.engine_factory)(w, h);
            // 分阶段耗时指标：失败时仍保留已记录阶段，供错误上报定位
            let mut metrics = TaskMetrics::new();
            let r = run_pipeline_with_metrics(&state2.cfg, lease.engine_mut(), &req, &mut metrics);
            // 推理层失败（会话损坏、装载异常等）会污染引擎：显式驱逐，避免坏会话长期留在池中
            if matches!(r, Err(photos_core::error::CoreError::Inference(_))) {
                lease.mark_broken();
            }
            (r, metrics)
        })
        .await;

        let elapsed = started.elapsed().as_millis() as i64;
        let (outcome, metrics) = match result {
            Ok((Ok(r), metrics)) => {
                // 产物落盘（命名规约集中在 photos_core::output，与 CLI 一致）
                let layout_spec = layout.as_deref().and_then(|id| state.cfg.layout.get(id));
                let saved = match save_task_outputs(
                    &state.out_dir,
                    task_id,
                    &size,
                    layout_spec,
                    layout.as_deref(),
                    &r,
                    &out_opts,
                ) {
                    Ok(paths) => Ok((
                        paths
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>(),
                        r.warnings,
                    )),
                    Err(e) => Err(e.to_string()),
                };
                (saved, metrics)
            }
            Ok((Err(e), metrics)) => (Err(e.to_string()), metrics),
            Err(e) => (Err(format!("任务执行异常：{e}")), TaskMetrics::new()),
        };
        // 失败阶段的定位：取最后一个已记录阶段，未记录任何阶段时归为「处理」
        let failed_stage = metrics.last_stage().unwrap_or("处理").to_string();
        let metrics_json = metrics.to_json();

        {
            let store = state.store.lock().unwrap();
            match &outcome {
                Ok((outputs, warnings)) => {
                    let outputs_json =
                        serde_json::to_string(outputs).unwrap_or_else(|_| "[]".into());
                    let warnings_json =
                        serde_json::to_string(warnings).unwrap_or_else(|_| "[]".into());
                    if let Err(e) = store.update_task(
                        task_id,
                        "succeeded",
                        "处理完成",
                        &outputs_json,
                        &warnings_json,
                        Some(elapsed),
                        Some(&metrics_json),
                    ) {
                        tracing::error!("更新任务成功状态失败：{e}");
                    }
                }
                Err(msg) => {
                    if let Err(e) =
                        store.update_task(task_id, "failed", msg, "[]", "[]", Some(elapsed), None)
                    {
                        tracing::error!("更新任务失败状态出错：{e}");
                    }
                }
            }
        }
        match &outcome {
            // 成功：输出分阶段耗时结构化日志
            Ok(_) => photos_core::logging::log_metrics(task_id, &metrics),
            // 失败：统一错误上报（结构化日志 + error_log 落库）
            Err(msg) => state.report_error("INTERNAL", &failed_stage, msg, Some(task_id)),
        }
    });
}

/// GET /tasks：历史任务分页列表（支持状态/模式/尺寸/底色/起始时间筛选）
#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// 任务状态：queued | running | succeeded | failed
    pub status: Option<String>,
    /// 运行模式 id
    pub mode: Option<String>,
    /// 尺寸 id（内置 id 或自定义形式 `px:宽x高` / `mm:宽x高@DPI`）
    pub size: Option<String>,
    /// 底色 id（内置 id 或自定义形式 `#RRGGBB` / `rgb:R,G,B`）
    pub background: Option<String>,
    /// 起始创建时间（`YYYY-MM-DD`，按文本比较）
    pub since: Option<String>,
}

/// 逗号分隔列 → 字符串数组（空串 → 空数组）
fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// 解析并校验筛选条件（非法取值返回 400；尺寸/底色归一化为落库 id 后再比较）
fn parse_filter(cfg: &Config, q: &PageQuery) -> Result<TaskFilter, ApiError> {
    let opt = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let status = match opt(&q.status).as_deref() {
        Some(s @ ("queued" | "running" | "succeeded" | "failed")) => Some(s.to_string()),
        Some(other) => {
            return Err(ApiError::InvalidParams(format!(
                "未知任务状态“{other}”，可选：queued、running、succeeded、failed"
            )));
        }
        None => None,
    };
    let mode = match opt(&q.mode) {
        Some(m) => {
            cfg.mode(&m)
                .map_err(|e| ApiError::InvalidParams(e.to_string()))?;
            Some(m)
        }
        None => None,
    };
    let size = match opt(&q.size) {
        Some(s) => Some(
            cfg.resolve_size(&s)
                .map_err(|e| ApiError::InvalidParams(e.to_string()))?
                .0,
        ),
        None => None,
    };
    let background = match opt(&q.background) {
        Some(b) => Some(
            cfg.resolve_background(&b)
                .map_err(|e| ApiError::InvalidParams(e.to_string()))?
                .0,
        ),
        None => None,
    };
    Ok(TaskFilter {
        status,
        mode,
        size,
        background,
        since: opt(&q.since),
    })
}

pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PageQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = q.offset.unwrap_or(0).max(0);
    let filter = match parse_filter(&state.cfg, &q) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let store = state.store.lock().unwrap();
    let total = match store.count_tasks_filtered(&filter) {
        Ok(t) => t,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let rows = match store.list_tasks_filtered(&filter, limit, offset) {
        Ok(r) => r,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|t| {
            let outputs: Vec<String> = artifact::parse_outputs(&t.outputs)
                .into_iter()
                .map(|a| a.filename)
                .collect();
            json!({
                "id": public_task_id(t.id),
                "input_path": t.input_path,
                "mode": t.mode,
                "size": t.size,
                "backgrounds": split_csv(&t.backgrounds),
                "status": t.status,
                "message": if t.message.is_empty() { serde_json::Value::Null } else { json!(t.message) },
                "created_at": t.created_at,
                "elapsed_ms": t.elapsed_ms,
                "outputs": outputs,
            })
        })
        .collect();
    Json(json!({ "total": total, "items": items })).into_response()
}

/// GET /tasks/{id}：轮询状态、告警、产物清单与提交参数
pub async fn get_task(
    State(state): State<Arc<AppState>>,
    AxumPath(task_public): AxumPath<String>,
) -> Response {
    let id = match parse_task_id(&task_public) {
        Some(id) => id,
        None => return ApiError::TaskNotFound.into_response(),
    };
    let store = state.store.lock().unwrap();
    let record = match store.get_task(id) {
        Ok(Some(r)) => r,
        Ok(None) => return ApiError::TaskNotFound.into_response(),
        Err(e) => return ApiError::from(e).into_response(),
    };
    let artifacts: Vec<serde_json::Value> = artifact::parse_outputs(&record.outputs)
        .into_iter()
        .map(|a| {
            json!({
                "kind": a.kind,
                "background": a.background,
                "layout": a.layout,
                "filename": a.filename,
            })
        })
        .collect();
    // 提交参数（JSON 文本；历史库为空时返回 null）
    let params: serde_json::Value =
        serde_json::from_str(&record.params).unwrap_or(serde_json::Value::Null);
    // 分阶段耗时指标（未采集时为空数组）
    let metrics: Vec<serde_json::Value> = TaskMetrics::from_json(&record.metrics)
        .stages
        .into_iter()
        .map(|s| json!({ "stage": s.stage, "ms": s.ms }))
        .collect();
    Json(json!({
        "id": public_task_id(record.id),
        "status": record.status,
        "message": if record.message.is_empty() { serde_json::Value::Null } else { json!(record.message) },
        "warnings": serde_json::from_str::<Vec<String>>(&record.warnings).unwrap_or_default(),
        "mode": record.mode,
        "size": record.size,
        "backgrounds": split_csv(&record.backgrounds),
        "rotate": record.rotate,
        "params": params,
        "beauty": record.beauty,
        "dress": record.dress,
        "elapsed_ms": record.elapsed_ms,
        "metrics": metrics,
        "created_at": record.created_at,
        "artifacts": artifacts,
    }))
    .into_response()
}

/// GET /metrics：任务统计、平均耗时、各阶段平均耗时与错误总数（可观测性面板数据源）
pub async fn get_metrics(State(state): State<Arc<AppState>>) -> Response {
    let store = state.store.lock().unwrap();
    let counts = match store.count_by_status() {
        Ok(c) => c,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let status_count = |s: &str| {
        counts
            .iter()
            .find(|(k, _)| k == s)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    };
    let (avg_ms, samples) = match store.elapsed_stats(METRICS_WINDOW) {
        Ok(v) => v,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let stages = match store.metrics_aggregate(METRICS_WINDOW) {
        Ok(v) => v,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let errors = match store.count_errors() {
        Ok(n) => n,
        Err(e) => return ApiError::from(e).into_response(),
    };
    // 耗时统一保留一位小数，便于前端直接展示
    let round1 = |v: f64| (v * 10.0).round() / 10.0;
    Json(json!({
        "tasks": {
            "total": counts.iter().map(|(_, n)| *n).sum::<i64>(),
            "queued": status_count("queued"),
            "running": status_count("running"),
            "succeeded": status_count("succeeded"),
            "failed": status_count("failed"),
        },
        "elapsed_ms": { "avg": round1(avg_ms), "samples": samples },
        "stages": stages
            .iter()
            .map(|s| json!({ "stage": s.stage, "avg_ms": round1(s.avg_ms), "samples": s.samples }))
            .collect::<Vec<_>>(),
        "errors": { "total": errors },
    }))
    .into_response()
}

/// GET /errors 查询参数（`limit` 钳制 1..=200）
#[derive(Debug, Deserialize)]
pub struct ErrorsQuery {
    pub limit: Option<i64>,
}

/// GET /errors：最近错误上报列表（倒序）
pub async fn list_errors(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ErrorsQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(20).clamp(1, 200);
    let store = state.store.lock().unwrap();
    let total = match store.count_errors() {
        Ok(n) => n,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let items = match store.list_errors(limit) {
        Ok(v) => v,
        Err(e) => return ApiError::from(e).into_response(),
    };
    Json(json!({
        "total": total,
        "items": items
            .iter()
            .map(|e| json!({
                "id": e.id,
                "created_at": e.created_at,
                "code": e.code,
                "stage": e.stage,
                "message": e.message,
                "task_id": e.task_id.map(public_task_id),
            }))
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

/// 删除任务占用的磁盘文件（仅限 out/tmp 目录内，防越界删除）；返回实际删除的文件数
fn purge_task_files(state: &AppState, record: &TaskRecord) -> usize {
    let mut paths: Vec<PathBuf> = serde_json::from_str::<Vec<String>>(&record.outputs)
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect();
    if !record.input_path.is_empty() {
        paths.push(PathBuf::from(&record.input_path));
    }
    let mut removed = 0;
    for p in paths {
        if (p.starts_with(&state.out_dir) || p.starts_with(&state.upload_dir))
            && std::fs::remove_file(&p).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// DELETE /tasks/{id}：删除任务记录，并连带删除磁盘产物与上传原图
pub async fn delete_task(
    State(state): State<Arc<AppState>>,
    AxumPath(task_public): AxumPath<String>,
) -> Response {
    let id = match parse_task_id(&task_public) {
        Some(id) => id,
        None => return ApiError::TaskNotFound.into_response(),
    };
    let record = {
        let store = state.store.lock().unwrap();
        match store.get_task(id) {
            Ok(Some(r)) => r,
            Ok(None) => return ApiError::TaskNotFound.into_response(),
            Err(e) => return ApiError::from(e).into_response(),
        }
    };
    let deleted_outputs = purge_task_files(&state, &record);
    let result = {
        let store = state.store.lock().unwrap();
        store.delete_task(id)
    };
    match result {
        Ok(_) => Json(json!({
            "id": public_task_id(id),
            "deleted_outputs": deleted_outputs,
        }))
        .into_response(),
        Err(e) => {
            state.report_error("INTERNAL", "删除任务", &e.to_string(), Some(id));
            ApiError::from(e).into_response()
        }
    }
}

/// DELETE /tasks：清空历史（连带删除全部磁盘产物与原图）
pub async fn clear_tasks(State(state): State<Arc<AppState>>) -> Response {
    let records = {
        let store = state.store.lock().unwrap();
        match store.list_all_tasks() {
            Ok(r) => r,
            Err(e) => return ApiError::from(e).into_response(),
        }
    };
    for r in &records {
        purge_task_files(&state, r);
    }
    let result = {
        let store = state.store.lock().unwrap();
        store.clear_tasks()
    };
    match result {
        Ok(n) => Json(json!({ "deleted": n })).into_response(),
        Err(e) => {
            state.report_error("INTERNAL", "清空历史", &e.to_string(), None);
            ApiError::from(e).into_response()
        }
    }
}

/// GET /tasks/{id}/input：读取上传原图（供历史记录「原图/结果」对比）
pub async fn task_input(
    State(state): State<Arc<AppState>>,
    AxumPath(task_public): AxumPath<String>,
) -> Response {
    let id = match parse_task_id(&task_public) {
        Some(id) => id,
        None => return ApiError::TaskNotFound.into_response(),
    };
    let path = {
        let store = state.store.lock().unwrap();
        match store.get_task(id) {
            Ok(Some(r)) => PathBuf::from(r.input_path),
            Ok(None) => return ApiError::TaskNotFound.into_response(),
            Err(e) => return ApiError::from(e).into_response(),
        }
    };
    // 仅允许读取上传目录内的文件
    if !path.starts_with(&state.upload_dir) {
        return ApiError::ArtifactNotFound("原图不存在".into()).into_response();
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return ApiError::ArtifactNotFound(format!("原图文件缺失：{e}")).into_response();
        }
    };
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(artifact::content_type(&filename)),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("inline; filename=\"{filename}\""))
            .unwrap_or_else(|_| HeaderValue::from_static("inline")),
    );
    (headers, bytes).into_response()
}

/// GET /tasks/{id}/output：下载指定产物 / bundle zip
#[derive(Debug, Deserialize)]
pub struct OutputQuery {
    pub artifact: Option<String>,
    pub background: Option<String>,
    pub layout: Option<String>,
}

pub async fn task_output(
    State(state): State<Arc<AppState>>,
    AxumPath(task_public): AxumPath<String>,
    Query(q): Query<OutputQuery>,
) -> Response {
    let id = match parse_task_id(&task_public) {
        Some(id) => id,
        None => return ApiError::TaskNotFound.into_response(),
    };
    let store = state.store.lock().unwrap();
    let record = match store.get_task(id) {
        Ok(Some(r)) => r,
        Ok(None) => return ApiError::TaskNotFound.into_response(),
        Err(e) => return ApiError::from(e).into_response(),
    };
    let artifacts = artifact::parse_outputs(&record.outputs);
    let kind = q.artifact.as_deref().unwrap_or("id_photo");

    // bundle：全部产物 zip 打包
    if kind == "bundle" {
        let bytes = match artifact::bundle_zip(&artifacts) {
            Ok(b) => b,
            Err(e) => {
                return ApiError::Internal(format!("打包下载失败：{e}")).into_response();
            }
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/zip"),
        );
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!(
                "attachment; filename=\"{}_bundle.zip\"",
                public_task_id(id)
            ))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        );
        return (headers, bytes).into_response();
    }

    let picked = match artifact::pick(
        &artifacts,
        kind,
        q.background.as_deref(),
        q.layout.as_deref(),
    ) {
        Some(a) => a,
        None => {
            return ApiError::ArtifactNotFound(format!("产物不存在：artifact={kind}"))
                .into_response();
        }
    };
    if !picked.path.exists() {
        return ApiError::ArtifactNotFound(format!("产物文件缺失：{}", picked.filename))
            .into_response();
    }
    let bytes = match std::fs::read(&picked.path) {
        Ok(b) => b,
        Err(e) => return ApiError::Internal(format!("读取产物失败：{e}")).into_response(),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(artifact::content_type(&picked.filename)),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("inline; filename=\"{}\"", picked.filename))
            .unwrap_or_else(|_| HeaderValue::from_static("inline")),
    );
    (headers, bytes).into_response()
}
