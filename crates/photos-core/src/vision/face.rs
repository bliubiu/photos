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
///
/// 流程：prior 解码 → 低分过滤 → NMS → 坐标还原（`scale_*` / `pad_*` 提供 letterbox 逆变换）。
///
/// 模型坐标 → 原图坐标的还原参数（letterbox 逆变换 + 模型输入尺寸）
#[derive(Debug, Clone, Copy)]
pub struct DecodeTransform {
    /// 模型输入尺寸（宽, 高）
    pub image_size: (u32, u32),
    /// 水平缩放系数
    pub scale_x: f32,
    /// 垂直缩放系数
    pub scale_y: f32,
    /// 水平填充像素（左）
    pub pad_x: f32,
    /// 垂直填充像素（上）
    pub pad_y: f32,
}

pub fn decode_retinaface(
    scores: &TensorData,
    boxes: &TensorData,
    landmarks: &TensorData,
    score_threshold: f32,
    iou_threshold: f32,
    transform: DecodeTransform,
) -> CoreResult<Vec<FaceDetection>> {
    let DecodeTransform { image_size, scale_x, scale_y, pad_x, pad_y } = transform;
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

/// 生成 P-Net 锚框级提议并解码（stride=12，cell=12，对齐经典 Caffe MTCNN）。
/// `heatmap` 为 `[2,H,W]` 或 `[1,2,H,W]`（索引 1 为前景分）；`bbox_reg` 为 `[4,H,W]` 或 `[1,4,H,W]`。
fn pnet_proposals(
    heatmap: &TensorData,
    bbox_reg: &TensorData,
    score_threshold: f32,
) -> CoreResult<Vec<FaceDetection>> {
    let (hm, h, w) = split_hw_channels(heatmap, 2, "MTCNN heatmap")?;
    let (reg, rh, rw) = split_hw_channels(bbox_reg, 4, "MTCNN bbox 回归")?;
    if rh != h || rw != w {
        return Err(CoreError::Image(format!(
            "MTCNN heatmap 与 bbox 回归特征图尺寸不一致：{h}x{w} vs {rh}x{rw}"
        )));
    }
    const STRIDE: f32 = 12.0;
    const CELL: f32 = 12.0;
    let mut dets = Vec::new();
    for y in 0..h {
        for x in 0..w {
            // 通道 1 = face；回归 4 通道依次为左/上/右/下偏移
            let score = hm[h * w + y * w + x];
            if score < score_threshold {
                continue;
            }
            let r0 = reg[y * w + x];
            let r1 = reg[h * w + y * w + x];
            let r2 = reg[2 * h * w + y * w + x];
            let r3 = reg[3 * h * w + y * w + x];
            // 对齐常见 P-Net 解码：左上 = stride*x - cell*r，右下 = stride*(x+1) + cell*r
            // （零回归时框尺寸约为 stride，避免空框）
            let x1 = STRIDE * x as f32 - CELL * r0;
            let y1 = STRIDE * y as f32 - CELL * r1;
            let x2 = STRIDE * (x as f32 + 1.0) + CELL * r2;
            let y2 = STRIDE * (y as f32 + 1.0) + CELL * r3;
            if x2 <= x1 || y2 <= y1 {
                continue;
            }
            // 关键点用框中心近似（P-Net 无 5 点；完整 MTCNN 若提供 landmarks 张量则优先）
            let cx = (x1 + x2) * 0.5;
            let cy = (y1 + y2) * 0.5;
            let points = [
                Point2::new(cx as f64, cy as f64),
                Point2::new(cx as f64, cy as f64),
                Point2::new(cx as f64, cy as f64),
                Point2::new(cx as f64, cy as f64),
                Point2::new(cx as f64, cy as f64),
            ];
            dets.push(FaceDetection {
                face: FaceBox {
                    x1,
                    y1,
                    x2,
                    y2,
                    score,
                },
                landmarks: points,
            });
        }
    }
    Ok(dets)
}

/// 从张量取出 `[C,H,W]` 通道布局（兼容丢掉 batch 维）。
fn split_hw_channels(
    t: &TensorData,
    channels: usize,
    name: &str,
) -> CoreResult<(Vec<f32>, usize, usize)> {
    let data = &t.data;
    let shape = &t.shape;
    // [1,C,H,W] 或 [C,H,W]
    let (c, h, w) = if shape.len() == 4 {
        (shape[1] as usize, shape[2] as usize, shape[3] as usize)
    } else if shape.len() == 3 {
        (shape[0] as usize, shape[1] as usize, shape[2] as usize)
    } else {
        return Err(CoreError::Image(format!(
            "{name} 张量布局应为 [C,H,W] 或 [1,C,H,W]，收到 {shape:?}"
        )));
    };
    if c != channels || h == 0 || w == 0 {
        return Err(CoreError::Image(format!(
            "{name} 通道数或尺寸非法：C={c} H={h} W={w}（期望 C={channels}）"
        )));
    }
    if data.len() != c * h * w {
        return Err(CoreError::Image(format!(
            "{name} 元素数 {} 与 C*H*W={} 不一致",
            data.len(),
            c * h * w
        )));
    }
    Ok((data.clone(), h, w))
}

/// 判断是否「融合终态」布局：三元组 [boxes N×4, scores N, landmarks N×10]（任意 batch 维）。
fn try_fused_mtcnn_outputs(
    outputs: &[TensorData],
) -> Option<(&TensorData, &TensorData, &TensorData)> {
    if outputs.len() < 3 {
        return None;
    }
    let mut lm_idx = None;
    for (i, t) in outputs.iter().enumerate() {
        if t.data.len() >= 10 && t.data.len() % 10 == 0 {
            let n = t.data.len() / 10;
            if outputs
                .iter()
                .enumerate()
                .any(|(j, u)| j != i && u.data.len() == n * 4)
                && outputs
                    .iter()
                    .enumerate()
                    .any(|(j, u)| j != i && (u.data.len() == n || u.data.len() == n * 2))
            {
                lm_idx = Some(i);
                break;
            }
        }
    }
    let li = lm_idx?;
    let n = outputs[li].data.len() / 10;
    let bi = (0..outputs.len()).find(|&i| i != li && outputs[i].data.len() == n * 4)?;
    let si = (0..outputs.len()).find(|&i| {
        i != li && i != bi && (outputs[i].data.len() == n || outputs[i].data.len() == n * 2)
    })?;
    Some((&outputs[bi], &outputs[si], &outputs[li]))
}

/// 融合终态解码：boxes 已为像素坐标（输入图尺度）或归一化 [0,1]，scores 为前景分，landmarks 同尺度。
fn decode_fused_mtcnn(
    boxes: &TensorData,
    scores: &TensorData,
    landmarks: &TensorData,
    score_threshold: f32,
    iou_threshold: f32,
    transform: DecodeTransform,
) -> CoreResult<Vec<FaceDetection>> {
    let DecodeTransform { image_size, scale_x, scale_y, pad_x, pad_y } = transform;
    let n = boxes.data.len() / 4;
    if n == 0 {
        return Err(CoreError::Image("MTCNN 融合输出无检测框".into()));
    }
    if landmarks.data.len() != n * 10 {
        return Err(CoreError::Image(format!(
            "MTCNN 融合 landmarks 长度 {} 与候选数 {n} 不一致",
            landmarks.data.len()
        )));
    }
    let (ih, iw) = (image_size.0 as f32, image_size.1 as f32);
    let score_stride = scores.data.len() / n;
    if score_stride != 1 && score_stride != 2 {
        return Err(CoreError::Image(format!(
            "MTCNN 融合 scores 布局异常：长度 {} / 候选 {n}",
            scores.data.len()
        )));
    }
    // 坐标是否为归一化：最大边长 < 2 视为 [0,1]
    let mut max_coord = 0.0f32;
    for i in 0..n {
        for k in 0..4 {
            max_coord = max_coord.max(boxes.data[i * 4 + k].abs());
        }
    }
    let norm = max_coord <= 2.0;

    let mut detections = Vec::with_capacity(n);
    for i in 0..n {
        let score = scores.data[i * score_stride + score_stride - 1];
        if score < score_threshold {
            continue;
        }
        let (mut x1, mut y1, mut x2, mut y2) = (
            boxes.data[i * 4],
            boxes.data[i * 4 + 1],
            boxes.data[i * 4 + 2],
            boxes.data[i * 4 + 3],
        );
        let mut lm = [0.0f32; 10];
        for k in 0..10 {
            lm[k] = landmarks.data[i * 10 + k];
        }
        if norm {
            x1 *= iw;
            x2 *= iw;
            y1 *= ih;
            y2 *= ih;
            for k in 0..5 {
                lm[k * 2] *= iw;
                lm[k * 2 + 1] *= ih;
            }
        }
        let mut points = [Point2::new(0.0, 0.0); 5];
        for (k, p) in points.iter_mut().enumerate() {
            *p = Point2::new(lm[k * 2] as f64, lm[k * 2 + 1] as f64);
        }
        detections.push(FaceDetection {
            face: FaceBox {
                x1,
                y1,
                x2,
                y2,
                score,
            },
            landmarks: points,
        });
    }
    finish_mtcnn_nms(detections, iou_threshold, scale_x, scale_y, pad_x, pad_y)
}

/// NMS + letterbox 逆变换
fn finish_mtcnn_nms(
    detections: Vec<FaceDetection>,
    iou_threshold: f32,
    scale_x: f32,
    scale_y: f32,
    pad_x: f32,
    pad_y: f32,
) -> CoreResult<Vec<FaceDetection>> {
    let keep = nms(
        &detections.iter().map(|d| d.face).collect::<Vec<_>>(),
        iou_threshold,
    );
    let mut out = Vec::with_capacity(keep.len());
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
        // 裁剪到非负半平面（原图坐标）
        d.face.x1 = d.face.x1.max(0.0);
        d.face.y1 = d.face.y1.max(0.0);
        out.push(d);
    }
    Ok(out)
}

