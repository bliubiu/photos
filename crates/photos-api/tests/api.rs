//! 契约级集成测试（docs/05-API契约.md）：7 端点 + 状态机 + 错误码。
//!
//! 用 demo 引擎工厂 + 临时 data_dir，避免依赖真实模型与污染仓库数据。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use photos_api::handlers::EngineFactory;
use photos_api::router;
use photos_core::config::Config;
use photos_core::pipeline::demo_balanced_engine;
use serde_json::{Value, json};
use tower::ServiceExt;

/// 测试夹具：临时 data_dir + demo 引擎 + 关闭模型预检
struct TestApp {
    cfg: Config,
    dir: tempfile::TempDir,
}

impl TestApp {
    fn new() -> Self {
        Self::new_with(|_| {})
    }

    fn new_with(adjust: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.general.data_dir = dir.path().join("data").display().to_string();
        adjust(&mut cfg);
        Self { cfg, dir }
    }

    fn app(&self) -> axum::Router {
        let factory: EngineFactory = Arc::new(|_mode, w, h| {
            photos_api::engine_pool::EngineLease::owned(Box::new(demo_balanced_engine(w, h)))
        });
        router(self.cfg.clone(), factory, false)
    }

    fn out_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("data").join("out")
    }
}

/// 生成一张 100x140 的 jpeg 字节（作为上传图片）
fn demo_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(100, 140, image::Rgb([20, 30, 40]));
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut buf, image::ImageFormat::Jpeg)
        .unwrap();
    buf.into_inner()
}

/// 构造 multipart 请求体（file + params）
fn multipart_body(file: &[u8], params: &str) -> (Vec<u8>, String) {
    let boundary = "----photos-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"in.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n",
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"params\"\r\n\r\n");
    body.extend_from_slice(params.as_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

/// 发送请求并读取完整响应
async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

async fn create_and_wait(app: &axum::Router, out_dir: &std::path::Path) -> (String, Value) {
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white","blue"],"effect_image":false}"#,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "创建任务失败：{json}");
    let id = json["id"].as_str().unwrap().to_string();

    // 轮询直至终态
    let detail = loop {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (_, d) = send(app, req).await;
        let st = d["status"].as_str().unwrap();
        if st == "succeeded" || st == "failed" {
            break d;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    std::fs::create_dir_all(out_dir).unwrap();
    (id, detail)
}

#[tokio::test]
async fn ping健康检查() {
    let t = TestApp::new();
    let app = t.app();
    let req = Request::builder().uri("/ping").body(Body::empty()).unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn 配置驱动前端下拉() {
    let t = TestApp::new();
    let app = t.app();
    let req = Request::builder()
        .uri("/config")
        .body(Body::empty())
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["default_mode"], "balanced");
    let modes = json["modes"].as_array().unwrap();
    assert!(
        modes
            .iter()
            .any(|m| m["id"] == "balanced" && m["label"] == "CPU 高性能")
    );
    let sizes = json["sizes"].as_array().unwrap();
    let one = sizes.iter().find(|s| s["id"] == "one_inch").unwrap();
    assert_eq!(one["width_px"], 295);
    assert_eq!(one["height_px"], 413);
    let bgs = json["backgrounds"].as_array().unwrap();
    let white = bgs.iter().find(|b| b["id"] == "white").unwrap();
    assert_eq!(white["rgb"], json!([255, 255, 255]));
    let layouts = json["layouts"].as_array().unwrap();
    assert!(layouts.iter().any(|l| l["id"] == "6inch"));
    // 工作流步骤元数据（驱动前端步骤编排面板）
    let steps = json["pipeline"]["steps"].as_array().unwrap();
    assert!(
        steps.iter().any(|s| s["id"] == "background"
            && s["label"] == "换底裁切"
            && s["stage"] == "换底裁切")
    );
    let matting = steps.iter().find(|s| s["id"] == "matting").unwrap();
    assert_eq!(matting["requires"], json!(["read_image"]));
    assert_eq!(json["pipeline"]["effective"].as_array().unwrap().len(), 10);
}

#[tokio::test]
async fn 工作流步骤非法返回400() {
    let t = TestApp::new();
    let app = t.app();
    // 未知步骤
    let (body, ctype) = multipart_body(&demo_jpeg(), r#"{"steps":["read_image","不存在的步骤"]}"#);
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["message"].as_str().unwrap().contains("未知工作流步骤"));
    // 依赖缺失（几何纠偏需姿态求解）
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"steps":["read_image","rotate","background"]}"#,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["message"].as_str().unwrap().contains("依赖"));
}

#[tokio::test]
async fn 关闭可选步骤的任务成功出图() {
    let t = TestApp::new();
    let app = t.app();
    // 仅保留必产出图的最小步骤链（关闭换装/美颜/排版）
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"backgrounds":["white"],"steps":["read_image","keypoint","matting","face_detect","pose","rotate","background"]}"#,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = json["id"].as_str().unwrap().to_string();
    let mut detail = Value::Null;
    for _ in 0..100 {
        let req = Request::builder()
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (_, json) = send(&app, req).await;
        if json["status"] == "succeeded" || json["status"] == "failed" {
            detail = json;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    // 关闭排版后无排版阶段耗时记录
    let stages = detail["metrics"].as_array().unwrap();
    assert!(!stages.iter().any(|s| s["stage"] == "排版"));
    assert!(stages.iter().any(|s| s["stage"] == "换底裁切"));
}

#[tokio::test]
async fn 模型列表结构() {
    let mut t = TestApp::new();
    // 模型目录指向临时目录，避免读取仓库 models/ 造成结果不确定
    t.cfg.general.models_dir = t.dir.path().join("models").display().to_string();
    let app = t.app();
    let req = Request::builder()
        .uri("/models")
        .body(Body::empty())
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert!(!items.is_empty());
    let rf = items.iter().find(|m| m["id"] == "retinaface").unwrap();
    assert!(rf["check_status"].is_string());
    assert!(rf["ready"].is_boolean());
    assert!(rf["message"].is_string());
    // 插件化扩展字段：角色 / 版本 / 内置标记
    assert_eq!(rf["role"], "face");
    assert_eq!(rf["builtin"], true);
    assert!(rf["versions"].is_array());
    // 旧库无 prefs 记录：无激活版本、也无版本目录（回归）
    assert!(rf["active_version"].is_null());
    assert!(rf["version"].is_null());
}

#[tokio::test]
async fn 任务全链路成功() {
    let t = TestApp::new();
    let app = t.app();
    let (_id, detail) = create_and_wait(&app, &t.out_dir()).await;
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    let artifacts = detail["artifacts"].as_array().unwrap();
    // 两底色 → 2 个 id_photo 产物
    assert_eq!(artifacts.len(), 2);
    let white = artifacts
        .iter()
        .find(|a| a["background"] == "white")
        .unwrap();
    assert_eq!(white["kind"], "id_photo");
    assert!(white["filename"].as_str().unwrap().contains("white.jpg"));
    assert!(detail["elapsed_ms"].is_number());
}

#[tokio::test]
async fn 参数非法返回400() {
    let t = TestApp::new();
    let app = t.app();
    let (body, ctype) = multipart_body(&demo_jpeg(), r#"{"size":"不存在的尺寸"}"#);
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "INVALID_PARAMS");
    assert!(json["message"].as_str().unwrap().contains("未知尺寸"));
}

#[tokio::test]
async fn 美颜强度越界返回400() {
    let t = TestApp::new();
    let app = t.app();
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"beauty":{"enabled":true,"brighten":1.5}}"#,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "INVALID_PARAMS");
    assert!(json["message"].as_str().unwrap().contains("提亮强度"));
}

#[tokio::test]
async fn 美颜开启任务成功且记录参数() {
    let t = TestApp::new();
    let app = t.app();
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"beauty":{"enabled":true,"skin_smooth":0.8,"brighten":0.2}}"#,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = json["id"].as_str().unwrap();
    // 轮询直至完成
    let mut detail = json.clone();
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (s, j) = send(&app, req).await;
        assert_eq!(s, StatusCode::OK);
        detail = j;
        if detail["status"] != "queued" && detail["status"] != "running" {
            break;
        }
    }
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    // 任务记录含美颜参数（磨皮/提亮非缺省）
    assert!(detail["beauty"].as_str().unwrap().contains("0.8"));
}

