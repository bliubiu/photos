//! 统一配置：`application.toml` 解析与合并。
//!
//! 参数优先级：命令行 > 环境变量（`PHOTOS_` 前缀）> toml 配置文件 > 默认值。
//! 唯一权威样例：`docs/examples/application.toml`（解析测试以其为夹具）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::error::{CoreError, CoreResult};
use crate::vision::mtcnn::{CASCADE_FACE_ID, cascade_model_ids};

/// 全局配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// 全局段（数据目录、日志目录、级别、默认模式、模型目录）
    #[serde(default)]
    pub general: GeneralConfig,
    /// 模型注册表 `[models.<id>]`（id 与 docs/04-模型清单.md §3 一致）
    #[serde(default)]
    pub models: BTreeMap<String, ModelSpec>,
    /// 模型下载配置（提取自 `[models.download]`）
    #[serde(default, rename = "models_download")]
    pub models_download: DownloadConfig,
    /// 模式套件 `[modes.speed|balanced|quality]`
    #[serde(default)]
    pub modes: BTreeMap<String, ModeSuite>,
    /// 尺寸集 `[sizes.<id>]`
    #[serde(default)]
    pub sizes: BTreeMap<String, SizeSpec>,
    /// 底色集 `[backgrounds.<id>]`
    #[serde(default)]
    pub backgrounds: BTreeMap<String, BackgroundSpec>,
    /// 排版规格 `[layout.<id>]`
    #[serde(default)]
    pub layout: BTreeMap<String, LayoutSpec>,
    /// 美颜参数 `[beauty]`
    #[serde(default)]
    pub beauty: BeautyConfig,
    /// 推理资源参数 `[inference]`
    #[serde(default)]
    pub inference: InferenceConfig,
    /// 服务参数 `[server]`
    #[serde(default)]
    pub server: ServerConfig,
}

/// 全局段
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// 数据目录（sqlite 与输出产物）
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// 日志目录（日轮转文件）
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
    /// 日志级别
    #[serde(default)]
    pub log_level: LogLevel,
    /// 默认模式（speed | balanced | quality）
    #[serde(default = "default_mode")]
    pub default_mode: String,
    /// 模型根目录
    #[serde(default = "default_models_dir")]
    pub models_dir: String,
    /// 输入图最大边长（px，0 = 不限制；超限时等比预缩放，降低峰值内存与耗时）
    #[serde(default)]
    pub max_input_side: u32,
}

/// 日志级别（DEBUG | INFO | ERROR）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Error,
}

impl LogLevel {
    /// 转 tracing 级别
    pub fn as_tracing(self) -> tracing::Level {
        match self {
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Error => tracing::Level::ERROR,
        }
    }

    /// 文本表示
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Error => "ERROR",
        }
    }
}

/// 模型注册表条目 `[models.<id>]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// onnx 文件路径（相对项目根或绝对路径）
    pub path: String,
    /// 64 位小写十六进制 sha256；定版前为占位全 0
    pub sha256: String,
    /// 输入张量约定 `[N,C,H,W]` 或 NHWC
    pub input_dims: Vec<i64>,
    /// 是否参与校验与预设解析（预留）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 单模型下载地址（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<ModelDownload>,
}

/// 单模型下载配置
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelDownload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// 下载配置 `[models.download]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadConfig {
    /// 一键下载为可选便利，处理链路本身零联网
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 单次下载超时（秒）
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// 并发下载数
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// 镜像基地址（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// 模式套件 `[modes.<suite>]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeSuite {
    /// 人脸检测模型 id
    pub face: String,
    /// 人体关键点模型 id
    pub keypoint: String,
    /// 人像分割模型 id
    pub matting: String,
    /// 执行提供方（cpu | cuda）
    #[serde(default)]
    pub execution_provider: ExecutionProvider,
}

/// 推理执行提供方
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionProvider {
    #[default]
    Cpu,
    Cuda,
}

/// 尺寸标准 `[sizes.<id>]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SizeSpec {
    pub name: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub dpi: u32,
    pub width_px: u32,
    pub height_px: u32,
}

/// 底色 `[backgrounds.<id>]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundSpec {
    pub name: String,
    pub rgb: [u8; 3],
}

impl BackgroundSpec {
    /// RGB 元组
    pub fn rgb_tuple(&self) -> (u8, u8, u8) {
        (self.rgb[0], self.rgb[1], self.rgb[2])
    }
}

