//! 人体关键点解析：MoveNet-Lightning/Thunder 输出（17 点，COCO 顺序）解码。
//! 重点取左右肩（索引 5/6）与双眼（索引 1/2）供姿态角度融合。

use crate::error::{CoreError, CoreResult};
use crate::inference::TensorData;
use crate::vision::geometry::Point2;

/// MoveNet 17 点索引（COCO 顺序）
pub const NOSE: usize = 0;
pub const LEFT_EYE: usize = 1;
pub const RIGHT_EYE: usize = 2;
pub const LEFT_EAR: usize = 3;
pub const RIGHT_EAR: usize = 4;
pub const LEFT_SHOULDER: usize = 5;
pub const RIGHT_SHOULDER: usize = 6;
pub const LEFT_HIP: usize = 11;
pub const RIGHT_HIP: usize = 12;
pub const LEFT_KNEE: usize = 13;
pub const RIGHT_KNEE: usize = 14;

/// 关键点置信度硬过滤阈值：低于该值视为完全不可用（置 None，不参与融合与降权）。
/// 高于该值但置信度偏低的点保留坐标，由融合阶段的置信度加权自动降权。
pub const SCORE_HARD_THRESHOLD: f32 = 0.2;

/// 关键点集合（低置信度点置 None；`scores` 与 `points` 一一对应，保留原始置信度供融合降权）
#[derive(Debug, Clone, PartialEq)]
pub struct KeypointSet {
    pub points: [Option<Point2>; 17],
    /// 各点原始置信度（[0,1]，硬过滤阈值以下的点为 0.0）
    pub scores: [f32; 17],
}

impl KeypointSet {
    pub fn point(&self, idx: usize) -> Option<Point2> {
        self.points.get(idx).copied().flatten()
    }

    /// 单点置信度（越界为 0.0）
    pub fn score(&self, idx: usize) -> f64 {
        self.scores.get(idx).copied().unwrap_or(0.0) as f64
    }

    /// 双眼置信度（两眼中较低者；任一眼缺失为 0）
    pub fn eyes_conf(&self) -> f64 {
        match (self.points[LEFT_EYE], self.points[RIGHT_EYE]) {
            (Some(_), Some(_)) => self.scores[LEFT_EYE].min(self.scores[RIGHT_EYE]) as f64,
            _ => 0.0,
        }
    }

    /// 双肩置信度（两肩中较低者；任一肩缺失为 0）
    pub fn shoulders_conf(&self) -> f64 {
        match (self.points[LEFT_SHOULDER], self.points[RIGHT_SHOULDER]) {
            (Some(_), Some(_)) => self.scores[LEFT_SHOULDER].min(self.scores[RIGHT_SHOULDER]) as f64,
            _ => 0.0,
        }
    }

    /// 双肩（左、右），任一缺失返回 None
    pub fn shoulders(&self) -> Option<(Point2, Point2)> {
        match (self.points[LEFT_SHOULDER], self.points[RIGHT_SHOULDER]) {
            (Some(l), Some(r)) => Some((l, r)),
            _ => None,
        }
    }

    /// 双眼（左、右），任一缺失返回 None
    pub fn eyes(&self) -> Option<(Point2, Point2)> {
        match (self.points[LEFT_EYE], self.points[RIGHT_EYE]) {
            (Some(l), Some(r)) => Some((l, r)),
            _ => None,
        }
    }

    /// 双髋（左、右），任一缺失返回 None（证件照常只拍上半身，缺失属正常）
    pub fn hips(&self) -> Option<(Point2, Point2)> {
        match (self.points[LEFT_HIP], self.points[RIGHT_HIP]) {
            (Some(l), Some(r)) => Some((l, r)),
            _ => None,
        }
    }

    /// 双膝（左、右），任一缺失返回 None（髋部不可用时的躯干垂直度兜底）
    pub fn knees(&self) -> Option<(Point2, Point2)> {
        match (self.points[LEFT_KNEE], self.points[RIGHT_KNEE]) {
            (Some(l), Some(r)) => Some((l, r)),
            _ => None,
        }
    }

    /// 下半身参考中点：优先双髋中点，缺失时退回双膝中点，均缺失返回 None
    pub fn lower_mid(&self) -> Option<Point2> {
        self.lower_ref().map(|(p, _)| p)
    }

