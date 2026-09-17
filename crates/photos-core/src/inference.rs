//! 推理抽象层：统一张量输入输出；`FakeEngine` 用于测试与链路打通（模型未就位时给出缺模型错误），
//! `OrtEngine`（feature = "ort"）为 ONNX Runtime 真后端（模型定版后就位）。

use std::collections::{HashMap, HashSet};
use std::path::Path;

#[cfg(feature = "ort")]
use std::path::PathBuf;

use crate::config::Config;
use crate::error::{CoreError, CoreResult};

/// 浮点张量（f32，按行主序连续存储）
#[derive(Debug, Clone, PartialEq)]
pub struct TensorData {
    /// 形状（如 `[1,3,640,640]`）
    pub shape: Vec<i64>,
    /// 连续数据，长度 = shape 元素数
    pub data: Vec<f32>,
}

impl TensorData {
    /// 构造张量（校验元素数与形状一致）
    pub fn new(shape: Vec<i64>, data: Vec<f32>) -> CoreResult<Self> {
        let expected: i64 = shape.iter().product();
        if expected as usize != data.len() {
            return Err(CoreError::Inference(format!(
                "张量形状 {shape:?} 与数据长度 {} 不一致",
                data.len()
            )));
        }
        Ok(Self { shape, data })
    }

    /// 元素数
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// 取指定维大小（越界返回 1）
    pub fn dim(&self, axis: usize) -> i64 {
        self.shape.get(axis).copied().unwrap_or(1)
    }
}

/// 推理引擎抽象
pub trait InferenceEngine: Send + Sync {
    /// 装载模型（惰性；模型缺失/校验失败返回中文错误）
    fn load(&mut self, cfg: &Config, model_id: &str) -> CoreResult<()>;
    /// 执行推理，返回输出张量列表
    fn run(&self, model_id: &str, input: &TensorData) -> CoreResult<Vec<TensorData>>;
}

/// 模拟推理引擎：回放固定输出张量，用于测试与链路打通。
/// 未 stub 的模型在 load 时校验磁盘文件，缺失给出中文指引。
#[derive(Debug, Default)]
pub struct FakeEngine {
    responses: HashMap<String, Vec<TensorData>>,
    /// stub 模型视为已就绪（跳过磁盘校验）
    stubbed: HashSet<String>,
}

impl FakeEngine {
    /// 空引擎（所有模型视为缺失）
    pub fn new() -> Self {
        Self::default()
    }

    /// 预置某模型的固定输出（回放）
    pub fn stub(mut self, model_id: &str, outputs: Vec<TensorData>) -> Self {
        self.responses.insert(model_id.to_string(), outputs);
        self.stubbed.insert(model_id.to_string());
        self
    }

    /// 构建一个「balanced 三件套」均已 stub 的引擎（便捷测试工具）
    pub fn balanced_stub(face_out: Vec<TensorData>, keypoint_out: Vec<TensorData>, matting_out: Vec<TensorData>) -> Self {
        Self::new()
            .stub("retinaface", face_out)
            .stub("movnet_light", keypoint_out)
            .stub("birefnet_lite", matting_out)
    }
}

impl InferenceEngine for FakeEngine {
    fn load(&mut self, cfg: &Config, model_id: &str) -> CoreResult<()> {
        if self.stubbed.contains(model_id) {
            return Ok(());
        }
        // 未 stub：按磁盘校验（复用模型管理逻辑）
        let spec = cfg.model_spec(model_id)?;
        let path = crate::model::resolve_model_path(cfg, Path::new(&spec.path));
        if !path.exists() {
            return Err(CoreError::Model(format!(
                "模型“{model_id}”缺失：{}。请按 docs/04-模型清单.md §6 放置模型或使用一键下载",
                path.display()
            )));
        }
        // 简单校验通过后视为可装载（完整 sha256 校验见 photos models）
        Ok(())
    }

    fn run(&self, model_id: &str, input: &TensorData) -> CoreResult<Vec<TensorData>> {
        let _ = input;
        self.responses.get(model_id).cloned().ok_or_else(|| {
            CoreError::Inference(format!("模型“{model_id}”无可用推理输出（未装载或未 stub）"))
        })
    }
}

/// ONNX Runtime 真后端（feature = "ort" 时启用；API 以 ort 2.0 候选版为准，模型定版后校准）
#[cfg(feature = "ort")]
pub struct OrtEngine {
    sessions: HashMap<String, ort::session::Session>,
}

#[cfg(feature = "ort")]
impl OrtEngine {
    /// 新建空引擎
    pub fn new() -> Self {
        Self { sessions: HashMap::new() }
    }

