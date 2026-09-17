//! photos-api：Axum 路由层（M3 实现），挂载 photos-core。
//!
//! 契约：docs/05-API契约.md。服务只绑定回环地址（默认 127.0.0.1 随机端口），
//! 提供 7 个端点（POST /tasks、GET /tasks、GET /tasks/{id}、GET /tasks/{id}/output、
//! GET /models、GET /config、GET /ping），任务状态机 queued → running → succeeded | failed。

pub mod artifact;
pub mod error;
pub mod handlers;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use handlers::{AppState, EngineFactory};
use photos_core::config::Config;

/// 组装应用路由（注入状态；测试可用自定义引擎工厂）
pub fn router(cfg: Config, engine_factory: EngineFactory, model_precheck: bool) -> Router {
    let state = match AppState::new(cfg, engine_factory, model_precheck) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!("初始化应用状态失败：{e}");
            panic!("初始化应用状态失败：{e}");
        }
    };
    Router::new()
        .route("/ping", get(handlers::ping))
        .route("/config", get(handlers::get_config))
        .route("/models", get(handlers::list_models))
        .route(
            "/tasks",
            post(handlers::create_task).get(handlers::list_tasks),
        )
        .route("/tasks/{id}", get(handlers::get_task))
        .route("/tasks/{id}/output", get(handlers::task_output))
        .with_state(state)
}

/// 默认引擎工厂：真实推理后端（feature=ort 时为 OrtEngine，否则 FakeEngine）
pub fn default_engine_factory() -> EngineFactory {
    Arc::new(|| photos_core::inference::default_engine())
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

/// `photos serve` 入口（同步）：启动本地 HTTP 服务并打印地址，直至 Ctrl+C
pub fn serve(cfg: Config) -> anyhow::Result<()> {
    serve_with(cfg, "127.0.0.1", 0)
}

/// 指定主机与端口启动服务（端口 0 = 随机）；打印实际监听地址后阻塞
pub fn serve_with(cfg: Config, host: &str, port: u16) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let app = router(cfg, default_engine_factory(), true);
        let listener = tokio::net::TcpListener::bind((host, port)).await?;
        let addr = listener.local_addr()?;
        tracing::info!("证件照本地服务已启动：http://{addr}");
        println!("证件照本地服务已启动：http://{addr}（Ctrl+C 退出）");
        run_server(app, listener).await
    })
}