/// MTCNN 输出解码。
///
/// 支持两类 ONNX 导出：
/// 1. **融合终态**：输出含 boxes `[N,4]`、scores `[N]`、landmarks `[N,10]`（常见于整网导出）
/// 2. **P-Net 映射**：`heatmap [1,2,H,W]` + `bbox [1,4,H,W]`（经典 det1/P-Net 风格）
///
/// 流程：解码 → 过滤 → NMS → letterbox 逆变换（`scale_*` / `pad_*`）。
pub fn decode_mtcnn(
    outputs: &[TensorData],
    score_threshold: f32,
    iou_threshold: f32,
    transform: DecodeTransform,
) -> CoreResult<Vec<FaceDetection>> {
    let DecodeTransform { image_size: input_size, scale_x, scale_y, pad_x, pad_y } = transform;
    if outputs.is_empty() {
        return Err(CoreError::Image("MTCNN 输出为空".into()));
    }
    // 优先融合终态
    if let Some((boxes, scores, lm)) = try_fused_mtcnn_outputs(outputs) {
        return decode_fused_mtcnn(
            boxes,
            scores,
            lm,
            score_threshold,
            iou_threshold,
            DecodeTransform {
                image_size: input_size,
                scale_x,
                scale_y,
                pad_x,
                pad_y,
            },
        );
    }
    // 否则按 P-Net：找 2 通道 heatmap 与 4 通道回归
    let heatmap = outputs.iter().find(|t| {
        let c = if t.shape.len() == 4 {
            t.shape[1]
        } else if t.shape.len() == 3 {
            t.shape[0]
        } else {
            0
        };
        c == 2
    });
    let bbox = outputs.iter().find(|t| {
        let c = if t.shape.len() == 4 {
            t.shape[1]
        } else if t.shape.len() == 3 {
            t.shape[0]
        } else {
            0
        };
        c == 4
    });
    match (heatmap, bbox) {
        (Some(hm), Some(bb)) => {
            let dets = pnet_proposals(hm, bb, score_threshold)?;
            finish_mtcnn_nms(dets, iou_threshold, scale_x, scale_y, pad_x, pad_y)
        }
        _ => Err(CoreError::Image(
            "无法识别 MTCNN 输出布局：期望融合三元组 [boxes,scores,landmarks] 或 P-Net [heatmap,bbox]".into(),
        )),
    }
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
            DecodeTransform {
                image_size: (64, 64),
                scale_x: 2.0,
                scale_y: 2.0,
                pad_x: 10.0,
                pad_y: 20.0,
            },
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
                DecodeTransform {
                    image_size: (64, 64),
                    scale_x: 1.0,
                    scale_y: 1.0,
                    pad_x: 0.0,
                    pad_y: 0.0
                }
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
                DecodeTransform {
                    image_size: (64, 64),
                    scale_x: 1.0,
                    scale_y: 1.0,
                    pad_x: 0.0,
                    pad_y: 0.0
                }
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
        assert!(
        decode_retinaface(
            &s,
            &b,
            &lm,
            0.5,
            0.5,
            DecodeTransform {
                image_size: (64, 64),
                scale_x: 1.0,
                scale_y: 1.0,
                pad_x: 0.0,
                pad_y: 0.0
            }
        )
        .is_err()
    );
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
            DecodeTransform {
                image_size: (64, 64),
                scale_x: 1.0,
                scale_y: 1.0,
                pad_x: 0.0,
                pad_y: 0.0,
            },
        )
        .unwrap();
        assert_eq!(dets.len(), 1);
        assert!((dets[0].face.x1 - 20.0).abs() < 1e-3);
        assert!((dets[0].face.y1 - 20.0).abs() < 1e-3);
    }

    #[test]
    fn mtcnn_pnet解码过滤与逆变换() {
        // 12x12 特征图（输入约 128），stride=12
        // 位置 (2,3)：前景分 0.95，回归全 0 → 框 [24,36,36,48]
        let h = 12usize;
        let w = 12usize;
        let mut hm = vec![0.0f32; 2 * h * w];
        let reg = vec![0.0f32; 4 * h * w];
        hm[h * w + 3 * w + 2] = 0.95;
        hm[h * w] = 0.2; // 低分过滤
        let dets = decode_mtcnn(
            &[
                TensorData::new(vec![1, 2, h as i64, w as i64], hm).unwrap(),
                TensorData::new(vec![1, 4, h as i64, w as i64], reg).unwrap(),
            ],
            0.5,
            0.4,
            DecodeTransform {
                image_size: (128, 128),
                scale_x: 1.0,
                scale_y: 1.0,
                pad_x: 0.0,
                pad_y: 0.0,
            },
        )
        .unwrap();
        assert_eq!(dets.len(), 1);
        let f = &dets[0].face;
        assert!((f.x1 - 24.0).abs() < 1e-3, "x1={}", f.x1);
        assert!((f.y1 - 36.0).abs() < 1e-3, "y1={}", f.y1);
        assert!((f.x2 - 36.0).abs() < 1e-3, "x2={}", f.x2);
        assert!((f.y2 - 48.0).abs() < 1e-3, "y2={}", f.y2);
    }

    #[test]
    fn mtcnn融合输出解码() {
        // 两候选：高分框 + 低分过滤；框为像素坐标
        let boxes = vec![10.0, 20.0, 50.0, 70.0, 15.0, 25.0, 55.0, 75.0];
        let scores = vec![0.92, 0.1];
        let mut lm = vec![0.0f32; 20];
        for k in 0..5 {
            lm[k * 2] = 20.0 + k as f32;
            lm[k * 2 + 1] = 30.0 + k as f32;
        }
        let dets = decode_mtcnn(
            &[
                TensorData::new(vec![2, 4], boxes).unwrap(),
                TensorData::new(vec![2], scores).unwrap(),
                TensorData::new(vec![2, 10], lm).unwrap(),
            ],
            0.5,
            0.4,
            DecodeTransform {
                image_size: (100, 100),
                scale_x: 1.0,
                scale_y: 1.0,
                pad_x: 0.0,
                pad_y: 0.0,
            },
        )
        .unwrap();
        assert_eq!(dets.len(), 1);
        assert!((dets[0].face.x1 - 10.0).abs() < 1e-3);
        assert!((dets[0].landmarks[0].x - 20.0).abs() < 1e-3);
    }

    #[test]
    fn mtcnn未知布局报错() {
        let err = decode_mtcnn(
            &[TensorData::new(vec![1, 3, 4, 4], vec![0.0; 48]).unwrap()],
            0.5,
            0.4,
            DecodeTransform {
                image_size: (32, 32),
                scale_x: 1.0,
                scale_y: 1.0,
                pad_x: 0.0,
                pad_y: 0.0,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("MTCNN"));
    }
}
