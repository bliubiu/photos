//! 人脸检测后处理：RetinaFace/MTCNN 输出解码（bbox + 5 关键点）、NMS、坐标还原。
//! 纯 Rust 实现；解码采用通用布局约定，模型定版后按实际输出校准。

use std::cmp::Ordering;

use crate::error::{CoreError, CoreResult};
use crate::inference::TensorData;
use crate::vision::geometry::Point2;

/// 人脸框（模型坐标，`x2/y2` 含边界）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
}

impl FaceBox {
    pub fn width(&self) -> f32 {
        self.x2 - self.x1
    }
    pub fn height(&self) -> f32 {
        self.y2 - self.y1
    }
    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }
    /// 人脸框中心（旋转中心使用）
    pub fn center(&self) -> Point2 {
        Point2::new(((self.x1 + self.x2) / 2.0) as f64, ((self.y1 + self.y2) / 2.0) as f64)
    }
}

/// 人脸检测结果：框 + 5 关键点（左眼、右眼、鼻尖、左嘴角、右嘴角）
#[derive(Debug, Clone, PartialEq)]
pub struct FaceDetection {
    pub face: FaceBox,
    pub landmarks: [Point2; 5],
}

/// 交并比（IoU）
pub fn iou(a: &FaceBox, b: &FaceBox) -> f32 {
    let inter_w = (a.x2.min(b.x2) - a.x1.max(b.x1)).max(0.0);
    let inter_h = (a.y2.min(b.y2) - a.y1.max(b.y1)).max(0.0);
    let inter = inter_w * inter_h;
    let union = a.area() + b.area() - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 贪心 NMS：按分数降序，抑制与已选框 IoU 超过阈值的框，返回保留索引
pub fn nms(boxes: &[FaceBox], iou_threshold: f32) -> Vec<usize> {
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&i, &j| boxes[j].score.partial_cmp(&boxes[i].score).unwrap_or(Ordering::Equal));
    let mut keep = Vec::new();
    let mut suppressed = vec![false; boxes.len()];
    for &i in &order {
        if suppressed[i] {
            continue;
        }
        keep.push(i);
        for &j in &order {
            if suppressed[j] || j == i {
                continue;
            }
            if iou(&boxes[i], &boxes[j]) > iou_threshold {
                suppressed[j] = true;
            }
        }
    }
    keep
}

