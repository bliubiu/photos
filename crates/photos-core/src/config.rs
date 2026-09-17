//! 统一配置：`application.toml` 解析与合并。
//!
//! 参数优先级：命令行 > 环境变量（`PHOTOS_` 前缀）> toml 配置文件 > 默认值。
//! 唯一权威样例：`docs/examples/application.toml`（解析测试以其为夹具）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::error::{CoreError, CoreResult};

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
        for (suite_id, suite) in &self.modes {
            for (role, model_id) in [
                ("人脸检测 face", &suite.face),
                ("关键点 keypoint", &suite.keypoint),
                ("抠图 matting", &suite.matting),
            ] {
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

fn default_models() -> BTreeMap<String, ModelSpec> {
    let mut m = BTreeMap::new();
    for (id, path, dims) in [
        ("mtcnn", "models/mtcnn.onnx", vec![1, 3, 640, 640]),
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

/// 默认注册表下载地址（一键下载开箱即用；balanced 三件套已就绪，其余可自行补充）
fn default_download_url(id: &str) -> Option<ModelDownload> {
    let url = match id {
        "retinaface" => {
            "https://github.com/Zeyi-Lin/HivisionIDPhotos/releases/download/pretrained-model/retinaface-resnet50.onnx"
        }
        "movnet_light" => {
            "https://huggingface.co/Xenova/movenet-singlepose-lightning/resolve/main/onnx/model.onnx"
        }
        "birefnet_lite" => {
            "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx"
        }
        "parsing_lip" => {
            "https://huggingface.co/levihsu/OOTDiffusion/resolve/main/checkpoints/humanparsing/parsing_lip.onnx"
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
}