    fn model_path(cfg: &Config, model_id: &str) -> CoreResult<PathBuf> {
        let spec = cfg.model_spec(model_id)?;
        Ok(crate::model::resolve_model_path(cfg, Path::new(&spec.path)))
    }
}

#[cfg(feature = "ort")]
impl InferenceEngine for OrtEngine {
    fn load(&mut self, cfg: &Config, model_id: &str) -> CoreResult<()> {
        if self.sessions.contains_key(model_id) {
            return Ok(());
        }
        // 装载前先做磁盘存在性校验
        let path = Self::model_path(cfg, model_id)?;
        if !path.exists() {
            return Err(CoreError::Model(format!(
                "模型“{model_id}”缺失：{}。请按 docs/04-模型清单.md §6 放置模型或使用一键下载",
                path.display()
            )));
        }
        let session = ort::session::Session::builder()
            .map_err(|e| CoreError::Inference(format!("创建推理会话失败：{e}")))?
            .commit_from_file(&path)
            .map_err(|e| CoreError::Inference(format!("装载模型 {} 失败：{e}", path.display())))?
            .into_session();
        self.sessions.insert(model_id.to_string(), session);
        Ok(())
    }

    fn run(&self, model_id: &str, input: &TensorData) -> CoreResult<Vec<TensorData>> {
        let session = self.sessions.get(model_id).ok_or_else(|| {
            CoreError::Inference(format!("模型“{model_id}”未装载"))
        })?;
        let value = ort::value::Value::from_array(
            input
                .data
                .iter()
                .copied()
                .collect::<Vec<f32>>()
                .into_shape(input.shape.iter().map(|&d| d as usize).collect::<Vec<_>>()),
        )
        .map_err(|e| CoreError::Inference(format!("输入张量转换失败：{e}")))?;
        let outputs = session
            .run(ort::inputs![value].map_err(|e| CoreError::Inference(format!("构建推理输入失败：{e}")))?)
            .map_err(|e| CoreError::Inference(format!("推理失败：{e}")))?;
        let mut result = Vec::new();
        for output in outputs {
            let arr = output
                .try_extract_array::<f32>()
                .map_err(|e| CoreError::Inference(format!("输出张量解析失败：{e}")))?;
            let shape: Vec<i64> = arr.shape().iter().map(|&d| d as i64).collect();
            let data: Vec<f32> = arr.iter().copied().collect();
            result.push(TensorData { shape, data });
        }
        Ok(result)
    }
}

/// 根据 feature 构建默认引擎（CLI 入口使用）
pub fn default_engine() -> Box<dyn InferenceEngine> {
    #[cfg(feature = "ort")]
    {
        Box::new(OrtEngine::new())
    }
    #[cfg(not(feature = "ort"))]
    {
        Box::new(FakeEngine::new())
    }
}

/// 便捷：校验某模式所需模型全部就绪（缺模型时报中文指引）
pub fn ensure_models_ready(cfg: &Config, engine: &mut dyn InferenceEngine, mode_id: &str) -> CoreResult<()> {
    let suite = cfg.mode(mode_id)?;
    for id in [&suite.face, &suite.keypoint, &suite.matting] {
        engine.load(cfg, id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 张量构造与长度校验() {
        let t = TensorData::new(vec![1, 2, 3], vec![0.0; 6]).unwrap();
        assert_eq!(t.dim(1), 2);
        assert_eq!(t.dim(5), 1);
        assert!(TensorData::new(vec![1, 2], vec![0.0; 3]).is_err());
    }

    #[test]
    fn fake引擎回放与缺模型错误() {
        let engine = FakeEngine::new().stub("retinaface", vec![TensorData::new(vec![1, 2], vec![1.0, 2.0]).unwrap()]);
        let out = engine.run("retinaface", &TensorData::new(vec![1], vec![0.0]).unwrap()).unwrap();
        assert_eq!(out[0].data, vec![1.0, 2.0]);
        let err = engine.run("movnet_light", &TensorData::new(vec![1], vec![0.0]).unwrap()).unwrap_err();
        assert!(err.to_string().contains("movnet_light"));
    }

    #[test]
    fn fake引擎未stub模型按磁盘校验() {
        let mut engine = FakeEngine::new();
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.models.get_mut("mtcnn").unwrap().path = dir.path().join("mtcnn.onnx").display().to_string();
        // 文件不存在 → 缺模型错误
        let err = engine.load(&cfg, "mtcnn").unwrap_err();
        assert!(err.to_string().contains("缺失"));
        // 放置文件 → 通过
        std::fs::write(dir.path().join("mtcnn.onnx"), b"onnx").unwrap();
        engine.load(&cfg, "mtcnn").unwrap();
    }
}
