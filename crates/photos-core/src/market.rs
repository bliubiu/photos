//! 模型市场清单：内置可下载模型目录 + 用户覆盖文件解析。
//!
//! 设计要点：
//! - 内置清单以 TOML 资源文件承载（`resources/model_market.toml`），用 `include_str!` 随二进制
//!   内嵌，**离线可用**、不依赖网络；处理链路本身零联网，下载只是可选便利。
//! - 用户可用 `data/model_market.toml` 按 `id` 覆盖内置条目（补齐直链与 sha256 后启用下载），
//!   避免为了填入真实地址而修改随包发布的内置清单。
//! - 内置条目默认 `enabled = false` 且 url/sha256 留空：WebUI 可展示清单但禁用下载按钮，
//!   待用户补齐真实直链后再启用（占位 sha256 不允许当作已就绪，与 `model.rs` 策略一致）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::config::{CUSTOM_MODELS_FILE, Config, ModelRole, ModelSpec};
use crate::error::{CoreError, CoreResult};

/// 内置清单资源（随二进制内嵌，离线可用）
const BUILTIN_MARKET: &str = include_str!("../resources/model_market.toml");

/// 市场条目：一个模型的一个可下载版本
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketEntry {
    /// 模型 id（与 `[models.<id>]` 一致）
    pub id: String,
    /// 版本号（写入 `models/<id>/<版本>/` 的目录名）
    pub version: String,
    /// 模型角色
    #[serde(default)]
    pub role: ModelRole,
    /// 下载地址（空表示未提供，禁用下载）
    #[serde(default)]
    pub url: String,
    /// 64 位小写十六进制 sha256（空表示未定版，禁止下载后直接启用）
    #[serde(default)]
    pub sha256: String,
    /// 文件字节数（0 表示未知）
    #[serde(default)]
    pub size: u64,
    /// 许可证标识（供用户判断可用性）
    #[serde(default)]
    pub license: String,
    /// 来源说明（官方仓库 / 发布页）
    #[serde(default)]
    pub source: String,
    /// 是否可下载（需同时具备 url 与 sha256）
    #[serde(default)]
    pub enabled: bool,
}

impl MarketEntry {
    /// 是否具备下载条件（启用且 url、sha256 均已填写且 sha256 非占位）
    pub fn downloadable(&self) -> bool {
        self.enabled
            && !self.url.trim().is_empty()
            && self.sha256.len() == 64
            && !self.sha256.chars().all(|c| c == '0')
    }
}

/// 市场清单文件结构（`[[models]]` 数组）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MarketFile {
    #[serde(default)]
    models: Vec<MarketEntry>,
}

/// 加载市场清单：内置条目 + 用户覆盖文件（同 `id` + `version` 覆盖，用户可追加新条目）
pub fn load(overlay: Option<&Path>) -> CoreResult<Vec<MarketEntry>> {
    let mut entries = parse(BUILTIN_MARKET, "内置模型清单")?;
    if let Some(p) = overlay {
        if p.exists() {
            let content = std::fs::read_to_string(p).map_err(|e| {
                CoreError::ConfigParse(format!("读取模型清单 {} 失败：{e}", p.display()))
            })?;
            let user = parse(&content, &format!("用户模型清单 {}", p.display()))?;
            for item in user {
                match entries
                    .iter_mut()
                    .find(|e| e.id == item.id && e.version == item.version)
                {
                    Some(slot) => *slot = item,
                    None => entries.push(item),
                }
            }
        }
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.version.cmp(&b.version)));
    Ok(entries)
}

/// 解析清单文本（含条目合法性校验，错误信息为中文）
fn parse(content: &str, source: &str) -> CoreResult<Vec<MarketEntry>> {
    let file: MarketFile = toml::from_str(content)
        .map_err(|e| CoreError::ConfigParse(format!("{source}解析失败：{e}")))?;
    for e in &file.models {
        if e.id.trim().is_empty() {
            return Err(CoreError::ConfigParse(format!(
                "{source}存在 id 为空的条目"
            )));
        }
        if e.version.trim().is_empty() {
            return Err(CoreError::ConfigParse(format!(
                "{source}的模型“{}”缺少 version",
                e.id
            )));
        }
        if !e.sha256.is_empty()
            && (e.sha256.len() != 64 || !e.sha256.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(CoreError::ConfigParse(format!(
                "{source}的模型“{}”版本“{}”的 sha256 非法：需 64 位十六进制或留空",
                e.id, e.version
            )));
        }
    }
    Ok(file.models)
}