#[tokio::test]
async fn 换装样式非法返回400() {
    let t = TestApp::new();
    let app = t.app();
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"dress":{"enabled":true,"style":"tuxedo"}}"#,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "INVALID_PARAMS");
    assert!(json["message"].as_str().unwrap().contains("未知正装样式"));
}

#[tokio::test]
async fn 换装开启任务成功且记录参数() {
    let t = TestApp::new();
    let app = t.app();
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"dress":{"enabled":true,"style":"suit_navy"}}"#,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = json["id"].as_str().unwrap();
    let mut detail = json.clone();
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (s, j) = send(&app, req).await;
        assert_eq!(s, StatusCode::OK);
        detail = j;
        if detail["status"] != "queued" && detail["status"] != "running" {
            break;
        }
    }
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    // 任务记录含换装参数（正装样式）
    assert!(detail["dress"].as_str().unwrap().contains("suit_navy"));
}

#[tokio::test]
async fn 全身套装样式任务成功且记录参数() {
    let t = TestApp::new();
    let app = t.app();
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"dress":{"enabled":true,"style":"suit_full_navy"}}"#,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = json["id"].as_str().unwrap();
    let mut detail = json.clone();
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (s, j) = send(&app, req).await;
        assert_eq!(s, StatusCode::OK);
        detail = j;
        if detail["status"] != "queued" && detail["status"] != "running" {
            break;
        }
    }
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    // 任务记录含全身套装换装参数
    assert!(detail["dress"].as_str().unwrap().contains("suit_full_navy"));
}

#[tokio::test]
async fn 多图分部位换装任务成功() {
    let t = TestApp::new();
    let app = t.app();
    // 服务端可读的分部位服装图：上衣红、下装蓝
    let dir = tempfile::tempdir().unwrap();
    let top_path = dir.path().join("top.jpg");
    let bottom_path = dir.path().join("bottom.jpg");
    image::RgbImage::from_pixel(120, 120, image::Rgb([200, 30, 30]))
        .save(&top_path)
        .unwrap();
    image::RgbImage::from_pixel(120, 120, image::Rgb([30, 30, 200]))
        .save(&bottom_path)
        .unwrap();
    let params = json!({
        "dress": {
            "enabled": true,
            "garments": {
                "top": top_path.to_string_lossy().to_string(),
                "bottom": bottom_path.to_string_lossy().to_string(),
            }
        }
    })
    .to_string();
    let (body, ctype) = multipart_body(&demo_jpeg(), &params);
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = json["id"].as_str().unwrap();
    let mut detail = json.clone();
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (s, j) = send(&app, req).await;
        assert_eq!(s, StatusCode::OK);
        detail = j;
        if detail["status"] != "queued" && detail["status"] != "running" {
            break;
        }
    }
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    // 任务记录含分部位服装图参数
    assert!(detail["dress"].as_str().unwrap().contains("garments"));
}

#[tokio::test]
async fn 换装分部位集合全空返回400() {
    let t = TestApp::new();
    let app = t.app();
    let (body, ctype) = multipart_body(&demo_jpeg(), r#"{"dress":{"enabled":true,"garments":{}}}"#);
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["message"].as_str().unwrap().contains("换装需提供"));
}

