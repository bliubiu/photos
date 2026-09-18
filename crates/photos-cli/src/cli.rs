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
    /// 查看模型注册表状态 / 一键下载模型
    Models(ModelsArgs),
    /// 处理证件照（单图或文件夹批量，一次出齐多底色/效果图/排版）
    Process(ProcessArgs),
    /// 启动本地 HTTP 服务（WebUI，M3）
    Serve(ServeArgs),
    /// 启动桌面版（Tauri 2，内嵌服务 + WebView）
    Gui(GuiArgs),
}

/// `photos gui` 参数
#[derive(Debug, clap::Args)]
pub struct GuiArgs {}

/// `photos serve` 参数
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// 监听地址（默认 127.0.0.1，仅本地回环）
    #[arg(long, value_name = "地址")]
    pub host: Option<String>,
    /// 监听端口（默认 0 = 系统随机端口）
    #[arg(long, value_name = "端口")]
    pub port: Option<u16>,
    /// 演示模式：内置 mock 引擎（非真实 AI；需显式指定，禁止静默降级）
    #[arg(long)]
    pub demo: bool,
}

/// `photos models` 参数
#[derive(Debug, clap::Args)]
pub struct ModelsArgs {
    /// 以 JSON 输出模型状态
    #[arg(long)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<ModelsCommand>,
}

/// `photos models` 子命令
#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// 一键下载模型（<id> 指定模型 id，all 下载全部已启用模型）
    Download {
        /// 模型 id 或 all
        #[arg(value_name = "模型id")]
        target: String,
    },
}

/// 单图 / 文件夹批量处理参数
#[derive(Debug, clap::Args)]
pub struct ProcessArgs {
    /// 输入图片路径或文件夹（文件夹递归处理其中图片）
    #[arg(value_name = "输入图片/文件夹")]
    pub input: PathBuf,
    /// 运行模式：speed | balanced | quality
    #[arg(short, long, value_name = "模式")]
    pub mode: Option<String>,
    /// 尺寸标准 id（默认 one_inch）
    #[arg(long, value_name = "尺寸")]
    pub size: Option<String>,
    /// 底色 id 列表，逗号分隔（默认 white；如 red,blue,white）
    #[arg(short = 'b', long, value_name = "底色")]
    pub backgrounds: Option<String>,
    /// 手动纠偏角度（度，上限 ±45）
    #[arg(long, value_name = "角度")]
    pub rotate: Option<f64>,
    /// 输出通用效果图（每底色各一张，保持全图尺寸）
    #[arg(long)]
    pub effect: bool,
    /// 排版相纸 id（6inch | a4，以首个底色证件照铺版）
    #[arg(long, value_name = "相纸")]
    pub layout: Option<String>,
    /// 美颜开关（磨皮/提亮/美白，M4 实现）
    #[arg(long)]
    pub beauty: bool,
    /// 磨皮强度 0..1（默认取配置 [beauty].skin_smooth）
    #[arg(long, value_name = "强度")]
    pub beauty_smooth: Option<f64>,
    /// 提亮强度 0..1（默认取配置 [beauty].brighten）
    #[arg(long, value_name = "强度")]
    pub beauty_brighten: Option<f64>,
    /// 美白强度 0..1（默认取配置 [beauty].whiten）
    #[arg(long, value_name = "强度")]
    pub beauty_whiten: Option<f64>,
    /// 演示模式：不依赖模型，用内置 mock 回放跑通全链路出图
    #[arg(long)]
    pub demo: bool,
    /// 用户服装图路径（智能换装：人像解析 + 服装贴合）
    #[arg(long, value_name = "服装图")]
    pub dress: Option<PathBuf>,
    /// 程序化正装样式（suit_navy | suit_black | shirt_white 上半身；suit_full_navy | suit_full_black 全身套装；未提供 --dress 时生效）
    #[arg(long, value_name = "样式")]
    pub dress_style: Option<String>,
    /// 分部位服装图：上衣（多图分部位贴合，优先于 --dress/--dress-style）
    #[arg(long, value_name = "上衣图")]
    pub dress_top: Option<PathBuf>,
    /// 分部位服装图：下装（多图分部位贴合）
    #[arg(long, value_name = "下装图")]
    pub dress_bottom: Option<PathBuf>,
    /// 分部位服装图：鞋（多图分部位贴合）
    #[arg(long, value_name = "鞋图")]
    pub dress_shoes: Option<PathBuf>,
    /// 额外输出透明底 PNG（RGBA，alpha 取抠图掩膜）
    #[arg(long)]
    pub transparent: bool,
    /// 自定义背景图路径（按证件照尺寸 cover 缩放裁切后合成，额外出 custombg 产物）
    #[arg(long, value_name = "背景图")]
    pub bg_image: Option<PathBuf>,
    /// 图片输出格式：jpg | webp（默认取配置 [output].format）
    #[arg(long, value_name = "格式")]
    pub format: Option<String>,
    /// JPG 压缩质量 1..=100（默认取配置 [output].jpg_quality；WebP 为无损编码不受此项影响）
    #[arg(long, value_name = "质量")]
    pub quality: Option<u8>,
    /// 排版相纸额外输出 PDF（页面按相纸物理尺寸设定，便于打印店直接使用）
    #[arg(long)]
    pub pdf: bool,
    /// 输出目录（默认 data/out）
    #[arg(short, long, value_name = "目录")]
    pub out: Option<PathBuf>,
}
