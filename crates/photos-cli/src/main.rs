//! photos CLI 入口。

use std::path::Path;

use anyhow::Result;
use clap::Parser;
use photos_core::config::Config;
use photos_core::logging::init_logging;

mod cli;
mod commands;

use cli::{Cli, Commands};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = load_config(cli.config.as_deref())?;
    let _guard = init_logging(Path::new(&cfg.general.log_dir), cfg.general.log_level)?;

    match &cli.command {
        Commands::Models(args) => commands::models::run(&cfg, args)?,
        Commands::Process(args) => commands::process::run(&cfg, args)?,
        Commands::Serve(args) => commands::serve::run(&cfg, args)?,
        Commands::Gui(args) => commands::gui::run(&cfg, args)?,
    }
    Ok(())
}

/// 加载配置（优先 CLI 指定路径，否则默认查找当前目录 application.toml）
fn load_config(path: Option<&Path>) -> Result<Config> {
    let cfg = match path {
        Some(p) => Config::load_from(Some(p))?,
        None => Config::load()?,
    };
    Ok(cfg)
}