/// 排版规格 `[layout.<id>]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSpec {
    pub name: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub margin_mm: f64,
    pub gap_mm: f64,
}

/// 美颜参数 `[beauty]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeautyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_skin_smooth")]
    pub skin_smooth: f64,
    #[serde(default = "default_brighten")]
    pub brighten: f64,
    #[serde(default = "default_whiten")]
    pub whiten: f64,
}

/// 推理资源参数 `[inference]`：约束 ONNX Runtime 的线程与内存行为
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceConfig {
    /// 单算子内并行线程数（0 = 由 ONNX Runtime 自动决定，通常吃满物理核）
    #[serde(default)]
    pub intra_threads: usize,
    /// 算子间并行线程数（0 = 自动）
    #[serde(default)]
    pub inter_threads: usize,
    /// 是否启用内存复用池（关闭可降低峰值内存，代价是性能略降）
    #[serde(default = "default_true")]
    pub memory_pattern: bool,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            intra_threads: 0,
            inter_threads: 0,
            memory_pattern: default_true(),
        }
    }
}

/// 服务参数 `[server]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 最大并发处理任务数（超出后排队；必须 ≥1）
    #[serde(default = "default_max_concurrent_tasks")]
    pub max_concurrent_tasks: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_tasks: default_max_concurrent_tasks(),
        }
    }
}

impl Config {
    /// 默认加载：优先读取当前目录 `application.toml`，再叠加环境变量
    pub fn load() -> CoreResult<Self> {
        Self::load_from(default_config_path().as_deref())
    }

    /// 从指定配置文件加载（文件不存在则仅用默认值 + 环境变量）
    pub fn load_from(path: Option<&Path>) -> CoreResult<Self> {
        let mut root = toml::Value::try_from(Self::default())
            .map_err(|e| CoreError::ConfigParse(e.to_string()))?;

        if let Some(p) = path {
            if p.exists() {
                let content = std::fs::read_to_string(p)?;
                let file_value: Value = toml::from_str(&content)
                    .map_err(|e| CoreError::ConfigParse(format!("{}：{e}", p.display())))?;
                merge_value(&mut root, file_value);
            }
        }

        // 提取 [models.download] 到独立字段（避免与注册表 id 冲突）
        extract_models_download(&mut root);

        // 环境变量覆盖（PHOTOS_ 前缀）
        inject_env(&mut root);

        let cfg: Config = root
            .try_into()
            .map_err(|e| CoreError::ConfigMerge(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 校验关键引用与取值
    pub fn validate(&self) -> CoreResult<()> {
        if self.modes.is_empty() {
            return Err(CoreError::ConfigValidate(
                "模式套件（[modes]）不能为空".into(),
            ));
        }
        if !self.modes.contains_key(&self.general.default_mode) {
            return Err(CoreError::ConfigValidate(format!(
                "默认模式“{}”未在 [modes] 中定义（可选：{}）",
                self.general.default_mode,
                self.modes.keys().cloned().collect::<Vec<_>>().join("、")
            )));
        }
        if self.sizes.is_empty() {
            return Err(CoreError::ConfigValidate(
                "尺寸集（[sizes]）不能为空".into(),
            ));
        }
        if self.backgrounds.is_empty() {
            return Err(CoreError::ConfigValidate(
                "底色集（[backgrounds]）不能为空".into(),
            ));
        }
        if self.server.max_concurrent_tasks == 0 {
            return Err(CoreError::ConfigValidate(
                "[server] max_concurrent_tasks 必须 ≥ 1（设为 0 会导致所有任务永久排队）".into(),
            ));
        }
        for (suite_id, suite) in &self.modes {
            for (role, model_id) in [
                ("人脸检测 face", &suite.face),
                ("关键点 keypoint", &suite.keypoint),
                ("抠图 matting", &suite.matting),
            ] {
                // speed 套件 face 逻辑 id 为级联（MTCNN 三级联），需校验全部子模型已注册
                if *model_id == CASCADE_FACE_ID {
                    for sub in cascade_model_ids() {
                        if !self.models.contains_key(sub) {
                            return Err(CoreError::ConfigValidate(format!(
                                "模式“{suite_id}”的{role}引用的级联“{model_id}”缺少子模型“{sub}”"
                            )));
                        }
                    }
                    continue;
                }
                if !self.models.contains_key(model_id) {
                    return Err(CoreError::ConfigValidate(format!(
                        "模式“{suite_id}”的{role}引用了未注册模型“{model_id}”"
                    )));
                }
            }
        }
        Ok(())
    }

    /// 取模式套件
    pub fn mode(&self, id: &str) -> CoreResult<&ModeSuite> {
        self.modes.get(id).ok_or_else(|| {
            CoreError::ConfigValidate(format!(
                "未知模式“{id}”，可选：{}",
                self.modes.keys().cloned().collect::<Vec<_>>().join("、")
            ))
        })
    }

    /// 取模型注册表条目
    pub fn model_spec(&self, id: &str) -> CoreResult<&ModelSpec> {
        self.models.get(id).ok_or_else(|| {
            CoreError::ConfigValidate(format!(
                "未知模型“{id}”，可选：{}",
                self.models.keys().cloned().collect::<Vec<_>>().join("、")
            ))
        })
    }

    /// 取尺寸标准
    pub fn size(&self, id: &str) -> CoreResult<&SizeSpec> {
        self.sizes.get(id).ok_or_else(|| {
            CoreError::ConfigValidate(format!(
                "未知尺寸“{id}”，可选：{}",
                self.sizes.keys().cloned().collect::<Vec<_>>().join("、")
            ))
        })
    }

    /// 取底色
    pub fn background(&self, id: &str) -> CoreResult<&BackgroundSpec> {
        self.backgrounds.get(id).ok_or_else(|| {
            CoreError::ConfigValidate(format!(
                "未知底色“{id}”，可选：{}",
                self.backgrounds
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("、")
            ))
        })
    }
}

/// 默认配置（与 docs/examples/application.toml 保持一致）
impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            models: default_models(),
            models_download: DownloadConfig::default(),
            modes: default_modes(),
            sizes: default_sizes(),
            backgrounds: default_backgrounds(),
            layout: default_layout(),
            beauty: BeautyConfig::default(),
            inference: InferenceConfig::default(),
            server: ServerConfig::default(),
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            log_dir: default_log_dir(),
            log_level: LogLevel::Info,
            default_mode: default_mode(),
            models_dir: default_models_dir(),
            max_input_side: 0,
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: default_timeout_secs(),
            concurrency: default_concurrency(),
            base_url: None,
        }
    }
}