#[tokio::test]
async fn 不支持媒体返回415() {
    let t = TestApp::new();
    let app = t.app();
    let boundary = "----photos-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"evil.exe\"\r\nContent-Type: application/octet-stream\r\n\r\n",
    );
    body.extend_from_slice(b"MZ-bytes");
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(json["code"], "UNSUPPORTED_MEDIA");
}

#[tokio::test]
async fn 任务不存在返回404() {
    let t = TestApp::new();
    let app = t.app();
    let req = Request::builder()
        .uri("/tasks/task_999999")
        .body(Body::empty())
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["code"], "TASK_NOT_FOUND");
}

#[tokio::test]
async fn 历史任务列表与下载() {
    let t = TestApp::new();
    let app = t.app();
    let (id, _) = create_and_wait(&app, &t.out_dir()).await;

    // 列表
    let req = Request::builder()
        .uri("/tasks?limit=10&offset=0")
        .body(Body::empty())
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total"], 1);
    let item = &json["items"][0];
    assert_eq!(item["id"], id);
    assert_eq!(item["status"], "succeeded");
    assert_eq!(item["backgrounds"], json!(["white", "blue"]));
    assert!(item["outputs"].as_array().unwrap().len() == 2);
    assert!(item["outputs"][0].as_str().unwrap().ends_with(".jpg"));

    // 下载单个产物
    let req = Request::builder()
        .uri(format!(
            "/tasks/{id}/output?artifact=id_photo&background=white"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.len() > 100, "产物字节过少");
    // jpeg 魔数
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);

    // bundle zip
    let req = Request::builder()
        .uri(format!("/tasks/{id}/output?artifact=bundle"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..2], b"PK");

    // 不存在的产物 → 404
    let req = Request::builder()
        .uri(format!(
            "/tasks/{id}/output?artifact=id_photo&background=red"
        ))
        .body(Body::empty())
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["code"], "ARTIFACT_NOT_FOUND");
}

#[tokio::test]
async fn 分页查询() {
    let t = TestApp::new();
    let app = t.app();
    for _ in 0..3 {
        let (_, _) = create_and_wait(&app, &t.out_dir()).await;
    }
    let req = Request::builder()
        .uri("/tasks?limit=2&offset=0")
        .body(Body::empty())
        .unwrap();
    let (_, json) = send(&app, req).await;
    assert_eq!(json["total"], 3);
    assert_eq!(json["items"].as_array().unwrap().len(), 2);
    // limit 超上限被钳制为 100，不报错
    let req = Request::builder()
        .uri("/tasks?limit=999")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
}

#[test]
fn 并发上限取自配置() {
    let t = TestApp::new_with(|cfg| cfg.server.max_concurrent_tasks = 3);
    let factory: EngineFactory = Arc::new(|_mode, w, h| {
        photos_api::engine_pool::EngineLease::owned(Box::new(demo_balanced_engine(w, h)))
    });
    let state = photos_api::handlers::AppState::new(t.cfg.clone(), factory, false, None)
        .unwrap();
    assert_eq!(state.slots.available_permits(), 3);
}

#[tokio::test]
async fn 引擎池复用连续任务() {
    let t = TestApp::new();
    let built = Arc::new(AtomicUsize::new(0));
    let counter = built.clone();
    // 池容量 1：连续任务应复用同一引擎（模型只装载一次）
    let pool = photos_api::engine_pool::EnginePool::new(
        Arc::new(move |w, h| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::new(demo_balanced_engine(w, h)) as Box<dyn photos_core::inference::InferenceEngine>
        }),
        1,
    );
    let acquired = pool.clone();
    let factory: EngineFactory = Arc::new(move |mode, w, h| acquired.acquire(&mode, w, h));
    let app = router(t.cfg.clone(), factory, false);

    for _ in 0..2 {
        let (_, detail) = create_and_wait(&app, &t.out_dir()).await;
        assert_eq!(
            detail["status"], "succeeded",
            "任务失败：{}",
            detail["message"]
        );
    }
    assert_eq!(built.load(Ordering::SeqCst), 1, "两个任务应复用同一引擎");
    assert_eq!(pool.created(), 1);
}

