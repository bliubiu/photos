//! photos CLI：子命令 `process / serve / models`（serve 复用 photos-api，M3 实现）。

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// 智能证件照处理工具（纯本地离线）
#[derive(Debug, Parser)]
#[command(name = "photos", version, about, long_about = None)]
pub struct Cli {
    /// 配置文件路径（默认查找当前目录 application.toml）
    #[arg(long, global = true, value_name = "路径")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// 查看模型注册表状态
    Models {
        /// 以 JSON 输出
        #[arg(long)]
        json: bool,
    },
    /// 处理证件照（单图，M1 实现）
    Process(ProcessArgs),
    /// 启动本地 HTTP 服务（M3 实现）
    Serve,
    /// 启动桌面版（M3 实现）
    Gui,
}

/// 单图处理参数
#[derive(Debug, clap::Args)]
pub struct ProcessArgs {
    /// 输入图片路径
    #[arg(value_name = "输入图片")]
    pub input: PathBuf,
    /// 运行模式：speed | balanced | quality
    #[arg(short, long, value_name = "模式")]
    pub mode: Option<String>,
    /// 尺寸标准 id（默认 one_inch）
    #[arg(long, value_name = "尺寸")]
    pub size: Option<String>,
    /// 底色 id（默认 white）
    #[arg(long, value_name = "底色")]
    pub bg: Option<String>,
    /// 手动纠偏角度（度，上限 ±45）
    #[arg(long, value_name = "角度")]
    pub rotate: Option<f64>,
    /// 演示模式：不依赖模型，用内置 mock 回放跑通全链路出图
    #[arg(long)]
    pub demo: bool,
    /// 输出目录（默认 data/out）
    #[arg(short, long, value_name = "目录")]
    pub out: Option<PathBuf>,
}
