//! MTCNN 三级联人脸检测（P-Net → R-Net → O-Net）。
//! 模型来源：linxiaohui/mtcnn-opencv（与 yiyuezhuo 同套 ONNX）。
//! 归一化：`(x - 127.5) * 0.0078125`；三级输入均为 NHWC（P-Net 空间维动态）。

use image::RgbImage;
use image::imageops::FilterType;

use crate::error::{CoreError, CoreResult};
use crate::inference::{InferenceEngine, TensorData};
use crate::vision::face::{FaceBox, FaceDetection, nms};
use crate::vision::geometry::Point2;

/// 级联三件套注册表 id（suite.face 逻辑名仍为 `mtcnn`）
pub const PNET_ID: &str = "mtcnn_pnet";
pub const RNET_ID: &str = "mtcnn_rnet";
pub const ONET_ID: &str = "mtcnn_onet";
/// speed 套件逻辑人脸检测 id
pub const CASCADE_FACE_ID: &str = "mtcnn";

/// 级联三级模型 id
pub fn cascade_model_ids() -> [&'static str; 3] {
    [PNET_ID, RNET_ID, ONET_ID]
}

/// 检测阈值（对齐 linxiaohui/mtcnn-opencv 默认 [0.6, 0.7, 0.7]）
const THRESHOLDS: [f32; 3] = [0.6, 0.7, 0.7];
/// 金字塔缩放因子
const SCALE_FACTOR: f32 = 0.709;
/// 最小人脸边长（像素）
const MIN_FACE_SIZE: u32 = 40;
/// 尺寸上限，避免过大金字塔
const MAX_PYRAMID_SCALES: usize = 16;

#[derive(Clone, Copy, Debug)]
struct Box5 {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    score: f32,
}

impl Box5 {
    fn to_face(self, landmarks: [Point2; 5]) -> FaceDetection {
        FaceDetection {
            face: FaceBox {
                x1: self.x1,
                y1: self.y1,
                x2: self.x2,
                y2: self.y2,
                score: self.score,
            },
            landmarks,
        }
    }
}

/// 完整 MTCNN 级联检测：在原图像素坐标系返回框与 5 关键点。
pub fn detect_mtcnn_cascade(
    engine: &mut dyn InferenceEngine,
    img: &RgbImage,
) -> CoreResult<Vec<FaceDetection>> {
    let (img_w, img_h) = img.dimensions();
    if img_w == 0 || img_h == 0 {
        return Err(CoreError::Image("图片尺寸为零".into()));
    }

    // ---- Stage1: 多尺度 P-Net ----
    let mut total: Vec<Box5> = Vec::new();
    for scale in pyramid_scales(img_w, img_h) {
        let sw = ((img_w as f32 * scale).ceil() as u32).max(12);
        let sh = ((img_h as f32 * scale).ceil() as u32).max(12);
        let scaled = image::imageops::resize(img, sw, sh, FilterType::Triangle);
        // Keras 导出的 P-Net 输入为 NHWC [1,H,W,3]（图内首层 Transpose NCHW）
        let input = mtcnn_nhwc(&scaled);
        let tensor = TensorData::new(vec![1, sh as i64, sw as i64, 3], input)?;
        let outs = engine.run(PNET_ID, &tensor)?;
        let proposals = pnet_decode(&outs, scale, THRESHOLDS[0])?;
        let kept = nms_boxes(&proposals, 0.5);
        total.extend(kept);
    }
    if total.is_empty() {
        return Ok(Vec::new());
    }
    total = nms_boxes(&total, 0.7);
    if total.is_empty() {
        return Ok(Vec::new());
    }
    // pnet_decode 已把回归并入框；此处仅 rerec 方形化
    total = rerec(total);
    total = fix_boxes(&total, img_w, img_h);

    // ---- Stage2: R-Net ----
    let r_in = crop_batch_mtcnn(img, &total, 24)?;
    if !r_in.is_empty() {
        let tensor = TensorData::new(vec![total.len() as i64, 24, 24, 3], r_in)?;
        let outs = engine.run(RNET_ID, &tensor)?;
        total = refine_stage(&total, &outs, 24, THRESHOLDS[1], img_w, img_h)?;
        total = nms_boxes(&total, 0.7);
        total = rerec(total);
        if total.is_empty() {
            return Ok(Vec::new());
        }
    } else {
        return Ok(Vec::new());
    }

    // ---- Stage3: O-Net ----
    let o_in = crop_batch_mtcnn(img, &total, 48)?;
    if o_in.is_empty() {
        return Ok(Vec::new());
    }
    let tensor = TensorData::new(vec![total.len() as i64, 48, 48, 3], o_in)?;
    let outs = engine.run(ONET_ID, &tensor)?;
    let (boxes, landmarks_raw) = refine_onet(&total, &outs, THRESHOLDS[2])?;
    if boxes.is_empty() {
        return Ok(Vec::new());
    }

    // 关键点：相对框归一化 → 像素，再 NMS(Min)
    let mut faces = Vec::with_capacity(boxes.len());
    for (i, b) in boxes.iter().enumerate() {
        let mut pts = [Point2::new(0.0, 0.0); 5];
        // landmarks_raw: 每框 10 个值 [x0..x4, y0..y4] 已在 refine_onet 转为像素
        if landmarks_raw.len() >= (i + 1) * 10 {
            let base = i * 10;
            for k in 0..5 {
                pts[k] = Point2::new(
                    landmarks_raw[base + k] as f64,
                    landmarks_raw[base + 5 + k] as f64,
                );
            }
        } else {
            let cx = (b.x1 + b.x2) * 0.5;
            let cy = (b.y1 + b.y2) * 0.5;
            for p in pts.iter_mut() {
                *p = Point2::new(cx as f64, cy as f64);
            }
        }
        faces.push(b.to_face(pts));
    }

    // Min-NMS（对齐 ONet 阶段）
    let final_idx = nms_min(&faces);
    Ok(final_idx.into_iter().map(|i| faces[i].clone()).collect())
}

