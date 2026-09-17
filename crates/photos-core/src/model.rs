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

/// 校验全部启用模型，返回状态报告列表
pub fn check_models(cfg: &Config, store: &Store) -> CoreResult<Vec<ModelStatus>> {
    let mut out = Vec::new();
    for (id, spec) in &cfg.models {
        if !spec.enabled {
            continue;
        }
        out.push(check_one(cfg, store, id, spec.path.as_ref(), &spec.sha256)?);
    }
    Ok(out)
}

/// 校验单个模型文件
fn check_one(
    cfg: &Config,
    store: &Store,
    id: &str,
    spec_path: &Path,
    expected: &str,
) -> CoreResult<ModelStatus> {
    let path = resolve_model_path(cfg, spec_path);

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

/// 解析模型路径：绝对路径直接使用，相对路径以当前工作目录为基准
/// （注册表 path 如 `models/mtcnn.onnx` 已相对项目根，勿与 models_dir 拼接造成重复）
pub fn resolve_model_path(_cfg: &Config, spec_path: &Path) -> PathBuf {
    if spec_path.is_absolute() {
        spec_path.to_path_buf()
    } else {
        spec_path.to_path_buf()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;

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
}
