//! 8 个端点处理器（契约 docs/05-API契约.md §2）：
//! POST /tasks、GET /tasks、GET /tasks/{id}、GET /tasks/{id}/output、
//! GET /models、POST /models/download、GET /config、GET /ping。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Multipart, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use photos_core::config::Config;
use photos_core::model::{CheckStatus, check_models, download_model, resolve_model_path};
use photos_core::output::{OutputFormat, save_task_outputs};
use photos_core::pipeline::{ProcessRequest, run_pipeline};
use photos_core::storage::{NewTask, Store};

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
}

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
    }))
    .into_response()
}

/// GET /models：模型注册表与校验状态
pub async fn list_models(State(state): State<Arc<AppState>>) -> Response {
    let store = state.store.lock().unwrap();
    let statuses = match check_models(&state.cfg, &store) {
        Ok(s) => s,
        Err(e) => return ApiError::from(e).into_response(),
    };
    Json(json!({
        "items": statuses.iter().map(|s| json!({
            "id": s.id,
            "path": s.path.display().to_string(),
            "ready": matches!(s.check_status, CheckStatus::Ready | CheckStatus::CachedOk),
            "check_status": s.check_status.code(),
            "message": s.message,
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// POST `/models/download` 请求体：`ids` 缺省（或空数组）表示下载全部「缺失」模型
#[derive(Debug, Default, Deserialize)]
pub struct DownloadParams {
    #[serde(default)]
    pub ids: Option<Vec<String>>,
}

/// POST /models/download：一键下载模型到注册表路径。
/// 未指定 `ids` 时下载全部缺失（文件不存在）的模型；已有文件视为成功，单个失败不阻断其余。
/// 下载为阻塞 IO，放入 `spawn_blocking` 执行，避免占用异步运行时线程。
pub async fn download_models(
    State(state): State<Arc<AppState>>,
    body: Option<Json<DownloadParams>>,
) -> Response {
    let cfg = state.cfg.clone();
    let requested = body.and_then(|Json(p)| p.ids).unwrap_or_default();
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

    let items = match tokio::task::spawn_blocking(move || download_each(&cfg, ids)).await {
        Ok(v) => v,
        Err(e) => return ApiError::Internal(format!("下载线程异常：{e}")).into_response(),
    };
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
            if let Err(e) = store.update_task(task_id, "running", "开始处理", "[]", "[]", None)
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
        };
        let result = tokio::task::spawn_blocking(move || {
            let (w, h) = image::image_dimensions(&req.input).unwrap_or((640, 640));
            // 与流水线内部预缩放对齐：引擎按缩放后尺寸构造
            let (w, h) =
                photos_core::pipeline::limited_dimensions(w, h, state2.cfg.general.max_input_side);
            // 借出引擎（生产模式来自进程级池，复用已装载模型的引擎；用完自动归还）
            let mut lease = (state2.engine_factory)(w, h);
            run_pipeline(&state2.cfg, lease.engine_mut(), &req)
        })
        .await;

        let elapsed = started.elapsed().as_millis() as i64;
        let outcome = match result {
            Ok(Ok(r)) => {
                // 产物落盘（命名规约集中在 photos_core::output，与 CLI 一致）
                let layout_spec = layout.as_deref().and_then(|id| state.cfg.layout.get(id));
                match save_task_outputs(
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
                }
            }
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(format!("任务执行异常：{e}")),
        };

        let store = state.store.lock().unwrap();
        match outcome {
            Ok((outputs, warnings)) => {
                let outputs_json = serde_json::to_string(&outputs).unwrap_or_else(|_| "[]".into());
                let warnings_json =
                    serde_json::to_string(&warnings).unwrap_or_else(|_| "[]".into());
                if let Err(e) = store.update_task(
                    task_id,
                    "succeeded",
                    "处理完成",
                    &outputs_json,
                    &warnings_json,
                    Some(elapsed),
                ) {
                    tracing::error!("更新任务成功状态失败：{e}");
                }
            }
            Err(msg) => {
                if let Err(e) =
                    store.update_task(task_id, "failed", &msg, "[]", "[]", Some(elapsed))
                {
                    tracing::error!("更新任务失败状态出错：{e}");
                }
            }
        }
    });
}

/// GET /tasks：历史任务分页列表
#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PageQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = q.offset.unwrap_or(0).max(0);
    let store = state.store.lock().unwrap();
    let total = match store.count_tasks() {
        Ok(t) => t,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let rows = match store.list_tasks_paged(limit, offset) {
        Ok(r) => r,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|t| {
            let backgrounds: Vec<String> = t
                .backgrounds
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let outputs: Vec<String> = artifact::parse_outputs(&t.outputs)
                .into_iter()
                .map(|a| a.filename)
                .collect();
            json!({
                "id": public_task_id(t.id),
                "input_path": t.input_path,
                "mode": t.mode,
                "size": t.size,
                "backgrounds": backgrounds,
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

/// GET /tasks/{id}：轮询状态、告警、产物清单
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
    Json(json!({
        "id": public_task_id(record.id),
        "status": record.status,
        "message": if record.message.is_empty() { serde_json::Value::Null } else { json!(record.message) },
        "warnings": serde_json::from_str::<Vec<String>>(&record.warnings).unwrap_or_default(),
        "beauty": record.beauty,
        "dress": record.dress,
        "elapsed_ms": record.elapsed_ms,
        "created_at": record.created_at,
        "artifacts": artifacts,
    }))
    .into_response()
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