/// 尺度金字塔：从 12/min_face 起，直到最短边 * scale < 12
fn pyramid_scales(w: u32, h: u32) -> Vec<f32> {
    let min_side = w.min(h) as f32;
    let m = 12.0 / MIN_FACE_SIZE as f32;
    let mut min_layer = min_side * m;
    let mut scales = Vec::new();
    let mut factor = 0usize;
    while min_layer >= 12.0 && scales.len() < MAX_PYRAMID_SCALES {
        scales.push(m * SCALE_FACTOR.powi(factor as i32));
        min_layer *= SCALE_FACTOR;
        factor += 1;
    }
    if scales.is_empty() {
        scales.push(1.0);
    }
    scales
}

/// RGB → (x-127.5)/128 NHWC（Keras MTCNN 输入布局）
fn mtcnn_nhwc(img: &RgbImage) -> Vec<f32> {
    let (w, h) = img.dimensions();
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x, y);
            for c in 0..3 {
                out.push((p[c] as f32 - 127.5) * 0.0078125);
            }
        }
    }
    out
}

/// 从输出张量识别 heatmap(2ch) / bbox(4ch)（按通道数，不依赖输出顺序）
fn split_hm_reg(outs: &[TensorData]) -> CoreResult<(&TensorData, &TensorData)> {
    let mut hm = None;
    let mut reg = None;
    for t in outs {
        let c = if t.shape.len() >= 2 {
            // [1,2,H,W] → dims[1]；[2,H,W] → dims[0]；[1,N,2] 时最后维 2
            if t.shape.len() == 4 && (t.shape[1] == 2 || t.shape[1] == 4) {
                t.shape[1] as usize
            } else if t.shape.len() == 3 && (t.shape[0] == 2 || t.shape[0] == 4) {
                t.shape[0] as usize
            } else if t.data.len() % 4 == 0 && t.data.len() % 2 == 0 && t.shape.len() == 2 {
                t.shape[1] as usize
            } else {
                0
            }
        } else {
            0
        };
        if c == 2 && hm.is_none() {
            hm = Some(t);
        } else if c == 4 && reg.is_none() {
            reg = Some(t);
        }
    }
    // 兜底：按元素数与空间维推断
    if hm.is_none() || reg.is_none() {
        for t in outs {
            if let Some((c, _, _)) = chw_shape(t) {
                if c == 2 && hm.is_none() {
                    hm = Some(t);
                } else if c == 4 && reg.is_none() {
                    reg = Some(t);
                }
            }
        }
    }
    match (hm, reg) {
        (Some(h), Some(r)) => Ok((h, r)),
        _ => Err(CoreError::Image(format!(
            "MTCNN 输出无法识别 heatmap/bbox（收到 {} 个张量）",
            outs.len()
        ))),
    }
}

