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
        Point2::new(
            ((self.x1 + self.x2) / 2.0) as f64,
            ((self.y1 + self.y2) / 2.0) as f64,
        )
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
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// 贪心 NMS：按分数降序，抑制与已选框 IoU 超过阈值的框，返回保留索引
pub fn nms(boxes: &[FaceBox], iou_threshold: f32) -> Vec<usize> {
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&i, &j| {
        boxes[j]
            .score
            .partial_cmp(&boxes[i].score)
            .unwrap_or(Ordering::Equal)
    });
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

/// RetinaFace R50 默认锚框配置（对齐 Hivision inference.py 的 cfg）
const RF_MIN_SIZES: [[f32; 2]; 3] = [[16.0, 32.0], [64.0, 128.0], [256.0, 512.0]];
const RF_STEPS: [f32; 3] = [8.0, 16.0, 32.0];
const RF_VARIANCE: [f32; 2] = [0.1, 0.2];

/// 生成 RetinaFace prior（对齐 Hivision prior_box.py）：中心 x/y 相对特征图尺寸归一化
/// [0,1]，宽/高相对输入图像尺寸归一化（min_size / image_w|h）
fn retinaface_priors(image_h: u32, image_w: u32) -> Vec<[f32; 4]> {
    let mut priors = Vec::new();
    for (idx, step) in RF_STEPS.iter().enumerate() {
        let fh = (image_h as f32 / step).ceil() as u32;
        let fw = (image_w as f32 / step).ceil() as u32;
        for y in 0..fh {
            for x in 0..fw {
                for &min_size in &RF_MIN_SIZES[idx] {
                    let s_kx = min_size / image_w as f32;
                    let s_ky = min_size / image_h as f32;
                    priors.push([
                        (x as f32 + 0.5) / fw as f32,
                        (y as f32 + 0.5) / fh as f32,
                        s_kx,
                        s_ky,
                    ]);
                }
            }
        }
    }
    priors
}

/// RetinaFace prior 候选总数（供测试/演示引擎构造完整输出）
pub fn retinaface_prior_count(image_size: (u32, u32)) -> usize {
    retinaface_priors(image_size.0, image_size.1).len()
}

/// SSD 式解码（对齐 Hivision box_utils.decode）：prior 中心 + loc 偏移（variance[0]=0.1），
/// 宽高 exp 缩放（variance[1]=0.2），输出 [x1, y1, x2, y2] 归一化坐标
fn decode_box(loc: &[f32], prior: &[f32; 4]) -> [f32; 4] {
    let cx = prior[0] + loc[0] * RF_VARIANCE[0] * prior[2];
    let cy = prior[1] + loc[1] * RF_VARIANCE[0] * prior[3];
    let w = prior[2] * (loc[2] * RF_VARIANCE[1]).exp();
    let h = prior[3] * (loc[3] * RF_VARIANCE[1]).exp();
    [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0]
}

/// 5 点关键点解码（对齐 Hivision box_utils.decode_landm）：5 点均为中心偏移公式
fn decode_landm(lm: &[f32], prior: &[f32; 4]) -> [f32; 10] {
    let mut out = [0.0f32; 10];
    for k in 0..5 {
        out[2 * k] = prior[0] + lm[2 * k] * RF_VARIANCE[0] * prior[2];
        out[2 * k + 1] = prior[1] + lm[2 * k + 1] * RF_VARIANCE[0] * prior[3];
    }
    out
}