#[tokio::test]
async fn 输入图超限时按最大边长预缩放() {
    let t = TestApp::new_with(|cfg| cfg.general.max_input_side = 80);
    let app = t.app();
    let (body, ctype) = multipart_body(&demo_jpeg(), r#"{"effect_image":true}"#);
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "创建任务失败：{json}");
    let id = json["id"].as_str().unwrap().to_string();

    let detail = loop {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (_, d) = send(&app, req).await;
        let st = d["status"].as_str().unwrap();
        if st == "succeeded" || st == "failed" {
            break d;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    // 100x140 长边缩到 80 → 57x80（与流水线内部预缩放一致）
    let effect = detail["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["filename"].as_str().unwrap().contains("effect"))
        .expect("应有效果图产物");
    let path = t.out_dir().join(effect["filename"].as_str().unwrap());
    assert_eq!(image::image_dimensions(path).unwrap(), (57, 80));
}

#[tokio::test]
async fn 透明底与自定义背景图产物() {
    let t = TestApp::new();
    let app = t.app();
    // 自定义背景图（纯红）
    let bg_path = t.dir.path().join("bg.png");
    image::RgbImage::from_pixel(200, 200, image::Rgb([200, 30, 30]))
        .save(&bg_path)
        .unwrap();
    let params = serde_json::json!({
        "mode": "balanced",
        "size": "one_inch",
        "backgrounds": ["white"],
        "transparent": true,
        "bg_image": bg_path.display().to_string(),
    })
    .to_string();
    let (body, ctype) = multipart_body(&demo_jpeg(), &params);
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "创建任务失败：{json}");
    let id = json["id"].as_str().unwrap().to_string();

    let detail = loop {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (_, d) = send(&app, req).await;
        let st = d["status"].as_str().unwrap();
        if st == "succeeded" || st == "failed" {
            break d;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );

    let artifacts = detail["artifacts"].as_array().unwrap();
    // 纯色底色 + 自定义背景（custombg）各一张证件照
    assert_eq!(artifacts.len(), 3, "实际产物：{artifacts:?}");
    assert!(artifacts.iter().any(|a| a["background"] == "custombg"));
    assert!(artifacts.iter().any(|a| a["background"] == "transparent"));

    // 透明底产物可下载且为 PNG
    let req = Request::builder()
        .uri(format!(
            "/tasks/{id}/output?artifact=id_photo&background=transparent"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    // PNG 魔数
    assert_eq!(&bytes[..4], &[0x89, 0x50, 0x4E, 0x47]);
}

#[tokio::test]
async fn 自定义尺寸与自定义底色出图() {
    let t = TestApp::new();
    let app = t.app();
    // 自定义像素尺寸 + 自定义十六进制底色（均归一化为文件名安全 id）
    let params = serde_json::json!({
        "mode": "balanced",
        "size": "px:200x280",
        "backgrounds": ["#ff0000"],
    })
    .to_string();
    let (body, ctype) = multipart_body(&demo_jpeg(), &params);
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "创建任务失败：{json}");
    let id = json["id"].as_str().unwrap().to_string();

    let detail = loop {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (_, d) = send(&app, req).await;
        let st = d["status"].as_str().unwrap();
        if st == "succeeded" || st == "failed" {
            break d;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    // 落库与产物命名使用归一化 id
    let artifacts = detail["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 1, "实际产物：{artifacts:?}");
    let filename = artifacts[0]["filename"].as_str().unwrap();
    assert!(
        filename.contains("px_200x280") && filename.contains("rgb-ff0000"),
        "产物命名应含归一化标识，实际：{filename}"
    );
    assert_eq!(artifacts[0]["background"], "rgb-ff0000");

    // 自定义尺寸生效（证件照像素等于自定义宽高）
    let path = t.out_dir().join(filename);
    assert_eq!(image::image_dimensions(path).unwrap(), (200, 280));
}

#[tokio::test]
async fn 自定义尺寸与底色非法取值返回400() {
    let t = TestApp::new();
    let app = t.app();
    for params in [
        r#"{"size":"px:0x280"}"#,
        r#"{"size":"mm:35x45@10"}"#,
        r##"{"backgrounds":["#ff00"]}"##,
        r#"{"backgrounds":["rgb:256,0,0"]}"#,
    ] {
        let (body, ctype) = multipart_body(&demo_jpeg(), params);
        let req = Request::builder()
            .method("POST")
            .uri("/tasks")
            .header(header::CONTENT_TYPE, ctype)
            .body(Body::from(body))
            .unwrap();
        let (status, json) = send(&app, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "参数 {params} 应被拒绝");
        assert_eq!(json["code"], "INVALID_PARAMS");
    }
}

/// POST /models/download（JSON body）
fn download_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/models/download")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn 模型下载_未知id返回400() {
    let t = TestApp::new();
    let app = t.app();
    let (status, json) = send(&app, download_request(r#"{"ids":["不存在的模型"]}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "INVALID_PARAMS");
    assert!(json["message"].as_str().unwrap().contains("未知模型"));
}

#[tokio::test]
async fn 模型下载_文件已存在直接成功() {
    let t = TestApp::new();
    // 全部模型路径指向同一已存在文件：默认（缺失集合）应为空，无需联网
    let fake = t.dir.path().join("fake.onnx");
    std::fs::write(&fake, b"onnx").unwrap();
    let mut cfg = t.cfg.clone();
    for spec in cfg.models.values_mut() {
        spec.path = fake.display().to_string();
        spec.download = None;
    }
    let app = router(cfg, test_factory(), false);

    let (status, json) = send(&app, download_request("{}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        json["items"].as_array().unwrap().is_empty(),
        "无缺失模型时应不下任何下载：{json}"
    );

    let (status, json) = send(&app, download_request(r#"{"ids":["retinaface"]}"#)).await;
    assert_eq!(status, StatusCode::OK);
    let item = &json["items"][0];
    assert_eq!(item["id"], "retinaface");
    assert_eq!(item["ok"], true);
    assert!(item["message"].as_str().unwrap().contains("已存在"));
}

#[tokio::test]
async fn 模型下载_缺失且无下载地址返回失败项() {
    let t = TestApp::new();
    // 目标文件不存在且未配置下载地址：单个失败不阻断，逐项返回中文原因
    let missing = t.dir.path().join("not-exist.onnx");
    let mut cfg = t.cfg.clone();
    let spec = cfg.models.get_mut("retinaface").unwrap();
    spec.path = missing.display().to_string();
    spec.download = None;
    let app = router(cfg, test_factory(), false);

    let (status, json) = send(&app, download_request(r#"{"ids":["retinaface"]}"#)).await;
    assert_eq!(status, StatusCode::OK);
    let item = &json["items"][0];
    assert_eq!(item["ok"], false);
    assert!(item["message"].as_str().unwrap().contains("未配置下载地址"));
}

/// 以自定义 params 创建任务并轮询至终态
async fn create_custom_and_wait(app: &axum::Router, params: &str) -> (String, Value) {
    let (body, ctype) = multipart_body(&demo_jpeg(), params);
    let req = Request::builder()
        .method("POST")
        .uri("/tasks")
        .header(header::CONTENT_TYPE, ctype)
        .body(Body::from(body))
        .unwrap();
    let (status, json) = send(app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "创建任务失败：{json}");
    let id = json["id"].as_str().unwrap().to_string();

    let detail = loop {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/tasks/{id}"))
            .body(Body::empty())
            .unwrap();
        let (_, d) = send(app, req).await;
        let st = d["status"].as_str().unwrap();
        if st == "succeeded" || st == "failed" {
            break d;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    (id, detail)
}

/// 产物文件中指定后缀的体积合计
fn outputs_size(detail: &Value, dir: &std::path::Path, suffix: &str) -> u64 {
    detail["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["filename"].as_str().unwrap().ends_with(suffix))
        .map(|a| {
            std::fs::metadata(dir.join(a["filename"].as_str().unwrap()))
                .unwrap()
                .len()
        })
        .sum()
}

#[tokio::test]
async fn 输出格式可切换为webp() {
    let t = TestApp::new();
    let app = t.app();
    let (_, detail) = create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"output_format":"webp"}"#,
    )
    .await;
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    let artifacts = detail["artifacts"].as_array().unwrap();
    let webp = artifacts
        .iter()
        .find(|a| a["filename"].as_str().unwrap().ends_with(".webp"))
        .expect("应有 webp 产物");
    let path = t.out_dir().join(webp["filename"].as_str().unwrap());
    let bytes = std::fs::read(path).unwrap();
    // RIFF....WEBP
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WEBP");
}

#[tokio::test]
async fn jpg质量参数影响产物体积() {
    let t = TestApp::new();
    let app = t.app();
    let (_, hi) = create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"jpg_quality":95}"#,
    )
    .await;
    let (_, lo) = create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"jpg_quality":20}"#,
    )
    .await;
    let hi_bytes = outputs_size(&hi, &t.out_dir(), ".jpg");
    let lo_bytes = outputs_size(&lo, &t.out_dir(), ".jpg");
    assert!(hi_bytes > 0 && lo_bytes > 0, "应各有 jpg 产物");
    assert!(
        lo_bytes < hi_bytes,
        "低质量产物应更小：{lo_bytes} vs {hi_bytes}"
    );
}

#[tokio::test]
async fn 排版可额外输出pdf() {
    let t = TestApp::new();
    let app = t.app();
    let (_, detail) = create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"layout":"6inch","pdf":true}"#,
    )
    .await;
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    let artifacts = detail["artifacts"].as_array().unwrap();
    // 排版图片与 PDF 同时输出
    assert!(artifacts.iter().any(|a| {
        let f = a["filename"].as_str().unwrap();
        f.contains("_layout_") && f.ends_with(".jpg")
    }));
    let pdf = artifacts
        .iter()
        .find(|a| a["filename"].as_str().unwrap().ends_with(".pdf"))
        .expect("应有 PDF 产物");
    let bytes = std::fs::read(t.out_dir().join(pdf["filename"].as_str().unwrap())).unwrap();
    assert_eq!(&bytes[..8], b"%PDF-1.4");
}

#[tokio::test]
async fn 输出参数非法返回400() {
    let t = TestApp::new();
    let app = t.app();
    for params in [
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"output_format":"tiff"}"#,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"jpg_quality":0}"#,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"jpg_quality":101}"#,
    ] {
        let (body, ctype) = multipart_body(&demo_jpeg(), params);
        let req = Request::builder()
            .method("POST")
            .uri("/tasks")
            .header(header::CONTENT_TYPE, ctype)
            .body(Body::from(body))
            .unwrap();
        let (status, json) = send(&app, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "应拒绝：{params} → {json}");
    }
}

/// GET 请求
fn get_request(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

/// DELETE 请求
fn delete_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

/// 查询列表并返回（total, 本页条数）
async fn list_total(app: &axum::Router, uri: &str) -> (i64, usize) {
    let (status, json) = send(app, get_request(uri)).await;
    assert_eq!(status, StatusCode::OK, "列表查询失败：{json}");
    (
        json["total"].as_i64().unwrap(),
        json["items"].as_array().unwrap().len(),
    )
}

#[tokio::test]
async fn 历史任务筛选与非法条件() {
    let t = TestApp::new();
    let app = t.app();
    create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"]}"#,
    )
    .await;
    create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"two_inch","backgrounds":["blue"]}"#,
    )
    .await;

    // 无筛选 → 全部
    assert_eq!(list_total(&app, "/tasks").await, (2, 2));
    // 状态 / 模式 / 尺寸 / 底色
    assert_eq!(list_total(&app, "/tasks?status=succeeded").await, (2, 2));
    assert_eq!(list_total(&app, "/tasks?status=failed").await, (0, 0));
    assert_eq!(list_total(&app, "/tasks?mode=balanced").await, (2, 2));
    assert_eq!(list_total(&app, "/tasks?mode=quality").await, (0, 0));
    assert_eq!(list_total(&app, "/tasks?size=one_inch").await, (1, 1));
    assert_eq!(list_total(&app, "/tasks?size=two_inch").await, (1, 1));
    assert_eq!(list_total(&app, "/tasks?background=white").await, (1, 1));
    assert_eq!(list_total(&app, "/tasks?background=blue").await, (1, 1));
    // 组合条件与自定义底色（归一化后无命中）
    assert_eq!(
        list_total(&app, "/tasks?size=one_inch&background=white").await,
        (1, 1)
    );
    assert_eq!(
        list_total(&app, "/tasks?background=%23ff0000").await,
        (0, 0)
    );
    // 起始时间（文本比较）
    assert_eq!(list_total(&app, "/tasks?since=2020-01-01").await, (2, 2));
    assert_eq!(list_total(&app, "/tasks?since=2999-01-01").await, (0, 0));
    // 筛选后仍可分页
    assert_eq!(
        list_total(&app, "/tasks?size=one_inch&limit=1&offset=1").await,
        (1, 0)
    );

    // 非法筛选条件 → 400 INVALID_PARAMS
    for uri in [
        "/tasks?status=unknown",
        "/tasks?mode=unknown",
        "/tasks?size=unknown",
        "/tasks?background=%23ff00",
    ] {
        let (status, json) = send(&app, get_request(uri)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} 应被拒绝");
        assert_eq!(json["code"], "INVALID_PARAMS");
    }
}

#[tokio::test]
async fn 删除任务连带清理磁盘产物() {
    let t = TestApp::new();
    let app = t.app();
    let (id, detail) = create_and_wait(&app, &t.out_dir()).await;
    let files: Vec<std::path::PathBuf> = detail["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| t.out_dir().join(a["filename"].as_str().unwrap()))
        .collect();
    assert!(files.iter().all(|p| p.exists()), "产物应先落盘");
    // 上传原图路径来自列表
    let (_, list) = send(&app, get_request("/tasks")).await;
    let input = std::path::PathBuf::from(list["items"][0]["input_path"].as_str().unwrap());
    assert!(input.exists(), "上传原图应留存");

    let (status, json) = send(&app, delete_request(&format!("/tasks/{id}"))).await;
    assert_eq!(status, StatusCode::OK, "删除失败：{json}");
    assert_eq!(json["id"], id);
    // 2 个产物 + 1 张原图
    assert_eq!(json["deleted_outputs"].as_i64().unwrap(), 3);
    assert!(files.iter().all(|p| !p.exists()), "产物文件应被删除");
    assert!(!input.exists(), "上传原图应被删除");

    // 记录已删除：查询与重复删除均 404
    let (status, json) = send(&app, get_request(&format!("/tasks/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["code"], "TASK_NOT_FOUND");
    let (status, _) = send(&app, delete_request(&format!("/tasks/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn 清空历史连带清理磁盘产物() {
    let t = TestApp::new();
    let app = t.app();
    let (_, d1) = create_and_wait(&app, &t.out_dir()).await;
    let (_, d2) = create_and_wait(&app, &t.out_dir()).await;

    let (status, json) = send(&app, delete_request("/tasks")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["deleted"], 2);
    assert_eq!(list_total(&app, "/tasks").await, (0, 0));
    for d in [d1, d2] {
        for a in d["artifacts"].as_array().unwrap() {
            let p = t.out_dir().join(a["filename"].as_str().unwrap());
            assert!(!p.exists(), "产物应被清理：{}", p.display());
        }
    }
}

#[tokio::test]
async fn 任务详情含提交参数与原图可访问() {
    let t = TestApp::new();
    let app = t.app();
    let (id, detail) = create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"rotate":3.5,"effect_image":true,"output_format":"webp"}"#,
    )
    .await;
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    // 详情补全回显字段
    assert_eq!(detail["mode"], "balanced");
    assert_eq!(detail["size"], "one_inch");
    assert_eq!(detail["backgrounds"], json!(["white"]));
    assert_eq!(detail["rotate"], 3.5);
    // 提交参数快照（供前端「复用参数」）
    let params = &detail["params"];
    assert_eq!(params["output_format"], "webp");
    assert_eq!(params["effect_image"], true);
    assert_eq!(params["transparent"], false);

    // 上传原图可访问（jpeg 魔数）
    let res = app
        .clone()
        .oneshot(get_request(&format!("/tasks/{id}/input")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/jpeg"
    );
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);

    // 不存在的任务 → 404
    let (status, json) = send(&app, get_request("/tasks/task_999999/input")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["code"], "TASK_NOT_FOUND");
}

#[tokio::test]
async fn 任务详情含分阶段耗时指标() {
    let t = TestApp::new();
    let app = t.app();
    let (_, detail) = create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"]}"#,
    )
    .await;
    assert_eq!(
        detail["status"], "succeeded",
        "任务失败：{}",
        detail["message"]
    );
    let stages = detail["metrics"].as_array().unwrap();
    let names: Vec<&str> = stages
        .iter()
        .map(|s| s["stage"].as_str().unwrap())
        .collect();
    for expect in [
        "读图",
        "人体关键点",
        "人像抠图",
        "人脸检测",
        "姿态求解",
        "几何纠偏",
        "换底裁切",
    ] {
        assert!(names.contains(&expect), "缺少阶段 {expect}：{names:?}");
    }
    assert!(stages.iter().all(|s| s["ms"].as_f64().unwrap() >= 0.0));
}

#[tokio::test]
async fn 指标聚合与错误上报端点() {
    let t = TestApp::new();
    let app = t.app();
    // 成功任务计入指标聚合
    let (_, ok_detail) = create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"]}"#,
    )
    .await;
    assert_eq!(ok_detail["status"], "succeeded");
    // 背景图不存在 → 任务失败并上报错误
    let (_, failed) = create_custom_and_wait(
        &app,
        r#"{"mode":"balanced","size":"one_inch","backgrounds":["white"],"bg_image":"no-such-bg.png"}"#,
    )
    .await;
    assert_eq!(failed["status"], "failed");
    assert!(
        failed["message"].as_str().unwrap().contains("背景图"),
        "实际：{}",
        failed["message"]
    );

    // GET /metrics：任务统计 + 平均耗时 + 各阶段平均耗时 + 错误总数
    let (status, metrics) = send(&app, get_request("/metrics")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(metrics["tasks"]["succeeded"], 1);
    assert_eq!(metrics["tasks"]["failed"], 1);
    assert!(metrics["elapsed_ms"]["avg"].as_f64().unwrap() >= 0.0);
    assert_eq!(metrics["elapsed_ms"]["samples"], 1);
    let agg = metrics["stages"].as_array().unwrap();
    assert!(
        agg.iter()
            .any(|s| s["stage"] == "人脸检测" && s["samples"] == 1),
        "实际：{agg:?}"
    );
    assert_eq!(metrics["errors"]["total"], 1);
    // 无池场景（demo）：engine_pool 为 null，前端据此隐藏引擎池面板
    assert!(metrics["engine_pool"].is_null());

    // GET /errors：失败任务的结构化错误记录
    let (status, errors) = send(&app, get_request("/errors")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(errors["total"], 1);
    let item = &errors["items"][0];
    assert_eq!(item["code"], "INTERNAL");
    // 失败阶段取最后一个已完成阶段（读取背景图失败发生在「换底裁切」结束前）
    assert_eq!(item["stage"], "几何纠偏");
    assert!(item["message"].as_str().unwrap().contains("背景图"));
    assert_eq!(item["task_id"], failed["id"]);

    // limit 越界钳制（0 → 默认 20，不报错）
    let (status, _) = send(&app, get_request("/errors?limit=0")).await;
    assert_eq!(status, StatusCode::OK);
}

/// 测试用引擎工厂（demo 回放，不入池）
fn test_factory() -> EngineFactory {
    Arc::new(|_mode, w, h| {
        photos_api::engine_pool::EngineLease::owned(Box::new(demo_balanced_engine(w, h)))
    })
}

// ---------- 插件化模型（市场 / 注册 / 多版本） ----------

/// POST 请求（JSON body）
fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn 模型市场清单默认禁用可展示() {
    let mut t = TestApp::new();
    t.cfg.general.models_dir = t.dir.path().join("models").display().to_string();
    let app = t.app();
    let (status, json) = send(&app, get_request("/models/market")).await;
    assert_eq!(status, StatusCode::OK, "获取市场清单失败：{json}");
    let items = json["items"].as_array().unwrap();
    assert!(!items.is_empty(), "内置清单不应为空");
    // 内置条目 url/sha256 为占位：可展示但禁用下载
    for it in items {
        assert_eq!(it["enabled"], false, "内置条目默认不应启用：{it}");
        assert_eq!(it["downloadable"], false, "占位条目不应可下载：{it}");
        assert!(it["version"].is_string());
        assert!(it["role"].is_string());
    }
    let rmbg = items
        .iter()
        .find(|m| m["id"] == "rmbg")
        .expect("应有 rmbg 条目");
    assert_eq!(rmbg["role"], "matting");
    assert_eq!(rmbg["registered"], true);
    assert_eq!(rmbg["downloaded"], false);

    // 占位条目不可下载：指定 version 但不带 ids 直接 400，带 ids 则逐项返回中文失败原因
    let (status, json) = send(
        &app,
        post_json("/models/download", r#"{"version":"1.4.0"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "INVALID_PARAMS");
    let (status, json) = send(
        &app,
        post_json("/models/download", r#"{"ids":["rmbg"],"version":"1.4.0"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["items"][0]["ok"], false);
    assert!(
        json["items"][0]["message"]
            .as_str()
            .unwrap()
            .contains("未启用下载"),
        "实际：{}",
        json["items"][0]["message"]
    );
}

#[tokio::test]
async fn 注册自定义模型写入注册表并可被配置加载() {
    let t = TestApp::new();
    let app = t.app();
    let ok_body = json!({
        "id": "my_matting",
        "path": "models/my_matting.onnx",
        "input_dims": [1, 3, 512, 512],
        "sha256": "1".repeat(64),
        "role": "matting",
        "preprocess": {
            "layout": "nchw",
            "norm": { "mean_std": { "mean": [0.5, 0.5, 0.5], "std": [0.5, 0.5, 0.5] } },
            "channel": "rgb"
        }
    })
    .to_string();

    let (status, json) = send(&app, post_json("/models/register", &ok_body)).await;
    assert_eq!(status, StatusCode::OK, "注册失败：{json}");
    assert_eq!(json["registered"], true);
    assert_eq!(json["replaced"], false);
    assert_eq!(json["restart_required"], true);
    // 重复注册同一 id → 覆盖
    let (status, json) = send(&app, post_json("/models/register", &ok_body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["replaced"], true);

    // 落盘到 <data_dir>/models.custom.toml，并可在重启后由 Config::load_from 读到
    let registry = t.dir.path().join("data").join("models.custom.toml");
    assert!(registry.exists(), "注册表应落盘：{}", registry.display());
    let app_toml = t.dir.path().join("application.toml");
    std::fs::write(
        &app_toml,
        format!(
            "[general]\ndata_dir = \"{}\"\n",
            t.dir
                .path()
                .join("data")
                .display()
                .to_string()
                .replace('\\', "/")
        ),
    )
    .unwrap();
    let reloaded = Config::load_from(Some(&app_toml)).unwrap();
    let spec = reloaded
        .models
        .get("my_matting")
        .expect("重启后应加载到注册的模型");
    assert_eq!(spec.role.as_str(), "matting");
    assert_eq!(spec.input_dims, vec![1, 3, 512, 512]);

    // 非法声明：角色非法 / sha256 非法 / 缺 sha256 且文件不存在 → 400 MODEL_REGISTER_INVALID
    for bad in [
        json!({ "id": "bad1", "path": "models/bad1.onnx", "input_dims": [1,3,512,512], "role": "banana", "sha256": "1".repeat(64) }).to_string(),
        json!({ "id": "bad2", "path": "models/bad2.onnx", "input_dims": [1,3,512,512], "sha256": "not-a-hash" }).to_string(),
        json!({ "id": "bad3", "path": "models/not-exist.onnx", "input_dims": [1,3,512,512] }).to_string(),
        // 布局与维度矛盾（声明 nchw 但第二维不是 3）
        json!({ "id": "bad4", "path": "models/bad4.onnx", "input_dims": [1,512,512,3], "sha256": "1".repeat(64), "preprocess": { "layout": "nchw" } }).to_string(),
        // id 非法（含路径分隔符）
        json!({ "id": "../evil", "path": "models/evil.onnx", "input_dims": [1,3,512,512], "sha256": "1".repeat(64) }).to_string(),
    ] {
        let (status, json) = send(&app, post_json("/models/register", &bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "应被拒绝：{bad} → {json}");
        assert_eq!(json["code"], "MODEL_REGISTER_INVALID");
    }
    // 非法请求体（缺必填字段）同样返回中文 400
    let (status, json) = send(&app, post_json("/models/register", r#"{"id":"x"}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "MODEL_REGISTER_INVALID");
    assert!(json["message"].as_str().unwrap().contains("注册请求体非法"));
}

#[tokio::test]
async fn 模型版本查询切换与回滚() {
    let mut t = TestApp::new();
    // 版本目录落在临时 models_dir，避免污染仓库
    let models_dir = t.dir.path().join("models");
    t.cfg.general.models_dir = models_dir.display().to_string();
    let app = t.app();

    // 旧库无 prefs 记录：版本列表为空、无激活版本（回归）
    let (status, json) = send(&app, get_request("/models/versions?id=rmbg")).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json["versions"].as_array().unwrap().is_empty());
    assert!(json["active_version"].is_null());

    // 未知 id / 未知版本 → 400 MODEL_VERSION_UNKNOWN
    let (status, json) = send(&app, get_request("/models/versions?id=nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "MODEL_VERSION_UNKNOWN");
    let (status, json) = send(
        &app,
        post_json("/models/activate", r#"{"id":"rmbg","version":"9.9.9"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "MODEL_VERSION_UNKNOWN");

    // 放入两个已下载版本
    for (v, content) in [
        ("1.0.0", b"onnx-a".as_slice()),
        ("1.1.0", b"onnx-b".as_slice()),
    ] {
        let dir = models_dir.join("rmbg").join(v);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rmbg-1.4.onnx"), content).unwrap();
    }
    let (_, json) = send(&app, get_request("/models/versions?id=rmbg")).await;
    assert_eq!(json["versions"], json!(["1.0.0", "1.1.0"]));
    // 注册表 sha256 为占位时不校验内容，可直接激活
    let (status, json) = send(
        &app,
        post_json("/models/activate", r#"{"id":"rmbg","version":"1.0.0"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "激活失败：{json}");
    assert_eq!(json["active_version"], "1.0.0");
    let (_, json) = send(&app, get_request("/models/versions?id=rmbg")).await;
    assert_eq!(json["active_version"], "1.0.0");

    // GET /models 反映激活版本与版本目录
    let (_, list) = send(&app, get_request("/models")).await;
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "rmbg")
        .unwrap();
    assert_eq!(item["active_version"], "1.0.0");
    assert_eq!(item["version"], "1.0.0");
    assert_eq!(item["builtin"], true);

    // 回滚到另一版本
    let (status, json) = send(
        &app,
        post_json("/models/activate", r#"{"id":"rmbg","version":"1.1.0"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["active_version"], "1.1.0");

    // 注册表声明真实 sha256 后，内容不符的版本拒绝激活（MODEL_MISSING）
    let mut cfg = t.cfg.clone();
    cfg.models.get_mut("rmbg").unwrap().sha256 = "a".repeat(64);
    let app = router(cfg, test_factory(), false);
    let (status, json) = send(
        &app,
        post_json("/models/activate", r#"{"id":"rmbg","version":"1.0.0"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{json}");
    assert_eq!(json["code"], "MODEL_MISSING");
}

#[tokio::test]
async fn 引擎池指标并入metrics() {
    let t = TestApp::new();
    // 池容量 2：先借出一个引擎保持占用，再验证 /metrics 的 engine_pool 指标
    let pool = photos_api::engine_pool::EnginePool::new(
        Arc::new(|w, h| {
            Box::new(demo_balanced_engine(w, h)) as Box<dyn photos_core::inference::InferenceEngine>
        }),
        2,
    );
    let acquired = pool.clone();
    let factory: EngineFactory = Arc::new(move |mode, w, h| acquired.acquire(&mode, w, h));
    let app = photos_api::router_with_frontend(t.cfg.clone(), factory, false, Some(pool.clone()), None);

    // 未借出：created 空、容量 2、无等待
    let (status, metrics) = send(&app, get_request("/metrics")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(metrics["engine_pool"]["capacity"], 2);
    assert_eq!(metrics["engine_pool"]["created"], 0);
    assert_eq!(metrics["engine_pool"]["waiting"], 0);
    let idle = metrics["engine_pool"]["idle_by_mode"].as_array().unwrap();
    assert!(idle.is_empty(), "无空闲引擎时列表应为空：{idle:?}");

    // 借出一个 balanced 引擎：created 增 1、空闲桶空
    {
        let _lease = pool.acquire("balanced", 10, 10);
        let (_, metrics) = send(&app, get_request("/metrics")).await;
        assert_eq!(metrics["engine_pool"]["created"], 1);
        assert!(metrics["engine_pool"]["idle_by_mode"].as_array().unwrap().is_empty());
    }

    // 归还后：空闲桶出现 balanced=1
    let (_, metrics) = send(&app, get_request("/metrics")).await;
    assert_eq!(metrics["engine_pool"]["created"], 1);
    assert_eq!(metrics["engine_pool"]["idle_by_mode"][0]["mode"], "balanced");
    assert_eq!(metrics["engine_pool"]["idle_by_mode"][0]["idle"], 1);
}
