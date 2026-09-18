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

/// 关键点集合（低置信度点置 None）
#[derive(Debug, Clone, PartialEq)]
pub struct KeypointSet {
    pub points: [Option<Point2>; 17],
}

impl KeypointSet {
    pub fn point(&self, idx: usize) -> Option<Point2> {
        self.points.get(idx).copied().flatten()
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
        let mid = |(a, b): (Point2, Point2)| Point2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        self.hips().or_else(|| self.knees()).map(mid)
    }

    /// 双肩中点，双肩缺失返回 None
    pub fn shoulder_mid(&self) -> Option<Point2> {
        self.shoulders()
            .map(|(l, r)| Point2::new((l.x + r.x) / 2.0, (l.y + r.y) / 2.0))
    }
}

/// MoveNet 输出解码：接受形状尾两维为 `[17, 3]`（y, x, score）或 `[17, 2]`（y, x）；
/// 坐标归一化到 [0,1]，按原图尺寸还原；score 低于阈值（默认 0.3）视为缺失。
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
    for (i, slot) in points.iter_mut().enumerate() {
        let base = i * row_len;
        let y = tensor.data[base] as f64;
        let x = tensor.data[base + 1] as f64;
        let score = if cols == 3 {
            tensor.data[base + 2]
        } else {
            1.0
        };
        *slot = (score > 0.3).then(|| Point2::new(x * img_w as f64, y * img_h as f64));
    }
    Ok(KeypointSet { points })
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
        data[5 * 3 + 2] = 0.1; // 左肩低置信
        data[6 * 3 + 2] = 0.9;
        let t = TensorData::new(vec![17, 3], data).unwrap();
        let ks = decode_movenet(&t, 100, 100).unwrap();
        assert!(ks.shoulders().is_none()); // 左肩缺失
        assert!(ks.point(6).is_some());
    }

    #[test]
    fn 两列布局默认高分() {
        let mut data = vec![0.0f32; 17 * 2];
        data[6 * 2] = 0.5;
        data[6 * 2 + 1] = 0.5;
        let t = TensorData::new(vec![1, 17, 2], data).unwrap();
        let ks = decode_movenet(&t, 100, 100).unwrap();
        assert!(ks.point(RIGHT_SHOULDER).is_some());
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
        let mut kps = KeypointSet { points: [None; 17] };
        // 髋膝均缺失 → None
        assert!(kps.lower_mid().is_none());
        // 仅双膝：退回膝中点
        kps.points[LEFT_KNEE] = Some(Point2::new(80.0, 400.0));
        kps.points[RIGHT_KNEE] = Some(Point2::new(120.0, 400.0));
        let by_knee = kps.lower_mid().unwrap();
        assert!((by_knee.x - 100.0).abs() < 1e-9 && (by_knee.y - 400.0).abs() < 1e-9);
        // 补上双髋：优先髋中点
        kps.points[LEFT_HIP] = Some(Point2::new(90.0, 300.0));
        kps.points[RIGHT_HIP] = Some(Point2::new(110.0, 300.0));
        let by_hip = kps.lower_mid().unwrap();
        assert!((by_hip.x - 100.0).abs() < 1e-9 && (by_hip.y - 300.0).abs() < 1e-9);
        // 肩中点
        assert!(kps.shoulder_mid().is_none());
        kps.points[LEFT_SHOULDER] = Some(Point2::new(60.0, 100.0));
        kps.points[RIGHT_SHOULDER] = Some(Point2::new(140.0, 100.0));
        let sm = kps.shoulder_mid().unwrap();
        assert!((sm.x - 100.0).abs() < 1e-9 && (sm.y - 100.0).abs() < 1e-9);
    }
}
