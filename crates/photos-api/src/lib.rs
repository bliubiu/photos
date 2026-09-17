//! photos-api：Axum 路由层（M3 实现），挂载 photos-core。
//!
//! 契约：docs/05-API契约.md。服务只绑定回环地址（默认 127.0.0.1 随机端口），
//! 提供 7 个端点（POST /tasks、GET /tasks、GET /tasks/{id}、GET /tasks/{id}/output、
//! GET /models、GET /config、GET /ping），任务状态机 queued → running → succeeded | failed。

pub mod artifact;
pub mod error;
pub mod handlers;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tower_http::services::ServeDir;

use handlers::{AppState, EngineFactory};
use photos_core::config::Config;

/// 定位前端构建产物目录（同源静态托管；找不到返回 None 即不托管）
pub fn frontend_dir() -> Option<PathBuf> {
    ["crates/photos-desktop/frontend/dist", "frontend/dist"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.join("index.html").exists())
}

/// 组装应用路由（注入状态；测试可用自定义引擎工厂）
pub fn router(cfg: Config, engine_factory: EngineFactory, model_precheck: bool) -> Router {
    router_with_frontend(cfg, engine_factory, model_precheck, frontend_dir())
}

/// 组装应用路由，可选挂载前端静态资源（同源托管）
pub fn router_with_frontend(
    cfg: Config,
    engine_factory: EngineFactory,
    model_precheck: bool,
    frontend: Option<PathBuf>,
) -> Router {
    let state = match AppState::new(cfg, engine_factory, model_precheck) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!("初始化应用状态失败：{e}");
            panic!("初始化应用状态失败：{e}");
        }
    };
    let app = Router::new()
        .route("/ping", get(handlers::ping))
        .route("/config", get(handlers::get_config))
        .route("/models", get(handlers::list_models))
        .route(
            "/tasks",
            post(handlers::create_task).get(handlers::list_tasks),
        )
        .route("/tasks/{id}", get(handlers::get_task))
        .route("/tasks/{id}/output", get(handlers::task_output))
        .with_state(state);
    if let Some(dir) = frontend {
        app.fallback_service(ServeDir::new(dir).append_index_html_on_directories(true))
    } else {
        app
    }
}

/// 生产引擎工厂：仅返回真实 OrtEngine（feature=ort）。
/// **无 ort 时返回 Err**，禁止静默降级 demo（演示请显式使用 [`demo_engine_factory`]）。
pub fn production_engine_factory() -> anyhow::Result<EngineFactory> {
    if !photos_core::inference::ORT_BUILT {
        anyhow::bail!(
            "当前构建未启用 ONNX 推理（feature=photos-core/ort），无法以真实模式启动 serve/桌面版。\n\
             请使用：cargo run -p photos-cli --features photos-core/ort -- serve\n\
             或显式演示模式：photos serve --demo（输出为模拟数据，非真实证件照）"
        );
    }
    Ok(Arc::new(|_, _| photos_core::inference::default_engine()))
}

/// 演示引擎工厂：内置 mock 回放（椭圆人形），**仅限显式 `--demo` 或 PHOTOS_DEMO=1**。
pub fn demo_engine_factory() -> EngineFactory {
    Arc::new(|w, h| Box::new(photos_core::pipeline::demo_balanced_engine(w, h)))
}

/// 根据 `PHOTOS_DEMO` 环境变量选择工厂：`1`/`true`/`yes` → demo，否则要求 ort。
/// 桌面壳等无法传 `--demo` 的入口使用。
pub fn engine_factory_from_env() -> anyhow::Result<EngineFactory> {
    let demo = std::env::var("PHOTOS_DEMO")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if demo {
        tracing::warn!("PHOTOS_DEMO 已启用：使用演示引擎，输出非真实 AI 推理结果");
        eprintln!("警告：演示模式已启用（PHOTOS_DEMO），输出为模拟数据，非真实证件照");
        return Ok(demo_engine_factory());
    }
    production_engine_factory()
}

/// 绑定回环地址（端口 0 = 随机），返回监听器与地址（供 serve / 桌面壳使用）
pub async fn bind_local() -> anyhow::Result<(tokio::net::TcpListener, std::net::SocketAddr)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

/// 在给定监听器上运行服务（阻塞至关闭）
pub async fn run_server(app: Router, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    axum::serve(listener, app).await?;
    Ok(())
}

/// `photos serve` 入口（同步）：生产模式启动本地 HTTP 服务（需 ort），直至 Ctrl+C
pub fn serve(cfg: Config) -> anyhow::Result<()> {
    serve_with_factory(cfg, "127.0.0.1", 0, production_engine_factory()?)
}

/// 指定主机与端口启动生产服务（端口 0 = 随机）；**无 ort 直接失败，不静默 demo**
pub fn serve_with(cfg: Config, host: &str, port: u16) -> anyhow::Result<()> {
    serve_with_factory(cfg, host, port, production_engine_factory()?)
}

/// 显式演示模式：内置 mock 引擎（`--demo`）
pub fn serve_demo_with(cfg: Config, host: &str, port: u16) -> anyhow::Result<()> {
    eprintln!("警告：serve 处于演示模式（--demo），输出为模拟数据，非真实证件照");
    tracing::warn!("serve 演示模式：使用内置 mock 引擎");
    serve_with_factory(cfg, host, port, demo_engine_factory())
}

/// 用给定引擎工厂启动服务
pub fn serve_with_factory(
    cfg: Config,
    host: &str,
    port: u16,
    engine_factory: EngineFactory,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let app = router(cfg, engine_factory, true);
        let listener = tokio::net::TcpListener::bind((host, port)).await?;
        let addr = listener.local_addr()?;
        tracing::info!("证件照本地服务已启动：http://{addr}");
        println!("证件照本地服务已启动：http://{addr}（Ctrl+C 退出）");
        run_server(app, listener).await
    })
}
