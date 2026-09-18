//! 性能指标：流水线分阶段耗时采集与聚合（可观测性基础设施）。
//!
//! 不引入第三方指标库，仅用 `std::time::Instant` + serde：
//! - [`TaskMetrics`]：单任务各阶段耗时，随任务落库（`task_history.metrics`）
//! - [`StageTimer`]：阶段计时器，`start` 后 `stop` 写入指标
//! - [`aggregate`]：把多条任务的指标聚合成各阶段平均耗时（供 `GET /metrics`）

use std::collections::BTreeMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// 单阶段耗时（毫秒）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageMetric {
    /// 阶段中文名（如「人脸检测」）
    pub stage: String,
    /// 阶段耗时（毫秒）
    pub ms: f64,
}

/// 单任务分阶段耗时指标（空表示未采集）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskMetrics {
    /// 按执行顺序记录的阶段耗时
    #[serde(default)]
    pub stages: Vec<StageMetric>,
}

impl TaskMetrics {
    /// 新建空指标
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否未采集到任何阶段
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// 追加一个阶段耗时
    pub fn add(&mut self, stage: impl Into<String>, ms: f64) {
        self.stages.push(StageMetric {
            stage: stage.into(),
            ms,
        });
    }

    /// 各阶段耗时之和（毫秒）
    pub fn total_ms(&self) -> f64 {
        self.stages.iter().map(|s| s.ms).sum()
    }

    /// 最后一个已记录阶段名（失败上报时用于定位阶段）
    pub fn last_stage(&self) -> Option<&str> {
        self.stages.last().map(|s| s.stage.as_str())
    }

    /// 中文摘要：`读图 12.0ms · 人脸检测 88.3ms`（保留一位小数）
    pub fn summary(&self) -> String {
        self.stages
            .iter()
            .map(|s| format!("{} {:.1}ms", s.stage, s.ms))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// 序列化为 JSON（供落库；失败时返回空指标 JSON）
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"stages":[]}"#.to_string())
    }

    /// 从 JSON 反序列化（解析失败或空串返回空指标）
    pub fn from_json(s: &str) -> Self {
        if s.trim().is_empty() {
            return Self::default();
        }
        serde_json::from_str(s).unwrap_or_default()
    }
}

/// 阶段计时器：`let t = StageTimer::start("读图"); …; t.stop(&mut metrics);`
pub struct StageTimer {
    stage: &'static str,
    started: Instant,
}

impl StageTimer {
    /// 开始计时
    pub fn start(stage: &'static str) -> Self {
        Self {
            stage,
            started: Instant::now(),
        }
    }

    /// 结束计时并写入指标（毫秒，保留小数）
    pub fn stop(self, metrics: &mut TaskMetrics) {
        metrics.add(self.stage, self.started.elapsed().as_secs_f64() * 1000.0);
    }
}

/// 阶段聚合结果（各阶段平均耗时）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageAggregate {
    /// 阶段中文名
    pub stage: String,
    /// 平均耗时（毫秒）
    pub avg_ms: f64,
    /// 参与统计的样本数
    pub samples: usize,
}

/// 把多条任务的指标聚合成各阶段平均耗时（按阶段名排序，样本为 0 返回空）
pub fn aggregate(metrics: &[TaskMetrics]) -> Vec<StageAggregate> {
    // BTreeMap 保证输出顺序稳定（按阶段名排序）
    let mut acc: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for m in metrics {
        for s in &m.stages {
            let e = acc.entry(s.stage.clone()).or_insert((0.0, 0));
            e.0 += s.ms;
            e.1 += 1;
        }
    }
    acc.into_iter()
        .map(|(stage, (sum, samples))| StageAggregate {
            stage,
            avg_ms: sum / samples as f64,
            samples,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 阶段计时与汇总() {
        let mut m = TaskMetrics::new();
        assert!(m.is_empty());
        let t = StageTimer::start("读图");
        std::thread::sleep(std::time::Duration::from_millis(2));
        t.stop(&mut m);
        m.add("人脸检测", 88.25);
        assert_eq!(m.stages.len(), 2);
        assert_eq!(m.stages[1].stage, "人脸检测");
        assert!(m.stages[0].ms > 0.0, "计时器应记录正耗时");
        assert_eq!(m.total_ms(), m.stages[0].ms + 88.25);
        assert_eq!(m.last_stage(), Some("人脸检测"));
        assert_eq!(
            m.summary(),
            format!("读图 {:.1}ms · 人脸检测 {:.1}ms", m.stages[0].ms, 88.25)
        );
    }

    #[test]
    fn 指标序列化往返() {
        let mut m = TaskMetrics::new();
        m.add("人像抠图", 120.5);
        let back = TaskMetrics::from_json(&m.to_json());
        assert_eq!(back, m);
        // 空串与非法 JSON 均降级为空指标
        assert!(TaskMetrics::from_json("").is_empty());
        assert!(TaskMetrics::from_json("{不是 JSON").is_empty());
    }

    #[test]
    fn 多任务阶段聚合() {
        let mut a = TaskMetrics::new();
        a.add("读图", 10.0);
        a.add("人脸检测", 20.0);
        let mut b = TaskMetrics::new();
        b.add("读图", 30.0);
        let agg = aggregate(&[a, b]);
        assert_eq!(agg.len(), 2);
        // 按阶段名排序：人脸检测 < 读图（UTF-8 字节序）
        let read = agg.iter().find(|s| s.stage == "读图").unwrap();
        assert_eq!(read.samples, 2);
        assert!((read.avg_ms - 20.0).abs() < 1e-9);
        let face = agg.iter().find(|s| s.stage == "人脸检测").unwrap();
        assert_eq!(face.samples, 1);
        assert!((face.avg_ms - 20.0).abs() < 1e-9);
        assert!(aggregate(&[]).is_empty());
    }
}
