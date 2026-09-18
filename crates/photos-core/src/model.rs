//! 模型管理：注册表读取 → 状态报告。
//!
//! 校验策略（docs/04-模型清单.md §5）：
//! - 惰性校验：首次 sha256 全量哈希，结果按 `路径 + mtime + size → sha256` 缓存到 kv_cache
//! - 文件未变跳过全量哈希（缓存命中目标 < 100ms）
//! - 注册表 sha256 为占位全 0（未定版）时，即使文件存在也不视为就绪

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::{CoreError, CoreResult};
use crate::storage::Store;

/// 模型校验状态（与 docs/04-模型清单.md §5 枚举一致）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// 文件存在且 sha256 与注册表一致
    Ready,
    /// 缓存命中且路径/mtime/size 未变
    CachedOk,
    /// 文件不存在
    Missing,
    /// 文件存在但 sha256 不一致（含未定版占位）
    HashMismatch,
}

impl CheckStatus {
    /// 中文状态名
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Ready => "就绪",
            CheckStatus::CachedOk => "缓存命中",
            CheckStatus::Missing => "缺失",
            CheckStatus::HashMismatch => "校验失败",
        }
    }

    /// 内部枚举名（与模型清单文档/API 一致）
    pub fn code(self) -> &'static str {
        match self {
            CheckStatus::Ready => "ready",
            CheckStatus::CachedOk => "cached_ok",
            CheckStatus::Missing => "missing",
            CheckStatus::HashMismatch => "hash_mismatch",
        }
    }
}

/// 单模型状态报告
#[derive(Debug, Clone)]
pub struct ModelStatus {
    pub id: String,
    pub path: PathBuf,
    pub expected_sha256: String,
    pub check_status: CheckStatus,
    pub message: String,
}

/// 激活版本在 sqlite `prefs` 中的键前缀
pub const MODEL_VERSION_PREFIX: &str = "model_version:";

/// 模型版本目录：`<models_dir>/<id>`（各版本为其下的子目录）
pub fn model_version_dir(cfg: &Config, id: &str) -> PathBuf {
    Path::new(&cfg.general.models_dir).join(id)
}

/// 版本目录内的模型文件名（取注册表 `path` 的文件名，如 `retinaface_r50.onnx`）
pub fn version_file_name(spec_path: &Path) -> String {
    spec_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "model.onnx".to_string())
}

/// 指定版本对应的模型文件路径：`<models_dir>/<id>/<版本>/<文件名>`
pub fn version_path(cfg: &Config, id: &str, version: &str, spec_path: &Path) -> PathBuf {
    model_version_dir(cfg, id)
        .join(version)
        .join(version_file_name(spec_path))
}