impl Default for BeautyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            skin_smooth: default_skin_smooth(),
            brighten: default_brighten(),
            whiten: default_whiten(),
        }
    }
}

fn default_data_dir() -> String {
    "data".into()
}
fn default_log_dir() -> String {
    "logs".into()
}
fn default_mode() -> String {
    "balanced".into()
}
fn default_models_dir() -> String {
    "models".into()
}
fn default_true() -> bool {
    true
}
fn default_timeout_secs() -> u64 {
    120
}
fn default_concurrency() -> usize {
    2
}
fn default_skin_smooth() -> f64 {
    0.3
}
fn default_brighten() -> f64 {
    0.2
}
fn default_whiten() -> f64 {
    0.1
}
fn default_max_concurrent_tasks() -> usize {
    2
}

fn default_models() -> BTreeMap<String, ModelSpec> {
    let mut m = BTreeMap::new();
    for (id, path, dims) in [
        // MTCNN 完整三级联（逻辑 speed 套件 face 仍叫 mtcnn）
        ("mtcnn_pnet", "models/pnet.onnx", vec![1, 3, 12, 12]),
        ("mtcnn_rnet", "models/rnet.onnx", vec![1, 3, 24, 24]),
        ("mtcnn_onet", "models/onet.onnx", vec![1, 3, 48, 48]),
        (
            "retinaface",
            "models/retinaface_r50.onnx",
            vec![1, 3, 640, 640],
        ),
        (
            "movnet_light",
            "models/movenet_lightning.onnx",
            vec![1, 192, 192, 3],
        ),
        (
            "movnet_thunder",
            "models/movenet_thunder.onnx",
            vec![1, 256, 256, 3],
        ),
        ("rmbg", "models/rmbg-1.4.onnx", vec![1, 3, 1024, 1024]),
        (
            "birefnet_lite",
            "models/birefnet-lite.onnx",
            vec![1, 3, 1024, 1024],
        ),
        (
            "birefnet_full",
            "models/birefnet-full.onnx",
            vec![1, 3, 1024, 1024],
        ),
        ("modnet", "models/modnet.onnx", vec![1, 3, 512, 512]),
        // 人像解析（LIP 20 类语义分割）：虚拟试衣换装用，独立于三模式套件
        (
            "parsing_lip",
            "models/parsing_lip.onnx",
            vec![1, 3, 473, 473],
        ),
    ] {
        m.insert(
            id.to_string(),
            ModelSpec {
                path: path.to_string(),
                // 定版前为占位全 0，禁止当作已就绪
                sha256: "0".repeat(64),
                input_dims: dims,
                enabled: true,
                download: default_download_url(id),
            },
        );
    }
    m
}