    /// 下半身参考中点及置信度（与 [`Self::lower_mid`] 的髋→膝选择一致）
    pub fn lower_ref(&self) -> Option<(Point2, f64)> {
        let mid = |(a, b): (Point2, Point2), c: f64| {
            (
                Point2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0),
                c,
            )
        };
        match self.hips() {
            Some(h) => Some(mid(h, self.scores[LEFT_HIP].min(self.scores[RIGHT_HIP]) as f64)),
            None => self
                .knees()
                .map(|k| mid(k, self.scores[LEFT_KNEE].min(self.scores[RIGHT_KNEE]) as f64)),
        }
    }

    /// 双肩中点，双肩缺失返回 None
    pub fn shoulder_mid(&self) -> Option<Point2> {
        self.shoulders()
            .map(|(l, r)| Point2::new((l.x + r.x) / 2.0, (l.y + r.y) / 2.0))
    }
}

/// MoveNet 输出解码：接受形状尾两维为 `[17, 3]`（y, x, score）或 `[17, 2]`（y, x）；
/// 坐标归一化到 [0,1]，按原图尺寸还原；score 低于 [`SCORE_HARD_THRESHOLD`] 视为完全不可用（置 None），
/// 未达该阈值但仍保留坐标的点由融合阶段按置信度自动降权。
pub fn decode_movenet(tensor: &TensorData, img_w: u32, img_h: u32) -> CoreResult<KeypointSet> {
    let n = tensor.shape.len();
    let rows = tensor.dim(n - 2);
    let cols = tensor.dim(n - 1);
    if rows != 17 {
        return Err(CoreError::Image(format!(
            "关键点行数应为 17，实际 {rows}（形状 {:?}）",
            tensor.shape
        )));
    }
    if !(2..=3).contains(&cols) {
        return Err(CoreError::Image(format!(
            "关键点列数应为 2 或 3，实际 {cols}（形状 {:?}）",
            tensor.shape
        )));
    }
    let row_len = cols as usize;
    let mut points = [None; 17];
    let mut scores = [0.0f32; 17];
    for (i, (slot, score_slot)) in points.iter_mut().zip(scores.iter_mut()).enumerate() {
        let base = i * row_len;
        let y = tensor.data[base] as f64;
        let x = tensor.data[base + 1] as f64;
        let score = if cols == 3 {
            tensor.data[base + 2]
        } else {
            1.0
        };
        *score_slot = score;
        *slot = (score > SCORE_HARD_THRESHOLD)
            .then(|| Point2::new(x * img_w as f64, y * img_h as f64));
    }
    Ok(KeypointSet { points, scores })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 归一化坐标还原() {
        // 17 点，第 5 点（左肩）x=0.5,y=0.25、第 6 点（右肩）x=0.6,y=0.3，score 高
        let mut data = vec![0.0f32; 17 * 3];
        data[5 * 3] = 0.25; // y
        data[5 * 3 + 1] = 0.5; // x
        data[5 * 3 + 2] = 0.99;
        data[6 * 3] = 0.3; // y
        data[6 * 3 + 1] = 0.6; // x
        data[6 * 3 + 2] = 0.98;
        let t = TensorData::new(vec![1, 17, 3], data).unwrap();
        let ks = decode_movenet(&t, 200, 100).unwrap();
        let (l, r) = ks.shoulders().unwrap();
        assert!((l.x - 100.0).abs() < 1e-3 && (l.y - 25.0).abs() < 1e-3);
        assert!((r.x - 120.0).abs() < 1e-3 && (r.y - 30.0).abs() < 1e-3);
    }

    #[test]
    fn 低置信度置缺失() {
        let mut data = vec![0.0f32; 17 * 3];
        data[5 * 3 + 2] = 0.1; // 左肩低于硬阈值 → 完全缺失
        data[6 * 3 + 2] = 0.9;
        let t = TensorData::new(vec![17, 3], data).unwrap();
        let ks = decode_movenet(&t, 100, 100).unwrap();
        assert!(ks.shoulders().is_none()); // 左肩缺失
        assert!(ks.point(6).is_some());
        assert!(
            (ks.score(5) - 0.1).abs() < 1e-6,
            "原始置信度应保留供融合降权，实际 {}",
            ks.score(5)
        );
        assert!((ks.score(6) - 0.9).abs() < 1e-6);
        assert_eq!(ks.shoulders_conf(), 0.0, "缺失点所在路置信度为 0");
    }

    #[test]
    fn 低于硬阈值但高于过旧阈值的关键点保留() {
        // 0.25 高于新硬阈值 0.2、低于旧值 0.3：坐标保留，置信度经加权自动降权
        let mut data = vec![0.0f32; 17 * 3];
        data[5 * 3] = 0.5;
        data[5 * 3 + 1] = 0.5;
        data[5 * 3 + 2] = 0.25;
        data[6 * 3] = 0.5;
        data[6 * 3 + 1] = 0.6;
        data[6 * 3 + 2] = 0.95;
        let t = TensorData::new(vec![17, 3], data).unwrap();
        let ks = decode_movenet(&t, 100, 100).unwrap();
        assert!(ks.shoulders().is_some(), "0.25 应保留坐标");
        assert!(ks.shoulders_conf() < 0.3 && ks.shoulders_conf() > 0.24);
    }

    #[test]
    fn 两列布局默认高分() {
        let mut data = vec![0.0f32; 17 * 2];
        data[6 * 2] = 0.5;
        data[6 * 2 + 1] = 0.5;
        let t = TensorData::new(vec![1, 17, 2], data).unwrap();
        let ks = decode_movenet(&t, 100, 100).unwrap();
        assert!(ks.point(RIGHT_SHOULDER).is_some());
        assert_eq!(ks.score(RIGHT_SHOULDER), 1.0, "无置信度列时视为满分");
    }

    #[test]
    fn 形状非法报错() {
        let t = TensorData::new(vec![1, 16, 3], vec![0.0; 16 * 3]).unwrap();
        assert!(decode_movenet(&t, 100, 100).is_err());
        let t2 = TensorData::new(vec![1, 17, 4], vec![0.0; 17 * 4]).unwrap();
        assert!(decode_movenet(&t2, 100, 100).is_err());
    }

    #[test]
    fn 下半身中点优先髋部并可退回膝部() {
        let mut kps = KeypointSet {
            points: [None; 17],
            scores: [0.0; 17],
        };
        // 髋膝均缺失 → None
        assert!(kps.lower_mid().is_none());
        assert!(kps.lower_ref().is_none());
        // 仅双膝：退回膝中点，置信度取两膝较低者
        kps.points[LEFT_KNEE] = Some(Point2::new(80.0, 400.0));
        kps.points[RIGHT_KNEE] = Some(Point2::new(120.0, 400.0));
        kps.scores[LEFT_KNEE] = 0.8;
        kps.scores[RIGHT_KNEE] = 0.9;
        let by_knee = kps.lower_mid().unwrap();
        assert!((by_knee.x - 100.0).abs() < 1e-9 && (by_knee.y - 400.0).abs() < 1e-9);
        assert!(
            (kps.lower_ref().unwrap().1 - 0.8).abs() < 1e-6,
            "取较低置信度"
        );
        // 补上双髋：优先髋中点
        kps.points[LEFT_HIP] = Some(Point2::new(90.0, 300.0));
        kps.points[RIGHT_HIP] = Some(Point2::new(110.0, 300.0));
        kps.scores[LEFT_HIP] = 0.95;
        kps.scores[RIGHT_HIP] = 0.99;
        let by_hip = kps.lower_mid().unwrap();
        assert!((by_hip.x - 100.0).abs() < 1e-9 && (by_hip.y - 300.0).abs() < 1e-9);
        assert!((kps.lower_ref().unwrap().1 - 0.95).abs() < 1e-6);
        // 肩中点
        assert!(kps.shoulder_mid().is_none());
        kps.points[LEFT_SHOULDER] = Some(Point2::new(60.0, 100.0));
        kps.points[RIGHT_SHOULDER] = Some(Point2::new(140.0, 100.0));
        kps.scores[LEFT_SHOULDER] = 0.7;
        kps.scores[RIGHT_SHOULDER] = 0.7;
        let sm = kps.shoulder_mid().unwrap();
        assert!((sm.x - 100.0).abs() < 1e-9 && (sm.y - 100.0).abs() < 1e-9);
        // 路置信度：眼/肩取较低者，缺失为 0
        assert_eq!(kps.eyes_conf(), 0.0);
        assert!((kps.shoulders_conf() - 0.7).abs() < 1e-6);
        kps.points[LEFT_EYE] = Some(Point2::new(80.0, 60.0));
        kps.points[RIGHT_EYE] = Some(Point2::new(120.0, 60.0));
        kps.scores[LEFT_EYE] = 0.6;
        kps.scores[RIGHT_EYE] = 0.88;
        assert!((kps.eyes_conf() - 0.6).abs() < 1e-6);
    }
}