fn chw_shape(t: &TensorData) -> Option<(usize, usize, usize)> {
    if t.shape.len() == 4 {
        Some((
            t.shape[1] as usize,
            t.shape[2] as usize,
            t.shape[3] as usize,
        ))
    } else if t.shape.len() == 3 {
        Some((
            t.shape[0] as usize,
            t.shape[1] as usize,
            t.shape[2] as usize,
        ))
    } else {
        None
    }
}

/// P-Net 解码：特征图坐标 → 原缩放图像素 → 原图像素（/scale）
fn pnet_decode(outs: &[TensorData], scale: f32, thr: f32) -> CoreResult<Vec<Box5>> {
    let (hm, reg) = split_hm_reg(outs)?;
    let (h, w) = {
        let (_, h, w) =
            chw_shape(hm).ok_or_else(|| CoreError::Image("P-Net heatmap 布局非法".into()))?;
        (h, w)
    };
    if h == 0 || w == 0 {
        return Ok(Vec::new());
    }
    let hm_data = &hm.data;
    let reg_data = &reg.data;
    // 兼容 [1,2,H,W] 与 NHWC [1,H,W,2]
    let hm_is_nchw = chw_shape(hm).map(|(c, _, _)| c == 2).unwrap_or(false);
    let stride = 2.0f32;
    let cell = 12.0f32;
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let score = if hm_is_nchw {
                // channel 1 = face
                let idx = h * w + y * w + x;
                hm_data.get(idx).copied().unwrap_or(0.0)
            } else {
                // NHWC [1,H,W,2] or [H,W,2]
                let base = y * w + x;
                hm_data.get(base * 2 + 1).copied().unwrap_or(0.0)
            };
            if score < thr {
                continue;
            }
            let (r0, r1, r2, r3) = if reg_data.len() >= 4 * h * w {
                // NCHW 4 通道
                let n = h * w;
                (
                    reg_data[y * w + x],
                    reg_data[n + y * w + x],
                    reg_data[n * 2 + y * w + x],
                    reg_data[n * 3 + y * w + x],
                )
            } else if reg_data.len() >= h * w * 4 {
                let b = (y * w + x) * 4;
                (
                    reg_data[b],
                    reg_data[b + 1],
                    reg_data[b + 2],
                    reg_data[b + 3],
                )
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            // 经典：x1 = stride*x+1，再加回归；映射到缩放图再 /scale
            let mut x1 = stride * x as f32 + 1.0 + r0 * cell;
            let mut y1 = stride * y as f32 + 1.0 + r1 * cell;
            let mut x2 = stride * x as f32 + 1.0 + (cell - 1.0) + r2 * cell;
            let mut y2 = stride * y as f32 + 1.0 + (cell - 1.0) + r3 * cell;
            // 转原图
            x1 /= scale;
            y1 /= scale;
            x2 /= scale;
            y2 /= scale;
            if x2 <= x1 || y2 <= y1 {
                continue;
            }
            out.push(Box5 {
                x1,
                y1,
                x2,
                y2,
                score,
            });
        }
    }
    Ok(out)
}

/// 按置信度 NMS（Union）
fn nms_boxes(boxes: &[Box5], thr: f32) -> Vec<Box5> {
    if boxes.is_empty() {
        return Vec::new();
    }
    let faces: Vec<FaceDetection> = boxes
        .iter()
        .map(|b| b.to_face([Point2::new(0.0, 0.0); 5]))
        .collect();
    let idx = nms(&faces.iter().map(|f| f.face).collect::<Vec<_>>(), thr);
    idx.into_iter().map(|i| boxes[i]).collect()
}