/// RetinaFace 输出解码（通用布局约定）：
/// - scores 张量 `[1, N]`（或 `[N]`）：各候选分数
/// - boxes 张量 `[1, N, 4]`（或 `[N, 4]`）：`[x1, y1, x2, y2]`（模型坐标）
/// - landmarks 张量 `[1, N, 10]`（或 `[N, 10]`）：5 点 `[x,y]` 对（左眼、右眼、鼻尖、左嘴角、右嘴角）
/// 流程：低分过滤 → NMS → 坐标还原（scale_x/scale_y 与 pad 提供 letterbox 逆变换，缺省为 1/0）。
pub fn decode_retinaface(
    scores: &TensorData,
    boxes: &TensorData,
    landmarks: &TensorData,
    score_threshold: f32,
    iou_threshold: f32,
    scale_x: f32,
    scale_y: f32,
    pad_x: f32,
    pad_y: f32,
) -> CoreResult<Vec<FaceDetection>> {
    let n = scores.data.len();
    if boxes.data.len() != n * 4 {
        return Err(CoreError::Image(format!(
            "检测框张量长度 {} 与候选数 {n} 不一致",
            boxes.data.len()
        )));
    }
    if landmarks.data.len() != n * 10 {
        return Err(CoreError::Image(format!(
            "关键点张量长度 {} 与候选数 {n} 不一致",
            landmarks.data.len()
        )));
    }

    let mut detections: Vec<FaceDetection> = Vec::new();
    for i in 0..n {
        let score = scores.data[i];
        if score < score_threshold {
            continue;
        }
        let b = &boxes.data[i * 4..i * 4 + 4];
        let lm = &landmarks.data[i * 10..i * 10 + 10];
        let mut points = [Point2::new(0.0, 0.0); 5];
        for (k, p) in points.iter_mut().enumerate() {
            *p = Point2::new(lm[k * 2] as f64, lm[k * 2 + 1] as f64);
        }
        detections.push(FaceDetection {
            face: FaceBox { x1: b[0], y1: b[1], x2: b[2], y2: b[3], score },
            landmarks: points,
        });
    }

    let keep = nms(&detections.iter().map(|d| d.face).collect::<Vec<_>>(), iou_threshold);
    let mut out = Vec::new();
    for &idx in &keep {
        let mut d = detections[idx].clone();
        d.face.x1 = (d.face.x1 - pad_x) / scale_x;
        d.face.y1 = (d.face.y1 - pad_y) / scale_y;
        d.face.x2 = (d.face.x2 - pad_x) / scale_x;
        d.face.y2 = (d.face.y2 - pad_y) / scale_y;
        for p in &mut d.landmarks {
            p.x = (p.x - pad_x as f64) / scale_x as f64;
            p.y = (p.y - pad_y as f64) / scale_y as f64;
        }
        out.push(d);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_(x1: f32, y1: f32, x2: f32, y2: f32, s: f32) -> FaceBox {
        FaceBox { x1, y1, x2, y2, score: s }
    }

    #[test]
    fn iou计算() {
        let a = box_(0.0, 0.0, 10.0, 10.0, 0.9);
        let b = box_(5.0, 0.0, 15.0, 10.0, 0.9); // 半重叠
        let v = iou(&a, &b);
        assert!((v - 50.0 / 150.0).abs() < 1e-4, "实际 {v}");
        assert_eq!(iou(&a, &box_(100.0, 100.0, 110.0, 110.0, 0.5)), 0.0);
        assert_eq!(iou(&a, &a), 1.0);
    }

    #[test]
    fn nms抑制重叠低分框() {
        let boxes = vec![
            box_(0.0, 0.0, 10.0, 10.0, 0.9),
            box_(1.0, 1.0, 11.0, 11.0, 0.5), // 高 IoU 低分 → 抑制
            box_(50.0, 50.0, 60.0, 60.0, 0.8), // 独立 → 保留
            box_(0.0, 0.0, 10.0, 10.0, 0.95), // 高 IoU 更高分 → 覆盖第一个
        ];
        let keep = nms(&boxes, 0.5);
        assert_eq!(keep, vec![3, 2]);
    }

    #[test]
    fn 解码过滤nms与坐标还原() {
        // 4 个候选：高分 A、低分（过滤）、高分 C、与 A 重叠高分（NMS 抑制）
        let scores = TensorData::new(vec![4], vec![0.9, 0.05, 0.8, 0.85]).unwrap();
        let boxes = TensorData::new(
            vec![4, 4],
            vec![
                10.0, 10.0, 40.0, 60.0, // A
                0.0, 0.0, 10.0, 10.0, // 低分
                100.0, 100.0, 140.0, 150.0, // C
                12.0, 12.0, 42.0, 62.0, // 与 A 重叠
            ],
        )
        .unwrap();
        let landmarks = TensorData::new(
            vec![4, 10],
            vec![
                15.0, 20.0, 35.0, 20.0, 25.0, 30.0, 20.0, 45.0, 30.0, 45.0, // A
                0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                110.0, 120.0, 130.0, 120.0, 120.0, 130.0, 115.0, 140.0, 125.0, 140.0, // C
                17.0, 22.0, 37.0, 22.0, 27.0, 32.0, 22.0, 47.0, 32.0, 47.0,
            ],
        )
        .unwrap();
        let dets = decode_retinaface(&scores, &boxes, &landmarks, 0.5, 0.5, 2.0, 2.0, 10.0, 20.0).unwrap();
        // 保留 A（去重叠）与 C
        assert_eq!(dets.len(), 2);
        // A 坐标还原：x = (10-10)/2 = 0
        let a = &dets[0];
        assert!((a.face.x1 - 0.0).abs() < 1e-3);
        assert!((a.face.y1 - -5.0).abs() < 1e-3); // (10-20)/2
        // 关键点还原
        assert!((a.landmarks[0].x - 2.5).abs() < 1e-3); // (15-10)/2
        assert!((a.landmarks[1].y - 0.0).abs() < 1e-3); // (20-20)/2
    }

    #[test]
    fn 张量长度校验() {
        let s = TensorData::new(vec![2], vec![0.9, 0.8]).unwrap();
        let b = TensorData::new(vec![2, 4], vec![0.0; 8]).unwrap();
        let lm = TensorData::new(vec![2, 10], vec![0.0; 20]).unwrap();
        assert!(decode_retinaface(&s, &TensorData::new(vec![1, 4], vec![0.0; 4]).unwrap(), &lm, 0.5, 0.5, 1.0, 1.0, 0.0, 0.0).is_err());
        assert!(decode_retinaface(&s, &b, &TensorData::new(vec![1, 10], vec![0.0; 10]).unwrap(), 0.5, 0.5, 1.0, 1.0, 0.0, 0.0).is_err());
    }
}
