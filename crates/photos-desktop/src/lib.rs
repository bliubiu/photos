//! photos-desktop：Tauri 2 桌面壳（M3）。
//!
//! 启动即内嵌本地 Axum 服务（127.0.0.1 随机端口，同源托管 WebUI 与 API），
//! 再用 WebView 加载该地址，实现「桌面窗口 + 前端无感知调用核心流水线」。

use std::sync::Arc;

use photos_core::config::Config;

/// 应用入口（main.rs 调用）
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // 配置加载失败时降级为默认配置（桌面版不应因缺配置而无法打开）
            let cfg = Config::load().unwrap_or_else(|e| {
                tracing::warn!("配置加载失败，使用默认配置：{e}");
                Config::default()
            });
            let addr = spawn_server(cfg)?;
            tracing::info!("本地服务已启动：http://{addr}");
            println!("本地服务地址：http://{addr}");
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(
                    format!("http://{addr}")
                        .parse()
                        .expect("本地服务地址格式错误"),
                ),
            )
            .title("智能证件照处理工具")
            .inner_size(1280.0, 820.0)
            .min_inner_size(960.0, 640.0)
            .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("运行 Tauri 桌面应用失败");
}

/// 在后台线程启动本地 Axum 服务，返回访问地址（http://127.0.0.1:端口）
fn spawn_server(cfg: Config) -> Result<String, Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (listener, addr) = rt.block_on(photos_api::bind_local())?;
    // 前端产物优先取源码树内 dist（dev 运行），否则探测相对路径
    let manifest_dist = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/dist");
    let frontend = if manifest_dist.join("index.html").exists() {
        Some(manifest_dist)
    } else {
        photos_api::frontend_dir()
    };
    // 禁止静默 demo：默认要求 ort；PHOTOS_DEMO=1 时显式演示并告警
    let engine_factory = photos_api::engine_factory_from_env()?;
    let app = photos_api::router_with_frontend(cfg, engine_factory, true, frontend);
    let _handle: Arc<_> = Arc::new(rt.spawn(async move {
        if let Err(e) = photos_api::run_server(app, listener).await {
            tracing::error!("本地服务异常退出：{e}");
        }
    }));
    // runtime 随进程存活（桌面应用退出即回收），避免句柄被析构后服务停止
    std::mem::forget(rt);
    Ok(addr.to_string())
}