/// 默认注册表下载地址（一键下载开箱即用；优先 GitHub，HF 走 hf-mirror 以适配国内网络）
fn default_download_url(id: &str) -> Option<ModelDownload> {
    let url = match id {
        "retinaface" => {
            "https://github.com/Zeyi-Lin/HivisionIDPhotos/releases/download/pretrained-model/retinaface-resnet50.onnx"
        }
        "movnet_light" => {
            "https://hf-mirror.com/Xenova/movenet-singlepose-lightning/resolve/main/onnx/model.onnx"
        }
        "movnet_thunder" => {
            "https://hf-mirror.com/Xenova/movenet-singlepose-thunder/resolve/main/onnx/model.onnx"
        }
        "birefnet_lite" => {
            "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx"
        }
        "birefnet_full" => {
            "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-epoch_244.onnx"
        }
        "rmbg" => "https://hf-mirror.com/briaai/RMBG-1.4/resolve/main/onnx/model.onnx",
        "modnet" => {
            "https://github.com/Zeyi-Lin/HivisionIDPhotos/releases/download/pretrained-model/modnet_photographic_portrait_matting.onnx"
        }
        "parsing_lip" => {
            "https://hf-mirror.com/levihsu/OOTDiffusion/resolve/main/checkpoints/humanparsing/parsing_lip.onnx"
        }
        "mtcnn_pnet" => {
            "https://raw.githubusercontent.com/linxiaohui/mtcnn-opencv/main/mtcnn_cv2/pnet.onnx"
        }
        "mtcnn_rnet" => {
            "https://raw.githubusercontent.com/linxiaohui/mtcnn-opencv/main/mtcnn_cv2/rnet.onnx"
        }
        "mtcnn_onet" => {
            "https://raw.githubusercontent.com/linxiaohui/mtcnn-opencv/main/mtcnn_cv2/onet.onnx"
        }
        _ => return None,
    };
    Some(ModelDownload {
        url: Some(url.into()),
    })
}

fn default_modes() -> BTreeMap<String, ModeSuite> {
    let mut m = BTreeMap::new();
    m.insert(
        "speed".into(),
        ModeSuite {
            face: "mtcnn".into(),
            keypoint: "movnet_light".into(),
            matting: "rmbg".into(),
            execution_provider: ExecutionProvider::Cpu,
        },
    );
    m.insert(
        "balanced".into(),
        ModeSuite {
            face: "retinaface".into(),
            keypoint: "movnet_light".into(),
            matting: "birefnet_lite".into(),
            execution_provider: ExecutionProvider::Cpu,
        },
    );
    m.insert(
        "quality".into(),
        ModeSuite {
            face: "retinaface".into(),
            keypoint: "movnet_thunder".into(),
            matting: "birefnet_full".into(),
            execution_provider: ExecutionProvider::Cuda,
        },
    );
    m
}

