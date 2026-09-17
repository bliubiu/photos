//! `photos serve`：启动本地 HTTP 服务（复用 photos-api），默认 127.0.0.1 随机端口。

use anyhow::Result;
use photos_core::config::Config;

use crate::cli::ServeArgs;

/// 启动服务（阻塞直至 Ctrl+C）
pub fn run(cfg: &Config, args: &ServeArgs) -> Result<()> {
    let host = args.host.clone().unwrap_or_else(|| "127.0.0.1".into());
    let port = args.port.unwrap_or(0);
    photos_api::serve_with(cfg.clone(), &host, port)?;
    Ok(())
}
