//! 契约级集成测试（docs/05-API契约.md）：7 端点 + 状态机 + 错误码。
//!
//! 用 demo 引擎工厂 + 临时 data_dir，避免依赖真实模型与污染仓库数据。

use std::sync::Arc;

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
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.general.data_dir = dir.path().join("data").display().to_string();
        Self { cfg, dir }
    }

    fn app(&self) -> axum::Router {
        let factory: EngineFactory =
            Arc::new(|_, _| Box::new(demo_balanced_engine(100, 140)) as Box<dyn photos_core::inference::InferenceEngine>);
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
    let req = Request::builder().uri("/config").body(Body::empty()).unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["default_mode"], "balanced");
    let modes = json["modes"].as_array().unwrap();
    assert!(modes.iter().any(|m| m["id"] == "balanced" && m["label"] == "CPU 高性能"));
    let sizes = json["sizes"].as_array().unwrap();
    let one = sizes.iter().find(|s| s["id"] == "one_inch").unwrap();
    assert_eq!(one["width_px"], 295);
    assert_eq!(one["height_px"], 413);
    let bgs = json["backgrounds"].as_array().unwrap();
    let white = bgs.iter().find(|b| b["id"] == "white").unwrap();
    assert_eq!(white["rgb"], json!([255, 255, 255]));
    let layouts = json["layouts"].as_array().unwrap();
    assert!(layouts.iter().any(|l| l["id"] == "6inch"));
}

#[tokio::test]
async fn 模型列表结构() {
    let t = TestApp::new();
    let app = t.app();
    let req = Request::builder().uri("/models").body(Body::empty()).unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert!(!items.is_empty());
    let rf = items.iter().find(|m| m["id"] == "retinaface").unwrap();
    assert!(rf["check_status"].is_string());
    assert!(rf["ready"].is_boolean());
    assert!(rf["message"].is_string());
}

#[tokio::test]
async fn 任务全链路成功() {
    let t = TestApp::new();
    let app = t.app();
    let (_id, detail) = create_and_wait(&app, &t.out_dir()).await;
    assert_eq!(detail["status"], "succeeded", "任务失败：{}", detail["message"]);
    let artifacts = detail["artifacts"].as_array().unwrap();
    // 两底色 → 2 个 id_photo 产物
    assert_eq!(artifacts.len(), 2);
    let white = artifacts.iter().find(|a| a["background"] == "white").unwrap();
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
    assert_eq!(detail["status"], "succeeded", "任务失败：{}", detail["message"]);
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
    assert_eq!(detail["status"], "succeeded", "任务失败：{}", detail["message"]);
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
    assert_eq!(detail["status"], "succeeded", "任务失败：{}", detail["message"]);
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
    assert_eq!(detail["status"], "succeeded", "任务失败：{}", detail["message"]);
    // 任务记录含分部位服装图参数
    assert!(detail["dress"].as_str().unwrap().contains("garments"));
}

#[tokio::test]
async fn 换装分部位集合全空返回400() {
    let t = TestApp::new();
    let app = t.app();
    let (body, ctype) = multipart_body(
        &demo_jpeg(),
        r#"{"dress":{"enabled":true,"garments":{}}}"#,
    );
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
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
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
    let req = Request::builder().uri("/tasks/task_999999").body(Body::empty()).unwrap();
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
    let req = Request::builder().uri("/tasks?limit=10&offset=0").body(Body::empty()).unwrap();
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
        .uri(format!("/tasks/{id}/output?artifact=id_photo&background=white"))
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
        .uri(format!("/tasks/{id}/output?artifact=id_photo&background=red"))
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
    let req = Request::builder().uri("/tasks?limit=2&offset=0").body(Body::empty()).unwrap();
    let (_, json) = send(&app, req).await;
    assert_eq!(json["total"], 3);
    assert_eq!(json["items"].as_array().unwrap().len(), 2);
    // limit 超上限被钳制为 100，不报错
    let req = Request::builder().uri("/tasks?limit=999").body(Body::empty()).unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
}