/// Min-IoU NMS（ONet）
fn nms_min(faces: &[FaceDetection]) -> Vec<usize> {
    if faces.is_empty() {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..faces.len()).collect();
    order.sort_by(|&a, &b| {
        faces[b]
            .face
            .score
            .partial_cmp(&faces[a].face.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep = Vec::new();
    let mut alive = vec![true; faces.len()];
    for &i in &order {
        if !alive[i] {
            continue;
        }
        keep.push(i);
        for &j in &order {
            if j == i || !alive[j] {
                continue;
            }
            let a = &faces[i].face;
            let b = &faces[j].face;
            let ix1 = a.x1.max(b.x1);
            let iy1 = a.y1.max(b.y1);
            let ix2 = a.x2.min(b.x2);
            let iy2 = a.y2.min(b.y2);
            let iw = (ix2 - ix1).max(0.0);
            let ih = (iy2 - iy1).max(0.0);
            let inter = iw * ih;
            let min_area = a.area().min(b.area()).max(1e-6);
            if inter / min_area > 0.7 {
                alive[j] = false;
            }
        }
    }
    keep
}

/// 方形化（rerec）
fn rerec(boxes: Vec<Box5>) -> Vec<Box5> {
    boxes
        .into_iter()
        .map(|mut b| {
            let w = b.x2 - b.x1;
            let h = b.y2 - b.y1;
            let side = w.max(h);
            b.x1 = b.x1 + w * 0.5 - side * 0.5;
            b.y1 = b.y1 + h * 0.5 - side * 0.5;
            b.x2 = b.x1 + side;
            b.y2 = b.y1 + side;
            b
        })
        .collect()
}

fn fix_boxes(boxes: &[Box5], w: u32, h: u32) -> Vec<Box5> {
    boxes
        .iter()
        .map(|b| Box5 {
            x1: b.x1.max(0.0),
            y1: b.y1.max(0.0),
            x2: b.x2.min(w as f32 - 1.0),
            y2: b.y2.min(h as f32 - 1.0),
            score: b.score,
        })
        .filter(|b| b.x2 > b.x1 && b.y2 > b.y1)
        .collect()
}

/// 裁剪并缩放到固定边长，输出 NHWC 展平 + MTCNN 归一化
fn crop_batch_mtcnn(img: &RgbImage, boxes: &[Box5], side: u32) -> CoreResult<Vec<f32>> {
    let (iw, ih) = img.dimensions();
    let mut out = Vec::with_capacity(boxes.len() * (side * side * 3) as usize);
    for b in boxes {
        let x1 = b.x1.floor().max(0.0) as u32;
        let y1 = b.y1.floor().max(0.0) as u32;
        let x2 = (b.x2.ceil() as u32).min(iw.saturating_sub(1));
        let y2 = (b.y2.ceil() as u32).min(ih.saturating_sub(1));
        let cw = x2.saturating_sub(x1).max(1);
        let ch = y2.saturating_sub(y1).max(1);
        let crop = image::imageops::crop_imm(img, x1, y1, cw, ch).to_image();
        let resized = image::imageops::resize(&crop, side, side, FilterType::Triangle);
        for y in 0..side {
            for x in 0..side {
                let p = resized.get_pixel(x, y);
                for c in 0..3 {
                    out.push((p[c] as f32 - 127.5) * 0.0078125);
                }
            }
        }
    }
    Ok(out)
}

/// R/ONet 共用：分数过滤 + bbreg + 裁剪到图内
fn refine_stage(
    prev: &[Box5],
    outs: &[TensorData],
    _side: usize,
    thr: f32,
    img_w: u32,
    img_h: u32,
) -> CoreResult<Vec<Box5>> {
    if prev.is_empty() {
        return Ok(Vec::new());
    }
    let n = prev.len();
    let (scores, regs) = split_score_reg(outs, n)?;
    let mut kept = Vec::new();
    for i in 0..n {
        if scores[i] < thr {
            continue;
        }
        let (r0, r1, r2, r3) = regs[i];
        let b = &prev[i];
        let w = b.x2 - b.x1 + 1.0;
        let h = b.y2 - b.y1 + 1.0;
        kept.push(Box5 {
            x1: b.x1 + r0 * w,
            y1: b.y1 + r1 * h,
            x2: b.x2 + r2 * w,
            y2: b.y2 + r3 * h,
            score: scores[i],
        });
    }
    Ok(fix_boxes(&kept, img_w, img_h))
}

/// 单个候选的 4 项 bbox 回归（左/上/右/下）
type Reg4 = (f32, f32, f32, f32);
/// 分数与回归的分解结果
type ScoreReg = (Vec<f32>, Vec<Reg4>);

/// 批次输出：scores[N] + regs[N,4]（按元素数与 shape 推断）
fn split_score_reg(outs: &[TensorData], n: usize) -> CoreResult<ScoreReg> {
    let mut scores = None;
    let mut regs = None;
    for t in outs {
        if t.data.len() == n && scores.is_none() {
            scores = Some(t.data.clone());
        } else if t.data.len() == n * 4 && regs.is_none() {
            let mut v = Vec::with_capacity(n);
            for i in 0..n {
                v.push((
                    t.data[i * 4],
                    t.data[i * 4 + 1],
                    t.data[i * 4 + 2],
                    t.data[i * 4 + 3],
                ));
            }
            regs = Some(v);
        }
    }
    // 两通道 softmax：取长度 2N 的
    if scores.is_none() {
        for t in outs {
            if t.data.len() == n * 2 {
                let mut s = Vec::with_capacity(n);
                for i in 0..n {
                    s.push(t.data[i * 2 + 1].max(t.data[i * 2]));
                    // 若布局是 [bg, fg] 取 index1；若已是 face 分直接用较大值兜底
                    s[i] = t.data[i * 2 + 1];
                }
                scores = Some(s);
            }
        }
    }
    match (scores, regs) {
        (Some(s), Some(r)) => Ok((s, r)),
        _ => Err(CoreError::Image(format!(
            "MTCNN R/ONet 输出无法解析（n={n}，{} 个张量）",
            outs.len()
        ))),
    }
}

/// O-Net：分数 + 回归 + landmarks（相对归一化 → 像素）
fn refine_onet(prev: &[Box5], outs: &[TensorData], thr: f32) -> CoreResult<(Vec<Box5>, Vec<f32>)> {
    let n = prev.len();
    let (scores, regs) = split_score_reg(outs, n)?;
    // landmarks：[N,10] 元素（x0..x4, y0..y4）
    let mut lm = None;
    for t in outs {
        if t.data.len() == n * 10 {
            lm = Some(t.data.clone());
            break;
        }
    }

    let mut boxes = Vec::new();
    let mut lm_out = Vec::new();
    for i in 0..n {
        if scores[i] < thr {
            continue;
        }
        let b = &prev[i];
        let (r0, r1, r2, r3) = regs[i];
        let w = b.x2 - b.x1 + 1.0;
        let h = b.y2 - b.y1 + 1.0;
        let nb = Box5 {
            x1: b.x1 + r0 * w,
            y1: b.y1 + r1 * h,
            x2: b.x2 + r2 * w,
            y2: b.y2 + r3 * h,
            score: scores[i],
        };
        if let Some(l) = &lm {
            // 相对框归一化 → 像素（与 Python 一致：x*w + x1 - 1 → 再 +1 近似为 x*w+x1）
            let base = i * 10;
            let mut px = [0.0f32; 10];
            for k in 0..5 {
                px[k] = l[base + k] * w + nb.x1;
                px[5 + k] = l[base + 5 + k] * h + nb.y1;
            }
            lm_out.extend_from_slice(&px);
        } else {
            let cx = (nb.x1 + nb.x2) * 0.5;
            let cy = (nb.y1 + nb.y2) * 0.5;
            lm_out.extend_from_slice(&[cx; 5]);
            lm_out.extend_from_slice(&[cy; 5]);
        }
        boxes.push(nb);
    }
    Ok((boxes, lm_out))
}