fn default_sizes() -> BTreeMap<String, SizeSpec> {
    let mut m = BTreeMap::new();
    m.insert(
        "one_inch".into(),
        SizeSpec {
            name: "一寸".into(),
            width_mm: 25.0,
            height_mm: 35.0,
            dpi: 300,
            width_px: 295,
            height_px: 413,
        },
    );
    m.insert(
        "two_inch".into(),
        SizeSpec {
            name: "二寸".into(),
            width_mm: 35.0,
            height_mm: 49.0,
            dpi: 300,
            width_px: 413,
            height_px: 579,
        },
    );
    m.insert(
        "small_one_inch".into(),
        SizeSpec {
            name: "小一寸".into(),
            width_mm: 22.0,
            height_mm: 32.0,
            dpi: 300,
            width_px: 260,
            height_px: 378,
        },
    );
    // 加宽规格：大一寸（33×48）、小二寸（35×45）
    m.insert(
        "big_one_inch".into(),
        SizeSpec {
            name: "大一寸".into(),
            width_mm: 33.0,
            height_mm: 48.0,
            dpi: 300,
            width_px: 390,
            height_px: 567,
        },
    );
    m.insert(
        "small_two_inch".into(),
        SizeSpec {
            name: "小二寸".into(),
            width_mm: 35.0,
            height_mm: 45.0,
            dpi: 300,
            width_px: 413,
            height_px: 531,
        },
    );
    // 签证规格
    m.insert(
        "us_visa".into(),
        SizeSpec {
            name: "美国签证".into(),
            width_mm: 50.8,
            height_mm: 50.8,
            dpi: 300,
            width_px: 600,
            height_px: 600,
        },
    );
    m.insert(
        "japan_visa".into(),
        SizeSpec {
            name: "日本签证".into(),
            width_mm: 45.0,
            height_mm: 45.0,
            dpi: 300,
            width_px: 531,
            height_px: 531,
        },
    );
    m.insert(
        "schengen_visa".into(),
        SizeSpec {
            name: "申根签证".into(),
            width_mm: 35.0,
            height_mm: 45.0,
            dpi: 300,
            width_px: 413,
            height_px: 531,
        },
    );
    m.insert(
        "uk_visa".into(),
        SizeSpec {
            name: "英国签证".into(),
            width_mm: 35.0,
            height_mm: 45.0,
            dpi: 300,
            width_px: 413,
            height_px: 531,
        },
    );
    // 国内证件规格
    m.insert(
        "passport".into(),
        SizeSpec {
            name: "护照".into(),
            width_mm: 33.0,
            height_mm: 48.0,
            dpi: 300,
            width_px: 390,
            height_px: 567,
        },
    );
    m.insert(
        "hkmo_permit".into(),
        SizeSpec {
            name: "港澳通行证".into(),
            width_mm: 33.0,
            height_mm: 48.0,
            dpi: 300,
            width_px: 390,
            height_px: 567,
        },
    );
    m.insert(
        "driver_license".into(),
        SizeSpec {
            name: "驾驶证".into(),
            width_mm: 22.0,
            height_mm: 32.0,
            dpi: 300,
            width_px: 260,
            height_px: 378,
        },
    );
    m.insert(
        "social_security_card".into(),
        SizeSpec {
            name: "社保卡".into(),
            width_mm: 26.0,
            height_mm: 32.0,
            dpi: 300,
            width_px: 307,
            height_px: 378,
        },
    );
    m.insert(
        "residence_permit".into(),
        SizeSpec {
            name: "居住证".into(),
            width_mm: 26.0,
            height_mm: 32.0,
            dpi: 300,
            width_px: 307,
            height_px: 378,
        },
    );
    // 考试报名规格
    m.insert(
        "exam_registration".into(),
        SizeSpec {
            name: "考试报名".into(),
            width_mm: 35.0,
            height_mm: 45.0,
            dpi: 300,
            width_px: 413,
            height_px: 531,
        },
    );
    m
}

fn default_backgrounds() -> BTreeMap<String, BackgroundSpec> {
    let mut m = BTreeMap::new();
    m.insert(
        "white".into(),
        BackgroundSpec {
            name: "白".into(),
            rgb: [255, 255, 255],
        },
    );
    m.insert(
        "blue".into(),
        BackgroundSpec {
            name: "蓝".into(),
            rgb: [67, 142, 219],
        },
    );
    m.insert(
        "red".into(),
        BackgroundSpec {
            name: "红".into(),
            rgb: [184, 45, 50],
        },
    );
    m.insert(
        "gray".into(),
        BackgroundSpec {
            name: "灰".into(),
            rgb: [128, 128, 128],
        },
    );
    m
}

fn default_layout() -> BTreeMap<String, LayoutSpec> {
    let mut m = BTreeMap::new();
    m.insert(
        "6inch".into(),
        LayoutSpec {
            name: "6寸相纸".into(),
            width_mm: 102.0,
            height_mm: 152.0,
            margin_mm: 3.0,
            gap_mm: 2.0,
        },
    );
    m.insert(
        "a4".into(),
        LayoutSpec {
            name: "A4".into(),
            width_mm: 210.0,
            height_mm: 297.0,
            margin_mm: 8.0,
            gap_mm: 3.0,
        },
    );
    m
}

/// 默认配置文件查找路径（当前目录 application.toml）
pub fn default_config_path() -> Option<PathBuf> {
    let p = Path::new("application.toml");
    p.exists().then(|| p.to_path_buf())
}