/// 按模型 id 归组：id → 该模型全部可下载版本
pub fn group_by_id(
    entries: &[MarketEntry],
) -> std::collections::BTreeMap<String, Vec<MarketEntry>> {
    let mut map: std::collections::BTreeMap<String, Vec<MarketEntry>> =
        std::collections::BTreeMap::new();
    for e in entries {
        map.entry(e.id.clone()).or_default().push(e.clone());
    }
    map
}

/// 市场清单的用户覆盖文件路径（`<data_dir>/model_market.toml`）
pub fn overlay_path(cfg: &Config) -> PathBuf {
    Path::new(&cfg.general.data_dir).join("model_market.toml")
}

/// 自定义模型注册表路径（`<data_dir>/models.custom.toml`）
pub fn custom_registry_path(cfg: &Config) -> PathBuf {
    Path::new(&cfg.general.data_dir).join(CUSTOM_MODELS_FILE)
}

/// 注册自定义模型：条目合入 `<data_dir>/models.custom.toml`（同 id 覆盖），
/// 写入前用「默认配置 + 当前生效模型表 + 候选文件」完整校验，非法声明不会落盘。
/// 返回是否覆盖了同 id 的既有条目；注册结果**重启进程后生效**（`AppState.cfg` 不可变）。
pub fn register_custom(cfg: &Config, id: &str, spec: &ModelSpec) -> CoreResult<bool> {
    let path = custom_registry_path(cfg);
    let mut file_root: Value = if path.exists() {
        let content = std::fs::read_to_string(&path).map_err(|e| {
            CoreError::ConfigParse(format!("读取自定义模型注册表 {} 失败：{e}", path.display()))
        })?;
        toml::from_str(&content).map_err(|e| {
            CoreError::ConfigParse(format!("自定义模型注册表 {} 解析失败：{e}", path.display()))
        })?
    } else {
        Value::Table(toml::map::Map::new())
    };

    let models = file_root
        .as_table_mut()
        .ok_or_else(|| CoreError::ConfigParse("自定义模型注册表格式非法：根节点必须是表".into()))?
        .entry("models".to_string())
        .or_insert_with(|| Value::Table(toml::map::Map::new()));
    let models_tbl = models.as_table_mut().ok_or_else(|| {
        CoreError::ConfigParse("自定义模型注册表格式非法：[models] 必须是表".into())
    })?;
    let entry = Value::try_from(spec).map_err(|e| CoreError::ConfigParse(e.to_string()))?;
    let replaced = models_tbl.insert(id.to_string(), entry).is_some();

    let mut candidate =
        Value::try_from(Config::default()).map_err(|e| CoreError::ConfigParse(e.to_string()))?;
    let mut current = toml::map::Map::new();
    for (k, v) in &cfg.models {
        current.insert(
            k.clone(),
            Value::try_from(v).map_err(|e| CoreError::ConfigParse(e.to_string()))?,
        );
    }
    crate::config::merge_value(&mut candidate, Value::Table(current));
    crate::config::merge_value(&mut candidate, file_root.clone());
    let merged: Config = candidate
        .try_into()
        .map_err(|e| CoreError::ConfigMerge(e.to_string()))?;
    merged.validate()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(&file_root)
        .map_err(|e| CoreError::ConfigParse(format!("序列化自定义模型注册表失败：{e}")))?;
    std::fs::write(&path, text)?;
    tracing::info!(
        "已注册自定义模型“{id}”到 {}（校验通过，重启后生效）",
        path.display()
    );
    Ok(replaced)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 内置清单可解析且与注册表模型id一致() {
        let entries = load(None).unwrap();
        assert!(!entries.is_empty(), "内置清单不应为空");
        // 内置清单覆盖的模型 id 必须已在内置注册表中，避免清单指向未知模型
        let cfg = crate::config::Config::load_from(None).unwrap();
        for e in &entries {
            assert!(
                cfg.models.contains_key(&e.id),
                "清单模型“{}”未在内置注册表中",
                e.id
            );
        }
    }

    #[test]
    fn 内置条目默认不可下载() {
        let entries = load(None).unwrap();
        for e in &entries {
            assert!(!e.downloadable(), "内置条目“{}”默认不应可直接下载", e.id);
        }
    }

    #[test]
    fn 用户清单覆盖与追加生效() {
        let dir = std::env::temp_dir().join(format!("photos_market_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model_market.toml");
        std::fs::write(
            &path,
            r#"
[[models]]
id = "rmbg"
version = "1.4.0"
role = "matting"
url = "https://example.com/rmbg.onnx"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
size = 176000000
license = "bria-rmbg"
enabled = true

[[models]]
id = "my_custom_model"
version = "2.0.0"
role = "matting"
url = "https://example.com/custom.onnx"
sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
enabled = true
"#,
        )
        .unwrap();

        let entries = load(Some(&path)).unwrap();
        let rmbg = entries
            .iter()
            .find(|e| e.id == "rmbg" && e.version == "1.4.0")
            .expect("覆盖后的 rmbg 条目应存在");
        assert_eq!(rmbg.license, "bria-rmbg");
        assert!(rmbg.downloadable(), "补齐直链后应可下载");
        assert!(entries.iter().any(|e| e.id == "my_custom_model"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 非法sha256给出中文报错() {
        let err = parse(
            r#"
[[models]]
id = "rmbg"
version = "1.4.0"
sha256 = "not-a-hash"
"#,
            "测试清单",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("sha256 非法"), "实际：{err}");
    }

    #[test]
    fn 缺少版本号给出中文报错() {
        let err = parse(
            r#"
[[models]]
id = "rmbg"
version = ""
"#,
            "测试清单",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("缺少 version"), "实际：{err}");
    }

    #[test]
    fn 按id归组版本() {
        let entries = load(None).unwrap();
        let grouped = group_by_id(&entries);
        assert!(grouped.contains_key("rmbg"));
        assert!(!grouped["rmbg"].is_empty());
    }

    /// 构造临时工程：application.toml 指定 data_dir，返回（临时目录, 应用配置路径, 配置）
    fn setup_project() -> (tempfile::TempDir, PathBuf, Config) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let app = dir.path().join("application.toml");
        std::fs::write(
            &app,
            format!(
                "[general]\ndata_dir = \"{}\"\n",
                data.display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();
        let cfg = Config::load_from(Some(&app)).unwrap();
        (dir, app, cfg)
    }

    fn custom_spec(dims: Vec<i64>) -> ModelSpec {
        ModelSpec {
            path: "models/my_matting.onnx".into(),
            sha256: "1".repeat(64),
            input_dims: dims,
            enabled: true,
            download: None,
            role: ModelRole::Matting,
            preprocess: crate::config::Preprocess {
                layout: crate::config::Layout::Nchw,
                resize: crate::config::ResizeMode::Stretch,
                norm: crate::config::Norm::MeanStd {
                    mean: [0.5, 0.5, 0.5],
                    std: [0.5, 0.5, 0.5],
                },
                channel: crate::config::ChannelOrder::Rgb,
            },
        }
    }

    #[test]
    fn 注册自定义模型落盘并可被配置加载() {
        let (_dir, app, cfg) = setup_project();
        let spec = custom_spec(vec![1, 3, 512, 512]);
        // 首次注册返回 false（非覆盖），重复注册返回 true
        assert!(!register_custom(&cfg, "my_matting", &spec).unwrap());
        assert!(register_custom(&cfg, "my_matting", &spec).unwrap());
        assert!(custom_registry_path(&cfg).exists());

        let reloaded = Config::load_from(Some(&app)).unwrap();
        let got = reloaded
            .models
            .get("my_matting")
            .expect("注册后应可通过配置加载");
        assert_eq!(got.role, ModelRole::Matting);
        assert_eq!(got.input_dims, vec![1, 3, 512, 512]);
    }

    #[test]
    fn 非法预处理声明拒绝注册且不落盘() {
        let (_dir, _app, cfg) = setup_project();
        // 声明 nchw 但第二维不是 3 → 校验失败
        let bad = custom_spec(vec![1, 512, 512, 3]);
        let err = register_custom(&cfg, "bad_model", &bad)
            .unwrap_err()
            .to_string();
        assert!(err.contains("preprocess.layout"), "实际：{err}");
        assert!(
            !custom_registry_path(&cfg).exists(),
            "校验失败时不应写入注册表文件"
        );
    }

    #[test]
    fn 自定义模型解析非法角色给出中文报错() {
        let err = ModelRole::parse(" banana ").unwrap_err().to_string();
        assert!(err.contains("未知模型角色"), "实际：{err}");
    }
}
