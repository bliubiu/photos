//! 7 个端点处理器（契约 docs/05-API契约.md §2）：
//! POST /tasks、GET /tasks、GET /tasks/{id}、GET /tasks/{id}/output、
//! GET /models、GET /config、GET /ping。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Multipart, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use photos_core::config::Config;
use photos_core::inference::InferenceEngine;
use photos_core::model::{CheckStatus, check_models};
use photos_core::pipeline::{ProcessRequest, run_pipeline};
use photos_core::storage::{NewTask, Store};

use crate::artifact;
use crate::error::{ApiError, model_missing};

/// 引擎工厂：构造推理引擎（真实 OrtEngine/FakeEngine；测试注入 stub/demo 引擎）
pub type EngineFactory = Arc<dyn Fn() -> Box<dyn InferenceEngine> + Send + Sync>;

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
}

impl AppState {
    /// 构建状态；打开 data_dir/photos.db，创建 out/tmp 目录
    pub fn new(cfg: Config, engine_factory: EngineFactory, model_precheck: bool) -> photos_core::error::CoreResult<Self> {
        let store = Store::open(Path::new(&cfg.general.data_dir).join("photos.db").as_path())?;
        let out_dir = PathBuf::from(&cfg.general.data_dir).join("out");
        let upload_dir = PathBuf::from(&cfg.general.data_dir).join("tmp");
        std::fs::create_dir_all(&out_dir)?;
        std::fs::create_dir_all(&upload_dir)?;
        Ok(Self {
            cfg: Arc::new(cfg),
            store: Mutex::new(store),
            engine_factory,
            out_dir,
            upload_dir,
            model_precheck,
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
    pub rotate: Option<f64>,
    pub layout: Option<String>,
    pub effect_image: Option<bool>,
}

/// 美颜参数（M4 实现算子；本阶段仅透传 enabled）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeautyParams {
    #[serde(default)]
    pub enabled: bool,
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
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
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

/// POST /tasks：multipart 提交（file + params JSON）→ 202 任务 id
pub async fn create_task(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Response {
    match create_task_inner(&state, &mut multipart).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn create_task_inner(state: &Arc<AppState>, multipart: &mut Multipart) -> Result<Response, ApiError> {
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
    let file_bytes = file_bytes.ok_or_else(|| ApiError::InvalidParams("缺少上传文件字段 file".into()))?;
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
    let mode = params.mode.clone().unwrap_or_else(|| cfg.general.default_mode.clone());
    cfg.mode(&mode).map_err(|e| ApiError::InvalidParams(e.to_string()))?;
    let size = params.size.clone().unwrap_or_else(|| "one_inch".into());
    cfg.size(&size).map_err(|e| ApiError::InvalidParams(e.to_string()))?;
    let bgs = params.backgrounds.clone().unwrap_or_else(|| vec!["white".into()]);
    if bgs.is_empty() {
        return Err(ApiError::InvalidParams("底色列表不能为空".into()));
    }
    for bg in &bgs {
        cfg.background(bg).map_err(|e| ApiError::InvalidParams(e.to_string()))?;
    }
    if let Some(layout) = &params.layout {
        if !cfg.layout.contains_key(layout) {
            return Err(ApiError::InvalidParams(format!("未知排版“{layout}”")));
        }
    }
    if let Some(r) = params.rotate {
        if !(-45.0..=45.0).contains(&r) {
            return Err(ApiError::InvalidParams(format!("手动纠偏角度需在 ±45° 内，收到 {r}°")));
        }
    }
    let effect = params.effect_image.unwrap_or(false);
    let beauty_enabled = params.beauty.as_ref().map(|b| b.enabled).unwrap_or(false);

    // 4. 模型预检（就绪才受理；缺失返回 503，不自动下载以免阻塞）
    if state.model_precheck {
        let store = state.store.lock().unwrap();
        let statuses = check_models(cfg, &store).map_err(ApiError::from)?;
        let suite = cfg.mode(&mode).map_err(|e| ApiError::InvalidParams(e.to_string()))?;
        let suite_ids = [suite.face.as_str(), suite.keypoint.as_str(), suite.matting.as_str()];
        if let Some(s) = statuses
            .iter()
            .find(|s| suite_ids.contains(&s.id.as_str()) && !matches!(s.check_status, CheckStatus::Ready | CheckStatus::CachedOk))
        {
            return Err(model_missing(&s.id, &s.message));
        }
    }

    // 5. 保存上传文件 + 落库 queued
    let upload_name = format!("{}_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0), safe_upload_name(&raw_name));
    let input_path = state.upload_dir.join(upload_name);
    std::fs::write(&input_path, &file_bytes).map_err(|e| ApiError::Internal(format!("保存上传文件失败：{e}")))?;

    let beauty_json = if beauty_enabled { "{\"enabled\":true}" } else { "{}" }.to_string();
    let (task_id, created_at) = {
        let store = state.store.lock().unwrap();
        let id = store
            .insert_task(&NewTask {
                input_path: input_path.display().to_string(),
                mode: mode.clone(),
                size: size.clone(),
                backgrounds: bgs.join(","),
                beauty: beauty_json,
                rotate: params.rotate,
                outputs: String::new(),
                status: "queued".into(),
                message: "已入队，等待处理".into(),
                warnings: String::new(),
                elapsed_ms: None,
            })
            .map_err(ApiError::from)?;
        let rec = store.get_task(id).map_err(ApiError::from)?.ok_or(ApiError::Internal("任务入库后查询失败".into()))?;
        (id, rec.created_at)
    };

    // 6. 后台异步处理（状态机 queued → running → succeeded | failed）
    spawn_task(state.clone(), task_id, TaskParams {
        mode: Some(mode),
        size: Some(size),
        backgrounds: Some(bgs),
        rotate: params.rotate,
        layout: params.layout,
        effect_image: Some(effect),
        beauty: params.beauty,
    }, input_path);

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
        {
            let store = state.store.lock().unwrap();
            if let Err(e) = store.update_task(task_id, "running", "开始处理", "[]", "[]", None) {
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
        let beauty = params.beauty.as_ref().map(|b| b.enabled).unwrap_or(false);

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
        };
        let result = tokio::task::spawn_blocking(move || {
            let mut engine = (state2.engine_factory)();
            run_pipeline(&state2.cfg, engine.as_mut(), &req)
        })
        .await;

        let elapsed = started.elapsed().as_millis() as i64;
        let outcome = match result {
            Ok(Ok(r)) => {
                // 产物落盘（命名规约与 CLI 一致）
                let mut outputs: Vec<String> = Vec::new();
                let mut save_err: Option<String> = None;
                std::fs::create_dir_all(&state.out_dir).ok();
                for photo in &r.photos {
                    let out_path = state.out_dir.join(format!("task_{task_id}_{size}_{}.jpg", photo.bg));
                    if let Err(e) = photo.image.save(&out_path) {
                        save_err = Some(format!("保存证件照失败：{e}"));
                        break;
                    }
                    outputs.push(out_path.display().to_string());
                }
                for eff in &r.effects {
                    let out_path = state.out_dir.join(format!("task_{task_id}_effect_{}.jpg", eff.bg));
                    if let Err(e) = eff.image.save(&out_path) {
                        save_err = Some(format!("保存效果图失败：{e}"));
                        break;
                    }
                    outputs.push(out_path.display().to_string());
                }
                if let Some(canvas) = &r.layout {
                    let layout_id = layout.as_deref().unwrap_or("layout");
                    let out_path = state.out_dir.join(format!("task_{task_id}_layout_{layout_id}.jpg"));
                    if let Err(e) = canvas.save(&out_path) {
                        save_err = Some(format!("保存排版失败：{e}"));
                    } else {
                        outputs.push(out_path.display().to_string());
                    }
                }
                if let Some(err) = save_err {
                    Err(err)
                } else {
                    Ok((outputs, r.warnings))
                }
            }
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(format!("任务执行异常：{e}")),
        };

        let store = state.store.lock().unwrap();
        match outcome {
            Ok((outputs, warnings)) => {
                let outputs_json = serde_json::to_string(&outputs).unwrap_or_else(|_| "[]".into());
                let warnings_json = serde_json::to_string(&warnings).unwrap_or_else(|_| "[]".into());
                if let Err(e) = store.update_task(task_id, "succeeded", "处理完成", &outputs_json, &warnings_json, Some(elapsed)) {
                    tracing::error!("更新任务成功状态失败：{e}");
                }
            }
            Err(msg) => {
                if let Err(e) = store.update_task(task_id, "failed", &msg, "[]", "[]", Some(elapsed)) {
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
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zip"));
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!("attachment; filename=\"{}_bundle.zip\"", public_task_id(id)))
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        );
        return (headers, bytes).into_response();
    }

    let picked = match artifact::pick(&artifacts, kind, q.background.as_deref(), q.layout.as_deref()) {
        Some(a) => a,
        None => {
            return ApiError::ArtifactNotFound(format!("产物不存在：artifact={kind}")).into_response();
        }
    };
    if !picked.path.exists() {
        return ApiError::ArtifactNotFound(format!("产物文件缺失：{}", picked.filename)).into_response();
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