/// 版本号比较：按 `.` 分段数值比较，非数值段回退字典序（避免 `1.10.0` 排在 `1.9.0` 之前）
fn compare_version(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<&str> = a.split('.').collect();
    let pb: Vec<&str> = b.split('.').collect();
    for i in 0..pa.len().max(pb.len()) {
        let sa = pa.get(i).copied().unwrap_or("");
        let sb = pb.get(i).copied().unwrap_or("");
        let ord = match (sa.parse::<u64>(), sb.parse::<u64>()) {
            (Ok(na), Ok(nb)) => na.cmp(&nb),
            _ => sa.cmp(sb),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// 某模型已下载的版本列表（升序；仅统计含对应模型文件的目录）
pub fn list_versions(cfg: &Config, id: &str) -> Vec<String> {
    let spec = match cfg.models.get(id) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let dir = model_version_dir(cfg, id);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|v| version_path(cfg, id, v, Path::new(&spec.path)).exists())
        .collect();
    out.sort_by(|a, b| compare_version(a, b));
    out
}

/// 读取 sqlite 记录的激活版本（无记录返回 None；目录已不存在时视为无效）
pub fn active_version(cfg: &Config, store: &Store, id: &str) -> CoreResult<Option<String>> {
    let key = format!("{MODEL_VERSION_PREFIX}{id}");
    let Some(v) = store.get_pref(&key)? else {
        return Ok(None);
    };
    let spec = cfg.model_spec(id)?;
    if version_path(cfg, id, &v, Path::new(&spec.path)).exists() {
        Ok(Some(v))
    } else {
        Ok(None)
    }
}

/// 激活指定版本：校验版本文件存在、sha256 与注册表一致（注册表未定版时跳过），并记录到 prefs
pub fn activate_version(cfg: &Config, store: &Store, id: &str, version: &str) -> CoreResult<()> {
    let spec = cfg.model_spec(id)?;
    let path = version_path(cfg, id, version, Path::new(&spec.path));
    if !path.exists() {
        return Err(CoreError::Model(format!(
            "模型“{id}”的版本“{version}”不存在：{}；请先下载或用一键下载补齐",
            path.display()
        )));
    }
    if !spec.sha256.chars().all(|c| c == '0') {
        let actual = sha256_file(&path)?;
        if actual != spec.sha256 {
            return Err(CoreError::Model(format!(
                "模型“{id}”版本“{version}”sha256 校验失败：预期 {}，实际 {actual}",
                spec.sha256
            )));
        }
    }
    store.set_pref(&format!("{MODEL_VERSION_PREFIX}{id}"), version)?;
    Ok(())
}

/// 解析模型路径（多版本优先）：
/// 1. 注册表 `path` 指向的文件存在 → 直接使用（旧布局、旧库零迁移）
/// 2. 否则若 prefs 记录的激活版本文件存在 → 使用该版本
/// 3. 否则若版本目录下存在版本 → 取最高版本并回写 prefs
/// 4. 都没有 → 返回注册表路径（由调用方判定为缺失）
pub fn resolve_model_path_versioned(
    cfg: &Config,
    store: &Store,
    id: &str,
    spec_path: &Path,
) -> CoreResult<PathBuf> {
    let direct = resolve_model_path(cfg, spec_path);
    if direct.exists() {
        return Ok(direct);
    }
    if let Some(v) = active_version(cfg, store, id)? {
        return Ok(version_path(cfg, id, &v, spec_path));
    }
    if let Some(v) = list_versions(cfg, id).last() {
        // 首次发现版本目录：记录为激活版本，便于后续切换与展示
        store.set_pref(&format!("{MODEL_VERSION_PREFIX}{id}"), v)?;
        return Ok(version_path(cfg, id, v, spec_path));
    }
    Ok(direct)
}

/// 校验全部启用模型，返回状态报告列表
pub fn check_models(cfg: &Config, store: &Store) -> CoreResult<Vec<ModelStatus>> {
    let mut out = Vec::new();
    for (id, spec) in &cfg.models {
        if !spec.enabled {
            continue;
        }
        out.push(check_one(cfg, store, id, spec)?);
    }
    Ok(out)
}

/// 校验单个模型文件
fn check_one(
    cfg: &Config,
    store: &Store,
    id: &str,
    spec: &crate::config::ModelSpec,
) -> CoreResult<ModelStatus> {
    let expected = spec.sha256.as_str();
    let path = resolve_model_path_versioned(cfg, store, id, Path::new(&spec.path))?;

    if !path.exists() {
        return Ok(ModelStatus {
            id: id.to_string(),
            path,
            expected_sha256: expected.to_string(),
            check_status: CheckStatus::Missing,
            message: "模型文件缺失；请按 docs/04-模型清单.md §6 放置模型或使用一键下载".into(),
        });
    }

    let meta = std::fs::metadata(&path)?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let cache_key = format!("sha256:{id}:{}:{mtime_ns}:{}", path.display(), meta.len());

    // 缓存命中且与注册表期望一致 → 跳过全量哈希
    if let Some(cached) = store.kv_get(&cache_key)? {
        if cached == expected {
            return Ok(ModelStatus {
                id: id.to_string(),
                path,
                expected_sha256: expected.to_string(),
                check_status: CheckStatus::CachedOk,
                message: "缓存命中，文件未变化，跳过全量哈希".into(),
            });
        }
    }

    // 全量哈希校验
    let actual = sha256_file(&path)?;
    if actual == expected {
        store.kv_set(&cache_key, &actual)?;
        return Ok(ModelStatus {
            id: id.to_string(),
            path,
            expected_sha256: expected.to_string(),
            check_status: CheckStatus::Ready,
            message: "文件存在且 sha256 与注册表一致".into(),
        });
    }

    if expected.chars().all(|c| c == '0') {
        return Ok(ModelStatus {
            id: id.to_string(),
            path,
            expected_sha256: expected.to_string(),
            check_status: CheckStatus::HashMismatch,
            message: "模型 sha256 尚未定版（占位全 0），禁止当作就绪；请按 docs/04-模型清单.md §7 回填后重试".into(),
        });
    }

    Ok(ModelStatus {
        id: id.to_string(),
        path,
        expected_sha256: expected.to_string(),
        check_status: CheckStatus::HashMismatch,
        message: format!(
            "sha256 不一致：预期 {expected}，实际 {actual}；请更换模型文件或更新注册表"
        ),
    })
}

/// 解析模型路径：
/// - **绝对路径**：直接使用
/// - **相对路径**：相对 `cfg.general.models_dir`（默认 `models`）
///   1. 若 `spec.path` 已以 `models_dir` 开头，剥离该前缀后拼接，避免 `models/models/…`
///   2. 否则若以历史默认前缀 `models/` 开头，同样剥离后接到当前 `models_dir`
///   3. 否则视为相对 `models_dir` 的文件名/子路径
///
/// 注册表样例写 `models/retinaface_r50.onnx`；改为 `models_dir = "mymodels"` 后应落到
/// `mymodels/retinaface_r50.onnx`，而不是继续写死项目根 `models/`。
pub fn resolve_model_path(cfg: &Config, spec_path: &Path) -> PathBuf {
    if spec_path.is_absolute() {
        return spec_path.to_path_buf();
    }
    let models_dir = Path::new(&cfg.general.models_dir);
    let rel = spec_path
        .strip_prefix(models_dir)
        .or_else(|_| spec_path.strip_prefix("models"))
        .unwrap_or(spec_path);
    if rel.as_os_str().is_empty() {
        return models_dir.to_path_buf();
    }
    models_dir.join(rel)
}

/// 计算文件 sha256（十六进制小写）
pub fn sha256_file(path: &Path) -> CoreResult<String> {
    let mut file = File::open(path)
        .map_err(|e| CoreError::Model(format!("打开模型文件 {} 失败：{e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| CoreError::Model(format!("读取模型文件 {} 失败：{e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

/// 字节数组转小写十六进制
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 汇总：就绪模型数量（ready + cached_ok）
pub fn ready_count(statuses: &[ModelStatus]) -> usize {
    statuses
        .iter()
        .filter(|s| matches!(s.check_status, CheckStatus::Ready | CheckStatus::CachedOk))
        .count()
}

/// 下载失败自动重试次数
const DOWNLOAD_MAX_RETRIES: u32 = 3;

/// 下载单个模型到注册表路径（`photos models download <id>`）。
/// 流程：读下载地址 → 临时文件流式写入 → sha256 校验（非占位时）→ 原子替换。
/// 依赖 `[models.<id>].download.url`；超时取 `[models_download].timeout_secs`。
pub fn download_model(cfg: &Config, model_id: &str) -> CoreResult<()> {
    let spec = cfg.model_spec(model_id)?;
    let url = spec
        .download
        .as_ref()
        .and_then(|d| d.url.as_deref())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            CoreError::Download(format!(
                "模型“{model_id}”未配置下载地址（请在配置 [models.{model_id}].download.url 填写）"
            ))
        })?;
    let path = resolve_model_path(cfg, Path::new(&spec.path));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let timeout = std::time::Duration::from_secs(cfg.models_download.timeout_secs.max(1));
    // 临时文件与目标同目录，保证 rename 原子替换
    let tmp = path.with_extension("onnx.downloading");

    let mut last_err = String::new();
    for attempt in 1..=DOWNLOAD_MAX_RETRIES {
        match download_to(url, &tmp, timeout) {
            Ok(()) => {
                last_err.clear();
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                let _ = std::fs::remove_file(&tmp);
                if attempt < DOWNLOAD_MAX_RETRIES {
                    continue;
                }
                return Err(CoreError::Download(format!(
                    "模型“{model_id}”下载失败（已重试 {DOWNLOAD_MAX_RETRIES} 次）：{last_err}"
                )));
            }
        }
    }

    // sha256 校验：注册表为占位全 0（未定版）时跳过校验、仅下载
    if !spec.sha256.chars().all(|c| c == '0') {
        let actual = sha256_file(&tmp)?;
        if actual != spec.sha256 {
            let _ = std::fs::remove_file(&tmp);
            return Err(CoreError::Download(format!(
                "模型“{model_id}”sha256 校验失败：预期 {}，实际 {actual}",
                spec.sha256
            )));
        }
    }

    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// 按显式地址下载指定版本到 `models/<id>/<版本>/`，校验 sha256 后原子替换，
/// 并记录为该模型的激活版本（市场清单下载入口；`expected_sha256` 必填且非占位）
pub fn download_version(
    cfg: &Config,
    store: &Store,
    id: &str,
    version: &str,
    url: &str,
    expected_sha256: &str,
) -> CoreResult<PathBuf> {
    if url.trim().is_empty() {
        return Err(CoreError::Download(format!(
            "模型“{id}”版本“{version}”未提供下载地址"
        )));
    }
    if expected_sha256.len() != 64 || expected_sha256.chars().all(|c| c == '0') {
        return Err(CoreError::Download(format!(
            "模型“{id}”版本“{version}”缺少有效 sha256，拒绝下载（无法校验文件完整性）"
        )));
    }
    let spec = cfg.model_spec(id)?;
    let path = version_path(cfg, id, version, Path::new(&spec.path));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let timeout = std::time::Duration::from_secs(cfg.models_download.timeout_secs.max(1));
    let tmp = path.with_extension("onnx.downloading");

    let mut last_err = String::new();
    for attempt in 1..=DOWNLOAD_MAX_RETRIES {
        match download_to(url, &tmp, timeout) {
            Ok(()) => {
                last_err.clear();
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                let _ = std::fs::remove_file(&tmp);
                if attempt < DOWNLOAD_MAX_RETRIES {
                    continue;
                }
                return Err(CoreError::Download(format!(
                    "模型“{id}”版本“{version}”下载失败（已重试 {DOWNLOAD_MAX_RETRIES} 次）：{last_err}"
                )));
            }
        }
    }

    let actual = sha256_file(&tmp)?;
    if actual != expected_sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err(CoreError::Download(format!(
            "模型“{id}”版本“{version}”sha256 校验失败：预期 {expected_sha256}，实际 {actual}"
        )));
    }
    std::fs::rename(&tmp, &path)?;
    store.set_pref(&format!("{MODEL_VERSION_PREFIX}{id}"), version)?;
    Ok(path)
}

/// 单次流式下载：HTTP GET 响应体写入临时文件（覆盖写入，保证重试幂等）
fn download_to(url: &str, tmp: &Path, timeout: std::time::Duration) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| format!("请求失败：{e}"))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP 状态码 {status}"));
    }
    let mut reader = resp.into_reader();
    let mut out = File::create(tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;
    std::io::copy(&mut reader, &mut out).map_err(|e| format!("写入失败：{e}"))?;
    Ok(())
}

/// 确保模型文件存在：缺失时按注册表下载地址自动下载（处理图片时无需手动执行下载命令）。
/// 已存在直接返回；无下载地址或下载失败时给出中文指引错误。
pub fn ensure_model_downloaded(cfg: &Config, model_id: &str) -> CoreResult<()> {
    let spec = cfg.model_spec(model_id)?;
    let path = resolve_model_path(cfg, Path::new(&spec.path));
    if path.exists() {
        return Ok(());
    }
    tracing::info!("模型“{model_id}”缺失，开始自动下载…");
    if let Err(e) = download_model(cfg, model_id) {
        return Err(CoreError::Model(format!(
            "模型“{model_id}”缺失且自动下载失败：{e}。请检查网络或按 docs/04-模型清单.md §6 放置模型后重试"
        )));
    }
    tracing::info!("模型“{model_id}”自动下载完成：{}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;
    use std::io::Write;

    fn setup() -> (tempfile::TempDir, Config, Store) {
        let dir = tempfile::tempdir().unwrap();
        // 用默认配置而非 load（避免读取测试进程的环境变量，防止并行测试互相干扰）
        let cfg = Config::default();
        let store = Store::open(&dir.path().join("photos.db")).unwrap();
        (dir, cfg, store)
    }

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn expected_of(content: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(content);
        hex_encode(&h.finalize())
    }

    /// 在 `<models_dir>/<id>/<版本>/` 下写入模型文件，返回其路径
    fn write_version(dir: &Path, cfg: &Config, id: &str, version: &str, content: &[u8]) -> PathBuf {
        let spec_path = Path::new(&cfg.models[id].path).to_path_buf();
        let path = version_path(cfg, id, version, &spec_path);
        let abs = dir.join(&path);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, content).unwrap();
        abs
    }

    #[test]
    fn 版本目录列表按数值升序且忽略无文件目录() {
        let (dir, mut cfg, _store) = setup();
        cfg.general.models_dir = dir.path().to_string_lossy().to_string();
        write_version(dir.path(), &cfg, "rmbg", "1.9.0", b"a");
        write_version(dir.path(), &cfg, "rmbg", "1.10.0", b"b");
        write_version(dir.path(), &cfg, "rmbg", "2.0.0", b"c");
        // 空目录（无模型文件）不计入
        std::fs::create_dir_all(dir.path().join("rmbg").join("3.0.0")).unwrap();

        assert_eq!(
            list_versions(&cfg, "rmbg"),
            vec![
                "1.9.0".to_string(),
                "1.10.0".to_string(),
                "2.0.0".to_string()
            ]
        );
        // 未知模型返回空
        assert!(list_versions(&cfg, "不存在").is_empty());
    }

    #[test]
    fn 注册表路径存在时优先于版本目录() {
        let (dir, mut cfg, store) = setup();
        cfg.general.models_dir = dir.path().to_string_lossy().to_string();
        // 注册表指向的文件存在（旧布局）
        let direct = dir.path().join("retinaface_r50.onnx");
        std::fs::write(&direct, b"legacy").unwrap();
        cfg.models.get_mut("retinaface").unwrap().path = direct.to_string_lossy().to_string();
        write_version(dir.path(), &cfg, "retinaface", "2.0.0", b"v2");

        let got = resolve_model_path_versioned(
            &cfg,
            &store,
            "retinaface",
            Path::new(&cfg.models["retinaface"].path),
        )
        .unwrap();
        assert_eq!(got, direct, "旧布局存在时应优先使用注册表路径");
    }

    #[test]
    fn 无记录时取最高版本并回写激活项() {
        let (dir, mut cfg, store) = setup();
        cfg.general.models_dir = dir.path().to_string_lossy().to_string();
        write_version(dir.path(), &cfg, "rmbg", "1.4.0", b"old");
        let newest = write_version(dir.path(), &cfg, "rmbg", "2.0.0", b"new");

        let got =
            resolve_model_path_versioned(&cfg, &store, "rmbg", Path::new(&cfg.models["rmbg"].path))
                .unwrap();
        assert_eq!(got, newest, "应取最高版本");
        assert_eq!(
            active_version(&cfg, &store, "rmbg").unwrap().as_deref(),
            Some("2.0.0")
        );
    }

    #[test]
    fn 激活已记录版本且目录缺失时视为无效() {
        let (dir, mut cfg, store) = setup();
        cfg.general.models_dir = dir.path().to_string_lossy().to_string();
        write_version(dir.path(), &cfg, "rmbg", "1.4.0", b"old");
        activate_version(&cfg, &store, "rmbg", "1.4.0").unwrap();
        assert_eq!(
            active_version(&cfg, &store, "rmbg").unwrap().as_deref(),
            Some("1.4.0")
        );

        // 移除版本目录后激活项失效
        std::fs::remove_dir_all(dir.path().join("rmbg")).unwrap();
        assert!(active_version(&cfg, &store, "rmbg").unwrap().is_none());
    }

    #[test]
    fn 激活不存在的版本给出中文报错() {
        let (dir, mut cfg, store) = setup();
        cfg.general.models_dir = dir.path().to_string_lossy().to_string();
        let err = activate_version(&cfg, &store, "rmbg", "9.9.9")
            .unwrap_err()
            .to_string();
        assert!(err.contains("版本“9.9.9”不存在"), "实际：{err}");
    }

    #[test]
    fn 无版本目录时回退注册表路径() {
        let (dir, mut cfg, store) = setup();
        cfg.general.models_dir = dir.path().to_string_lossy().to_string();
        let expected = dir.path().join("rmbg-1.4.onnx");
        let got =
            resolve_model_path_versioned(&cfg, &store, "rmbg", Path::new(&cfg.models["rmbg"].path))
                .unwrap();
        assert_eq!(
            got, expected,
            "无版本目录应回退注册表路径（供上层判定缺失）"
        );
    }

    #[test]
    fn 校验时命中版本目录内的模型文件() {
        let (dir, mut cfg, store) = setup();
        cfg.general.models_dir = dir.path().to_string_lossy().to_string();
        let content = b"versioned-model";
        write_version(dir.path(), &cfg, "rmbg", "1.4.0", content);
        // 注册表给出该版本的 sha256
        let sha = expected_of(content);
        cfg.models.get_mut("rmbg").unwrap().sha256 = sha.clone();

        let statuses = check_models(&cfg, &store).unwrap();
        let rmbg = statuses.iter().find(|s| s.id == "rmbg").unwrap();
        assert_eq!(rmbg.check_status, CheckStatus::Ready);
        assert!(rmbg.path.to_string_lossy().contains("1.4.0"));
    }

    #[test]
    fn 解析路径按models_dir拼接并避免重复前缀() {
        let mut cfg = Config::default();
        // 默认 models_dir=models：注册表 models/x.onnx → models/x.onnx
        assert_eq!(
            resolve_model_path(&cfg, Path::new("models/retinaface_r50.onnx")),
            PathBuf::from("models/retinaface_r50.onnx")
        );
        // 仅文件名
        assert_eq!(
            resolve_model_path(&cfg, Path::new("retinaface_r50.onnx")),
            PathBuf::from("models/retinaface_r50.onnx")
        );
        // 绝对路径原样（用平台真实绝对路径，避免 Windows 下 /abs 不是绝对路径）
        let abs = std::env::temp_dir().join("photos_abs_model.onnx");
        assert!(abs.is_absolute());
        assert_eq!(resolve_model_path(&cfg, &abs), abs);

        // 自定义 models_dir：剥离历史 models/ 前缀后接到 mymodels
        cfg.general.models_dir = "mymodels".into();
        assert_eq!(
            resolve_model_path(&cfg, Path::new("models/retinaface_r50.onnx")),
            PathBuf::from("mymodels/retinaface_r50.onnx")
        );
        // 已写成 mymodels/ 前缀时不重复
        assert_eq!(
            resolve_model_path(&cfg, Path::new("mymodels/retinaface_r50.onnx")),
            PathBuf::from("mymodels/retinaface_r50.onnx")
        );
        // 无前缀文件名
        assert_eq!(
            resolve_model_path(&cfg, Path::new("retinaface_r50.onnx")),
            PathBuf::from("mymodels/retinaface_r50.onnx")
        );
    }

    #[test]
    fn 缺失模型状态() {
        let (_d, cfg, store) = setup();
        let mut cfg = cfg;
        // 指向不存在的路径
        cfg.models.get_mut("retinaface").unwrap().path = "models/不存在.onnx".into();
        let statuses = check_models(&cfg, &store).unwrap();
        let rf = statuses.iter().find(|s| s.id == "retinaface").unwrap();
        assert_eq!(rf.check_status, CheckStatus::Missing);
        assert!(rf.message.contains("放置"));
    }

    #[test]
    fn 文件就绪并写入缓存() {
        let (dir, cfg, store) = setup();
        let content = b"model-data";
        let path = write_file(dir.path(), "retinaface.onnx", content);
        let mut cfg = cfg;
        cfg.models.get_mut("retinaface").unwrap().path = path.display().to_string();
        cfg.models.get_mut("retinaface").unwrap().sha256 = expected_of(content);

        let s1 = check_models(&cfg, &store).unwrap();
        let rf = s1.iter().find(|s| s.id == "retinaface").unwrap();
        assert_eq!(rf.check_status, CheckStatus::Ready);

        // 再次校验 → 缓存命中
        let s2 = check_models(&cfg, &store).unwrap();
        let rf2 = s2.iter().find(|s| s.id == "retinaface").unwrap();
        assert_eq!(rf2.check_status, CheckStatus::CachedOk);
    }

    #[test]
    fn 文件变化导致哈希不一致() {
        let (dir, cfg, store) = setup();
        let content = b"old-data";
        let path = write_file(dir.path(), "rmbg.onnx", content);
        let mut cfg = cfg;
        cfg.models.get_mut("rmbg").unwrap().path = path.display().to_string();
        cfg.models.get_mut("rmbg").unwrap().sha256 = expected_of(content);
        check_models(&cfg, &store).unwrap();

        // 覆盖文件内容（mtime/size 变化），缓存失效 → 哈希不一致
        std::fs::write(&path, b"new-longer-data").unwrap();
        let s = check_models(&cfg, &store).unwrap();
        let rf = s.iter().find(|x| x.id == "rmbg").unwrap();
        assert_eq!(rf.check_status, CheckStatus::HashMismatch);
        assert!(rf.message.contains("不一致"));
    }

    #[test]
    fn 占位sha256视为未就绪() {
        let (dir, cfg, store) = setup();
        let content = b"any-content";
        let path = write_file(dir.path(), "modnet.onnx", content);
        let mut cfg = cfg;
        cfg.models.get_mut("modnet").unwrap().path = path.display().to_string();
        // sha256 保持占位全 0
        let s = check_models(&cfg, &store).unwrap();
        let rf = s.iter().find(|x| x.id == "modnet").unwrap();
        assert_eq!(rf.check_status, CheckStatus::HashMismatch);
        assert!(rf.message.contains("未定版"));
    }

    #[test]
    fn 禁用模型不参与校验() {
        let (_d, cfg, store) = setup();
        let mut cfg = cfg;
        cfg.models.get_mut("modnet").unwrap().enabled = false;
        cfg.models.get_mut("modnet").unwrap().path = "models/不存在.onnx".into();
        let s = check_models(&cfg, &store).unwrap();
        assert!(!s.iter().any(|x| x.id == "modnet"));
    }

    #[test]
    fn 就绪计数() {
        let (dir, cfg, store) = setup();
        let content = b"x";
        let path = write_file(dir.path(), "a.onnx", content);
        let mut cfg = cfg;
        // 让 retinaface 与 movnet_light 就绪，其余缺失
        for id in ["retinaface", "movnet_light"] {
            let spec = cfg.models.get_mut(id).unwrap();
            spec.path = path.display().to_string();
            spec.sha256 = expected_of(content);
        }
        let s = check_models(&cfg, &store).unwrap();
        assert_eq!(ready_count(&s), 2);
    }

    /// 起一个返回固定字节/状态码的 HTTP server（支持多次请求，覆盖下载重试），返回完整 URL
    fn start_http_server(content: &'static [u8], status: u16) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(8) {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    content.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(content);
            }
        });
        format!("http://{addr}/model.onnx")
    }

    /// 构造下载场景：本地 server + 目标路径/url/sha256 写入 retinaface 注册表
    fn download_cfg(dir: &Path, url: &str, expected: &str) -> Config {
        let mut cfg = Config::default();
        let spec = cfg.models.get_mut("retinaface").unwrap();
        spec.path = dir.join("retinaface.onnx").display().to_string();
        spec.sha256 = expected.to_string();
        spec.download = Some(crate::config::ModelDownload {
            url: Some(url.into()),
        });
        cfg
    }

    #[test]
    fn 下载成功并写入目标文件() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"model-bytes-download";
        let url = start_http_server(content, 200);
        let cfg = download_cfg(dir.path(), &url, &expected_of(content));
        download_model(&cfg, "retinaface").unwrap();
        let target = dir.path().join("retinaface.onnx");
        assert!(target.exists());
        assert_eq!(std::fs::read(&target).unwrap(), content);
        // 临时文件已清理
        assert!(!dir.path().join("retinaface.onnx.downloading").exists());
    }

    #[test]
    fn 下载校验sha256失败报错() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"model-bytes-download";
        let url = start_http_server(content, 200);
        let cfg = download_cfg(dir.path(), &url, &expected_of(b"other-content"));
        let err = download_model(&cfg, "retinaface").unwrap_err();
        assert!(err.to_string().contains("sha256 校验失败"), "实际：{err}");
        // 校验失败不落盘
        assert!(!dir.path().join("retinaface.onnx").exists());
    }

    #[test]
    fn 未配置下载地址报错() {
        let _dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.models.get_mut("retinaface").unwrap().download = None;
        let err = download_model(&cfg, "retinaface").unwrap_err();
        assert!(err.to_string().contains("未配置下载地址"), "实际：{err}");
    }

    #[test]
    fn 服务器非200报错() {
        let dir = tempfile::tempdir().unwrap();
        let url = start_http_server(b"not-found", 404);
        let cfg = download_cfg(dir.path(), &url, "");
        let err = download_model(&cfg, "retinaface").unwrap_err();
        assert!(err.to_string().contains("404"), "实际：{err}");
    }

    #[test]
    fn 自动下载缺失模型() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"auto-download-bytes";
        let url = start_http_server(content, 200);
        let cfg = download_cfg(dir.path(), &url, &expected_of(content));
        ensure_model_downloaded(&cfg, "retinaface").unwrap();
        let target = dir.path().join("retinaface.onnx");
        assert!(target.exists());
        assert_eq!(std::fs::read(&target).unwrap(), content);
    }

    #[test]
    fn 已存在模型跳过下载() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_file(dir.path(), "retinaface.onnx", b"existing");
        // 即使下载地址无效（未启动 server），已存在也应直接通过
        let cfg = download_cfg(dir.path(), "http://127.0.0.1:1/不存在.onnx", "");
        assert_eq!(
            std::fs::read(cfg.model_spec("retinaface").unwrap().path.as_str()).unwrap(),
            std::fs::read(&target).unwrap()
        );
        ensure_model_downloaded(&cfg, "retinaface").unwrap();
    }

    #[test]
    fn 无下载地址自动下载报错() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.models.get_mut("retinaface").unwrap().path =
            dir.path().join("retinaface.onnx").display().to_string();
        cfg.models.get_mut("retinaface").unwrap().download = None;
        let err = ensure_model_downloaded(&cfg, "retinaface").unwrap_err();
        assert!(err.to_string().contains("自动下载失败"), "实际：{err}");
    }
}