/// RetinaFace 输出解码（Hivision 官方模型约定）：
/// - scores 张量 `[1, N]`（或 `[N]`，或真实模型 `[1, N, 2]` 取人脸分数列）：各候选分数
/// - boxes 张量 `[1, N, 4]`（或 `[N, 4]`）：prior 回归偏移（Hivision 官方模型的 loc）
/// - landmarks 张量 `[1, N, 10]`（或 `[N, 10]`）：5 点偏移（左眼、右眼、鼻尖、左嘴角、右嘴角）
/// 流程：prior 解码 → 低分过滤 → NMS → 坐标还原（scale_x/scale_y 与 pad 提供 letterbox 逆变换）。
pub fn decode_retinaface(
    scores: &TensorData,
    boxes: &TensorData,
    landmarks: &TensorData,
    score_threshold: f32,
    iou_threshold: f32,
    image_size: (u32, u32),
    scale_x: f32,
    scale_y: f32,
    pad_x: f32,
    pad_y: f32,
) -> CoreResult<Vec<FaceDetection>> {
    if boxes.data.len() % 4 != 0 {
        return Err(CoreError::Image(format!(
            "检测框张量长度 {} 不是 4 的倍数",
            boxes.data.len()
        )));
    }
    let n = boxes.data.len() / 4;
    if n == 0 {
        return Err(CoreError::Image("检测框张量为空".into()));
    }
    // scores 支持 [N] 或 [N, 2]（真实 RetinaFace 输出两列，人脸分数在最后一列）
    let score_stride = scores.data.len() / n;
    if scores.data.len() != n * score_stride || !(1..=2).contains(&score_stride) {
        return Err(CoreError::Image(format!(
            "分数张量长度 {} 与候选数 {n} 不一致（应为 1 或 2 倍）",
            scores.data.len()
        )));
    }
    if landmarks.data.len() != n * 10 {
        return Err(CoreError::Image(format!(
            "关键点张量长度 {} 与候选数 {n} 不一致",
            landmarks.data.len()
        )));
    }
    // prior 数量必须与候选数一致，否则说明模型输出或配置不匹配
    let priors = retinaface_priors(image_size.0, image_size.1);
    if priors.len() != n {
        return Err(CoreError::Image(format!(
            "候选数 {n} 与锚框数 {} 不一致（模型或输入尺寸不匹配）",
            priors.len()
        )));
    }
    let (ih, iw) = (image_size.0 as f32, image_size.1 as f32);

    let mut detections: Vec<FaceDetection> = Vec::new();
    for i in 0..n {
        let score = scores.data[i * score_stride + score_stride - 1];
        if score < score_threshold {
            continue;
        }
        let b = decode_box(&boxes.data[i * 4..i * 4 + 4], &priors[i]);
        let lm = decode_landm(&landmarks.data[i * 10..i * 10 + 10], &priors[i]);
        let mut points = [Point2::new(0.0, 0.0); 5];
        for (k, p) in points.iter_mut().enumerate() {
            *p = Point2::new(
                lm[k * 2] as f64 * iw as f64,
                lm[k * 2 + 1] as f64 * ih as f64,
            );
        }
        detections.push(FaceDetection {
            face: FaceBox {
                x1: b[0] * iw,
                y1: b[1] * ih,
                x2: b[2] * iw,
                y2: b[3] * ih,
                score,
            },
            landmarks: points,
        });
    }

    let keep = nms(
        &detections.iter().map(|d| d.face).collect::<Vec<_>>(),
        iou_threshold,
    );
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
        FaceBox {
            x1,
            y1,
            x2,
            y2,
            score: s,
        }
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
            box_(1.0, 1.0, 11.0, 11.0, 0.5),   // 高 IoU 低分 → 抑制
            box_(50.0, 50.0, 60.0, 60.0, 0.8), // 独立 → 保留
            box_(0.0, 0.0, 10.0, 10.0, 0.95),  // 高 IoU 更高分 → 覆盖第一个
        ];
        let keep = nms(&boxes, 0.5);
        assert_eq!(keep, vec![3, 2]);
    }

    #[test]
    fn prior生成数量与640输入一致() {
        // 640 输入 → stride 8/16/32 特征图 80²/40²/20²，各 2 个 min_size
        let p = retinaface_priors(640, 640);
        assert_eq!(p.len(), 80 * 80 * 2 + 40 * 40 * 2 + 20 * 20 * 2);
        assert_eq!(p.len(), 16800);
        // 首个 prior：stride8 cell(0,0) min16 → 中心 (0.5/80, 0.5/80)，尺寸 16/640
        assert_eq!(p[0], [0.5 / 80.0, 0.5 / 80.0, 16.0 / 640.0, 16.0 / 640.0]);
        // 第二个：同 cell min32
        assert_eq!(p[1], [0.5 / 80.0, 0.5 / 80.0, 32.0 / 640.0, 32.0 / 640.0]);
    }

    #[test]
    fn prior解码公式() {
        // loc 全 0 → bbox 为 prior 中心 ± 半尺寸；landmark 全 0 → 全部落在 prior 中心
        let prior = [0.5, 0.5, 0.25, 0.25];
        let b = decode_box(&[0.0; 4], &prior);
        assert!((b[0] - 0.375).abs() < 1e-6);
        assert!((b[1] - 0.375).abs() < 1e-6);
        assert!((b[2] - 0.625).abs() < 1e-6);
        // loc 正向偏移 → 中心右移、尺寸增大（variance[1]=0.2 的 exp 缩放）
        let b2 = decode_box(&[1.0, 1.0, 1.0, 1.0], &prior);
        assert!((b2[0] + b2[2]) / 2.0 > 0.5 && b2[2] - b2[0] > 0.25);
        let lm = decode_landm(&[0.0; 10], &prior);
        for i in 0..5 {
            assert!((lm[2 * i] - 0.5).abs() < 1e-6);
            assert!((lm[2 * i + 1] - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn 解码过滤nms与坐标还原() {
        // image_size 64x64 → 168 个 prior（候选），loc/landmark 全 0。
        // prior 生成顺序：y 外层、x 内层，每 cell 两个 min_size，中心相对特征图归一化。
        // prior[70]（cell(4,3) min16）→ bbox (20,28,36,44) 高分 A
        // prior[72]（cell(4,4) min16）→ (28,28,44,44) C；prior[73]（cell(4,4) min32）→ (20,20,52,52) D（与 C IoU 0.25）
        let n = 168usize;
        let scores = {
            let mut s = vec![0.0f32; n];
            s[70] = 0.9; // A
            s[71] = 0.05; // 低分过滤
            s[72] = 0.8; // C
            s[73] = 0.85; // D
            s
        };
        let dets = decode_retinaface(
            &TensorData::new(vec![n as i64], scores).unwrap(),
            &TensorData::new(vec![n as i64, 4], vec![0.0; n * 4]).unwrap(),
            &TensorData::new(vec![n as i64, 10], vec![0.0; n * 10]).unwrap(),
            0.5,
            0.5,
            (64, 64),
            2.0,
            2.0,
            10.0,
            20.0,
        )
        .unwrap();
        // A、D、C 均保留（D 与 C IoU 0.25 < 0.5）
        assert_eq!(dets.len(), 3);
        // A 坐标还原：x = (20-10)/2 = 5
        let a = &dets[0];
        assert!((a.face.x1 - 5.0).abs() < 1e-3);
        assert!((a.face.y1 - 4.0).abs() < 1e-3); // (28-20)/2
        // C 还原：y1 = (28-20)/2 = 4，x1 = (28-10)/2 = 9
        let c = &dets[2];
        assert!((c.face.x1 - 9.0).abs() < 1e-3);
        assert!((c.face.y1 - 4.0).abs() < 1e-3);
        // 关键点还原：loc 全 0 → prior[70] 中心 (28,36) → (28-10)/2=9, (36-20)/2=8
        assert!((a.landmarks[0].x - 9.0).abs() < 1e-3);
        assert!((a.landmarks[0].y - 8.0).abs() < 1e-3);
    }

    #[test]
    fn 张量长度校验() {
        let s = TensorData::new(vec![2], vec![0.9, 0.8]).unwrap();
        let b = TensorData::new(vec![2, 4], vec![0.0; 8]).unwrap();
        let lm = TensorData::new(vec![2, 10], vec![0.0; 20]).unwrap();
        assert!(
            decode_retinaface(
                &s,
                &TensorData::new(vec![1, 4], vec![0.0; 4]).unwrap(),
                &lm,
                0.5,
                0.5,
                (64, 64),
                1.0,
                1.0,
                0.0,
                0.0
            )
            .is_err()
        );
        assert!(
            decode_retinaface(
                &s,
                &b,
                &TensorData::new(vec![1, 10], vec![0.0; 10]).unwrap(),
                0.5,
                0.5,
                (64, 64),
                1.0,
                1.0,
                0.0,
                0.0
            )
            .is_err()
        );
    }

    #[test]
    fn 锚框数量与候选不一致报错() {
        // image_size=64 → prior 数 168 与候选 2 不匹配
        let s = TensorData::new(vec![2], vec![0.9, 0.8]).unwrap();
        let b = TensorData::new(vec![2, 4], vec![0.0; 8]).unwrap();
        let lm = TensorData::new(vec![2, 10], vec![0.0; 20]).unwrap();
        assert!(decode_retinaface(&s, &b, &lm, 0.5, 0.5, (64, 64), 1.0, 1.0, 0.0, 0.0).is_err());
    }

    #[test]
    fn 两列分数布局取人脸分数列() {
        // 真实 RetinaFace 输出 scores [1, N, 2]：背景分在前、人脸分在后 → 取后一列
        // 168 个候选，仅候选 73 高分 → prior[73]（cell(4,4) min32）→ bbox (20,20,52,52)
        let n = 168usize;
        let mut scores = vec![0.0f32; n * 2];
        scores[73 * 2 + 1] = 0.95; // 候选 73 人脸分
        scores[73 * 2] = 0.05;
        let dets = decode_retinaface(
            &TensorData::new(vec![1, n as i64, 2], scores).unwrap(),
            &TensorData::new(vec![1, n as i64, 4], vec![0.0; n * 4]).unwrap(),
            &TensorData::new(vec![1, n as i64, 10], vec![0.0; n * 10]).unwrap(),
            0.5,
            0.5,
            (64, 64),
            1.0,
            1.0,
            0.0,
            0.0,
        )
        .unwrap();
        assert_eq!(dets.len(), 1);
        assert!((dets[0].face.x1 - 20.0).abs() < 1e-3);
        assert!((dets[0].face.y1 - 20.0).abs() < 1e-3);
    }
}
