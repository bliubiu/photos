//! `photos process`：处理证件照（M1 实现核心链路）。

use anyhow::Result;
use photos_core::config::Config;

use crate::cli::ProcessArgs;

/// 单图处理入口（M1 阶段实现）
pub fn run(cfg: &Config, args: &ProcessArgs) -> Result<()> {
    let _ = (cfg, args);
    anyhow::bail!("process 子命令将于 M1 阶段实现核心链路（当前为骨架占位）")
}