/// 深度合并：overlay 覆盖 base（表递归合并，标量/数组整体覆盖）
fn merge_value(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Table(b), Value::Table(o)) => {
            for (k, v) in o {
                if let Some(bv) = b.get_mut(&k) {
                    merge_value(bv, v);
                } else {
                    b.insert(k, v);
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// 将 `[models.download]` 从注册表提取到 `models_download` 字段
fn extract_models_download(root: &mut Value) {
    let Some(models) = root.get_mut("models").and_then(|m| m.as_table_mut()) else {
        return;
    };
    if let Some(dl) = models.remove("download") {
        if let Some(cur) = root.get_mut("models_download") {
            merge_value(cur, dl);
        } else if let Some(tbl) = root.as_table_mut() {
            tbl.insert("models_download".into(), dl);
        }
    }
}

/// 环境变量注入：`PHOTOS_` + 配置路径大写、`.`/`-` → `_`
/// 例：`PHOTOS_GENERAL_LOG_LEVEL=DEBUG`、`PHOTOS_MODES_BALANCED_MATTING=rmbg`
fn inject_env(root: &mut Value) {
    let Some(tbl) = root.as_table_mut() else {
        return;
    };
    for (key, val) in std::env::vars() {
        let Some(suffix) = key.strip_prefix("PHOTOS_") else {
            continue;
        };
        if suffix.is_empty() {
            continue;
        }
        let segs: Vec<&str> = suffix.split('_').collect();
        inject_path(tbl, &segs, &env_value(&val));
    }
}

/// 环境变量值解析：数字 / 布尔注入为对应类型，否则按字符串
fn env_value(raw: &str) -> Value {
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Integer(i);
    }
    match raw {
        "true" => return Value::Boolean(true),
        "false" => return Value::Boolean(false),
        _ => {}
    }
    if let Ok(f) = raw.parse::<f64>() {
        return Value::Float(f);
    }
    Value::String(raw.to_string())
}

/// 按路径段注入叶子值（贪心最长表匹配；键统一小写后匹配配置键）
fn inject_path(tbl: &mut toml::map::Map<String, Value>, segs: &[&str], val: &Value) {
    if segs.is_empty() {
        return;
    }
    if segs.len() == 1 {
        insert_leaf(tbl, segs[0].to_lowercase(), val.clone());
        return;
    }
    for len in (1..segs.len()).rev() {
        let key = segs[..len].join("_").to_lowercase();
        if let Some(Value::Table(child)) = tbl.get_mut(&key) {
            inject_path(child, &segs[len..], val);
            return;
        }
    }
    insert_leaf(tbl, segs.join("_").to_lowercase(), val.clone());
}

/// 叶子插入（数组字段不支持环境变量覆盖，防止破坏结构）
fn insert_leaf(tbl: &mut toml::map::Map<String, Value>, key: String, val: Value) {
    if let Some(existing) = tbl.get(&key) {
        if existing.is_array() {
            return;
        }
    }
    tbl.insert(key, val);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量测试串行化（避免并行污染）
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_envs<K, V>(pairs: &[(K, V)], f: impl FnOnce()) -> std::sync::MutexGuard<'static, ()>
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        // 容忍此前测试 panic 导致的锁中毒
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let keys: Vec<String> = pairs.iter().map(|(k, _)| k.as_ref().to_string()).collect();
        for (k, v) in pairs {
            unsafe {
                std::env::set_var(k.as_ref(), v.as_ref());
            }
        }
        let _cleanup = EnvCleanup(keys);
        f();
        _guard
    }

    /// 环境变量清理守卫：无论 f 是否 panic，都在 Drop 时移除变量
    struct EnvCleanup(Vec<String>);
    impl Drop for EnvCleanup {
        fn drop(&mut self) {
            for k in &self.0 {
                unsafe {
                    std::env::remove_var(k);
                }
            }
        }
    }

    #[test]
    fn 默认配置与样例一致() {
        let cfg = Config::default();
        assert_eq!(cfg.general.data_dir, "data");
        assert_eq!(cfg.general.log_dir, "logs");
        assert_eq!(cfg.general.log_level, LogLevel::Info);
        assert_eq!(cfg.general.default_mode, "balanced");
        assert_eq!(cfg.mode("balanced").unwrap().matting, "birefnet_lite");
        assert_eq!(cfg.mode("speed").unwrap().face, "mtcnn");
        assert_eq!(
            cfg.mode("quality").unwrap().execution_provider,
            ExecutionProvider::Cuda
        );
        assert_eq!(cfg.size("one_inch").unwrap().width_px, 295);
        assert_eq!(cfg.size("one_inch").unwrap().height_px, 413);
        assert_eq!(cfg.background("blue").unwrap().rgb, [67, 142, 219]);
        assert_eq!(cfg.layout.get("6inch").unwrap().width_mm, 102.0);
        assert!(!cfg.beauty.enabled);
        // 默认配置中不存在 "download" 注册条目
        assert!(!cfg.models.contains_key("download"));
    }

    #[test]
    fn 尺寸库覆盖签证与国内证件规格() {
        let cfg = Config::default();
        // 签证类：美国 2×2 英寸、日本 45×45、申根/英国 35×45
        assert_eq!(cfg.size("us_visa").unwrap().width_px, 600);
        assert_eq!(cfg.size("us_visa").unwrap().height_px, 600);
        assert_eq!(cfg.size("japan_visa").unwrap().width_px, 531);
        assert_eq!(cfg.size("japan_visa").unwrap().height_px, 531);
        assert_eq!(cfg.size("schengen_visa").unwrap().width_px, 413);
        assert_eq!(cfg.size("schengen_visa").unwrap().height_px, 531);
        assert_eq!(cfg.size("uk_visa").unwrap().width_px, 413);
        assert_eq!(cfg.size("uk_visa").unwrap().height_px, 531);
        // 国内证件：护照/港澳通行证 33×48、驾驶证 22×32、社保卡与居住证 26×32
        assert_eq!(cfg.size("passport").unwrap().width_px, 390);
        assert_eq!(cfg.size("passport").unwrap().height_px, 567);
        assert_eq!(cfg.size("hkmo_permit").unwrap().width_px, 390);
        assert_eq!(cfg.size("hkmo_permit").unwrap().height_px, 567);
        assert_eq!(cfg.size("driver_license").unwrap().width_px, 260);
        assert_eq!(cfg.size("driver_license").unwrap().height_px, 378);
        assert_eq!(cfg.size("social_security_card").unwrap().width_px, 307);
        assert_eq!(cfg.size("social_security_card").unwrap().height_px, 378);
        assert_eq!(cfg.size("residence_permit").unwrap().width_px, 307);
        assert_eq!(cfg.size("residence_permit").unwrap().height_px, 378);
        // 考试报名与加宽规格：大一寸 33×48、小二寸 35×45、考试报名 35×45
        assert_eq!(cfg.size("big_one_inch").unwrap().width_px, 390);
        assert_eq!(cfg.size("big_one_inch").unwrap().height_px, 567);
        assert_eq!(cfg.size("small_two_inch").unwrap().width_px, 413);
        assert_eq!(cfg.size("small_two_inch").unwrap().height_px, 531);
        assert_eq!(cfg.size("exam_registration").unwrap().width_px, 413);
        assert_eq!(cfg.size("exam_registration").unwrap().height_px, 531);
    }

    #[test]
    fn 资源限制默认值与并发零值校验() {
        let cfg = Config::default();
        assert_eq!(cfg.general.max_input_side, 0);
        assert_eq!(cfg.inference.intra_threads, 0);
        assert_eq!(cfg.inference.inter_threads, 0);
        assert!(cfg.inference.memory_pattern);
        assert_eq!(cfg.server.max_concurrent_tasks, 2);
        // 并发上限为 0 会导致任务永久排队，视为非法配置
        let mut bad = Config::default();
        bad.server.max_concurrent_tasks = 0;
        let err = bad.validate().unwrap_err().to_string();
        assert!(err.contains("max_concurrent_tasks"), "实际 {err}");
    }

    #[test]
    fn toml覆盖资源限制() {
        // 与 env 测试串行，避免读到并行测试设置的临时环境变量
        let _g = with_envs::<&str, &str>(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("application.toml");
            std::fs::write(
                &path,
                r#"
[general]
max_input_side = 1600

[inference]
intra_threads = 4
inter_threads = 1
memory_pattern = false

[server]
max_concurrent_tasks = 1
"#,
            )
            .unwrap();
            let cfg = Config::load_from(Some(&path)).unwrap();
            assert_eq!(cfg.general.max_input_side, 1600);
            assert_eq!(cfg.inference.intra_threads, 4);
            assert_eq!(cfg.inference.inter_threads, 1);
            assert!(!cfg.inference.memory_pattern);
            assert_eq!(cfg.server.max_concurrent_tasks, 1);
            cfg.validate().unwrap();
        });
    }

    #[test]
    fn toml文件覆盖默认值() {
        // 与 env 测试串行，避免读到并行测试设置的临时环境变量
        let _g = with_envs::<&str, &str>(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("application.toml");
            std::fs::write(
                &path,
                r#"
[general]
log_level = "DEBUG"
default_mode = "speed"

[models.download]
enabled = false

[backgrounds.custom]
name = "自定义"
rgb = [10, 20, 30]
"#,
            )
            .unwrap();
            let cfg = Config::load_from(Some(&path)).unwrap();
            assert_eq!(cfg.general.log_level, LogLevel::Debug);
            assert_eq!(cfg.general.default_mode, "speed");
            // [models.download] 被提取，不进注册表
            assert!(!cfg.models.contains_key("download"));
            assert!(!cfg.models_download.enabled);
            assert_eq!(cfg.models_download.concurrency, 2); // 未覆盖，沿用默认
            // 新增底色合并保留
            assert_eq!(cfg.background("custom").unwrap().rgb, [10, 20, 30]);
            assert_eq!(cfg.background("white").unwrap().rgb, [255, 255, 255]);
        });
    }

    #[test]
    fn 环境变量覆盖toml() {
        let _g = with_envs(
            &[
                ("PHOTOS_GENERAL_LOG_LEVEL", "ERROR"),
                ("PHOTOS_MODES_BALANCED_MATTING", "rmbg"),
            ],
            || {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("application.toml");
                std::fs::write(&path, "[general]\nlog_level = \"DEBUG\"\n").unwrap();
                let cfg = Config::load_from(Some(&path)).unwrap();
                // 环境变量优先于 toml
                assert_eq!(cfg.general.log_level, LogLevel::Error);
                assert_eq!(cfg.mode("balanced").unwrap().matting, "rmbg");
            },
        );
    }

    #[test]
    fn 环境变量深层路径注入() {
        let _g = with_envs(
            &[
                ("PHOTOS_SIZES_ONE_INCH_WIDTH_PX", "300"),
                ("PHOTOS_GENERAL_MODELS_DIR", "mymodels"),
            ],
            || {
                let cfg = Config::load_from(None).unwrap();
                assert_eq!(cfg.size("one_inch").unwrap().width_px, 300);
                assert_eq!(cfg.general.models_dir, "mymodels");
            },
        );
    }

    #[test]
    fn 数组字段不受环境变量破坏() {
        let _g = with_envs(&[("PHOTOS_MODELS_RETINAFACE_INPUT_DIMS", "9999")], || {
            let cfg = Config::load_from(None).unwrap();
            assert_eq!(
                cfg.model_spec("retinaface").unwrap().input_dims,
                vec![1, 3, 640, 640]
            );
        });
    }

    #[test]
    fn 未知默认模式校验失败() {
        let _g = with_envs::<&str, &str>(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("application.toml");
            std::fs::write(&path, "[general]\ndefault_mode = \"nope\"\n").unwrap();
            let err = Config::load_from(Some(&path)).unwrap_err();
            assert!(err.to_string().contains("默认模式"));
        });
    }

    #[test]
    fn 未注册模型引用校验失败() {
        let _g = with_envs::<&str, &str>(&[], || {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("application.toml");
            std::fs::write(
                &path,
                "[modes.balanced]\nface = \"不存在的模型\"\nkeypoint = \"movnet_light\"\nmatting = \"rmbg\"\n",
            )
            .unwrap();
            let err = Config::load_from(Some(&path)).unwrap_err();
            assert!(err.to_string().contains("未注册模型"));
        });
    }

    #[test]
    fn speed级联逻辑id校验通过() {
        // 默认配置：speed.face=mtcnn（级联逻辑 id），注册表含 mtcnn_pnet/rnet/onet 三子模型
        let _g = with_envs::<&str, &str>(&[], || {
            let cfg = Config::load_from(None).unwrap();
            assert!(cfg.models.contains_key("mtcnn_pnet"));
            assert!(cfg.models.contains_key("mtcnn_rnet"));
            assert!(cfg.models.contains_key("mtcnn_onet"));
            cfg.validate().unwrap();
        });
    }

    #[test]
    fn speed级联缺子模型校验失败() {
        // 移除一个子模型后，级联逻辑 id 校验应报错
        let _g = with_envs::<&str, &str>(&[], || {
            let mut cfg = Config::default();
            cfg.models.remove("mtcnn_onet");
            let err = cfg.validate().unwrap_err().to_string();
            assert!(err.contains("级联"), "实际 {err}");
            assert!(err.contains("mtcnn_onet"), "实际 {err}");
        });
    }
}
