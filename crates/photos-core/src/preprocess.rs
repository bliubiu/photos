//! 推理前处理：图像 → 模型输入张量（letterbox / resize + 归一化 + 布局对齐）。
//!
//! 布局约定（docs/04-模型清单.md §3）：
//! - NCHW `[1,3,H,W]`：RetinaFace / MTCNN / RMBG / BiRefNet / MODNet
//! - NHWC `[1,H,W,3]`：MoveNet（RGB 归一化 [0,1]，坐标输出相对输入归一化）
//!
//! 布局与归一化由模型注册表声明（`[models.<id>.preprocess]`）驱动：
//! - `layout`：`auto`（按通道维位置推断，默认）| `nchw` | `nhwc`
//! - `resize`：`letterbox`（等比 + 灰边，检测类默认）| `stretch`（直接拉伸，抠图/分割类）
//! - `norm`：`unit`（RGB÷255→[0,1]）| `none` | `mean` | `mean_std`
//! - `channel`：`rgb`（默认）| `bgr`
//!
//! 抠图模型（BiRefNet/RMBG/MODNet）官方与 HivisionIDPhotos 均为**直接拉伸** resize，
//! letterbox 灰边不在训练分布内，会系统性劣化边缘/发丝置信度——内置抠图模型统一声明
//! `stretch`；人脸/关键点检测类保持 `letterbox`（坐标可逆变换还原）。
//! 插件化注册的模型只需在配置中声明预处理，pipeline 侧无需改动。

use image::{GrayImage, RgbImage};

use crate::config::{ChannelOrder, Layout, Norm, Preprocess, ResizeMode};
use crate::error::{CoreError, CoreResult};
use crate::inference::TensorData;

/// letterbox 逆变换参数：把模型坐标还原到原图坐标
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LetterBox {
    /// 统一缩放系数（原图 → 目标图的等比缩放）
    pub scale: f32,
    /// 水平填充像素（左/右各半，取左）
    pub pad_x: f32,
    /// 垂直填充像素（上/下各半，取上）
    pub pad_y: f32,
}

/// 模型输入：张量 + letterbox 逆变换参数（仅人脸检测类模型需要坐标还原）
#[derive(Debug, Clone)]
pub struct ModelInput {
    /// 对齐模型 input_dims 的输入张量
    pub tensor: TensorData,
    /// letterbox 逆变换参数；非 letterbox 输入为 None（解码侧按 1/0 处理）
    pub letterbox: Option<LetterBox>,
}

/// 依据模型 input_dims 构造推理输入：
/// - NCHW（通道在 dims[1]）：按 `resize` 声明——`letterbox` 等比缩放 + 灰边填充（返回逆变换
///   参数）；`stretch` 直接拉伸（mask 输出与原图为同一几何映射，无需逆变换）
/// - NHWC（通道在最后一维）：直接 resize（MoveNet 官方约定），坐标为归一化，无需逆变换
///
/// `retinaface` 为 true 时采用 RetinaFace 官方预处理：RGB 0-255 减均值 (104,117,123)，不归一化
/// （见 HivisionIDPhotos/hivision/creator/retinaface/inference.py）。
///
/// 该函数为兼容入口，等价于 [`build_input_with`] 传入 `retinaface` 对应的预处理声明；
/// 需要自定义归一化/通道序（插件化注册的模型）请改用 [`build_input_with`]。
pub fn build_input(img: &RgbImage, dims: &[i64], retinaface: bool) -> CoreResult<ModelInput> {
    let pp = if retinaface {
        Preprocess {
            layout: Layout::Auto,
            resize: ResizeMode::Letterbox,
            norm: Norm::Mean([104.0, 117.0, 123.0]),
            channel: ChannelOrder::Rgb,
        }
    } else {
        Preprocess::default()
    };
    build_input_with(img, dims, &pp)
}

/// 依据模型注册表声明的预处理约定构造推理输入（插件化模型接入点）
pub fn build_input_with(img: &RgbImage, dims: &[i64], pp: &Preprocess) -> CoreResult<ModelInput> {
    match pp.effective_layout(dims)? {
        Layout::Nchw => {
            let (tw, th) = (dims[2] as u32, dims[3] as u32);
            if tw == 0 || th == 0 {
                return Err(CoreError::Inference(format!("模型输入尺寸非法：{dims:?}")));
            }
            match pp.resize {
                // 直接拉伸：与训练分布一致（BiRefNet/RMBG/MODNet 官方约定）
                ResizeMode::Stretch => {
                    let resized = image::imageops::resize(
                        img,
                        tw,
                        th,
                        image::imageops::FilterType::Triangle,
                    );
                    let data = rgb_to_nchw_with(&resized, pp.norm, pp.channel);
                    Ok(ModelInput {
                        tensor: TensorData::new(dims.to_vec(), data)?,
                        letterbox: None,
                    })
                }
                ResizeMode::Letterbox => {
                    let (canvas, lb) = letterbox(img, tw, th);
                    let data = rgb_to_nchw_with(&canvas, pp.norm, pp.channel);
                    Ok(ModelInput {
                        tensor: TensorData::new(dims.to_vec(), data)?,
                        letterbox: Some(lb),
                    })
                }
            }
        }
        Layout::Nhwc => {
            let (tw, th) = (dims[1] as u32, dims[2] as u32);
            if tw == 0 || th == 0 {
                return Err(CoreError::Inference(format!("模型输入尺寸非法：{dims:?}")));
            }
            let resized =
                image::imageops::resize(img, tw, th, image::imageops::FilterType::Triangle);
            let data = rgb_to_nhwc_with(&resized, pp.norm, pp.channel);
            Ok(ModelInput {
                tensor: TensorData::new(dims.to_vec(), data)?,
                letterbox: None,
            })
        }
        Layout::Auto => Err(CoreError::Inference(format!(
            "不支持的输入布局 {dims:?}：需 NCHW（通道在第二维）或 NHWC（通道在最后一维）且通道数为 3"
        ))),
    }
}

/// 等比缩放 + 灰边填充到目标尺寸（letterbox），返回（处理后图像，逆变换参数）。
/// 填充色取常见推理灰值 [114,114,114]；scale 为原图→目标图的统一缩放系数。
pub fn letterbox(img: &RgbImage, target_w: u32, target_h: u32) -> (RgbImage, LetterBox) {
    let (w, h) = img.dimensions();
    let scale = (target_w as f32 / w.max(1) as f32)
        .min(target_h as f32 / h.max(1) as f32)
        .max(1e-6);
    let new_w = (w as f32 * scale).round().max(1.0) as u32;
    let new_h = (h as f32 * scale).round().max(1.0) as u32;
    let resized = image::imageops::resize(img, new_w, new_h, image::imageops::FilterType::Triangle);
    let pad_x = ((target_w - new_w) / 2) as f32;
    let pad_y = ((target_h - new_h) / 2) as f32;
    let mut canvas = RgbImage::from_pixel(target_w, target_h, image::Rgb([114, 114, 114]));
    for y in 0..new_h {
        for x in 0..new_w {
            canvas.put_pixel(pad_x as u32 + x, pad_y as u32 + y, *resized.get_pixel(x, y));
        }
    }
    (
        canvas,
        LetterBox {
            scale,
            pad_x,
            pad_y,
        },
    )
}

/// 单通道值 → 归一化后的 f32（按声明方式处理；`channel` 决定取 R/G/B 哪一路）
#[inline]
fn normalize_channel(v: u8, norm: Norm, idx: usize) -> f32 {
    let raw = v as f32;
    match norm {
        Norm::Unit => raw / 255.0,
        Norm::None => raw,
        Norm::Mean(mean) => raw - mean[idx],
        Norm::MeanStd { mean, std } => (raw - mean[idx]) / std[idx],
    }
}

/// 取出像素的通道值（支持 RGB / BGR 通道序）
#[inline]
fn channel_value(p: &image::Rgb<u8>, channel: ChannelOrder, idx: usize) -> u8 {
    match channel {
        ChannelOrder::Rgb => p[idx],
        ChannelOrder::Bgr => p[2 - idx],
    }
}

/// 图像 → NCHW 行主序 f32（C,H,W），按声明的归一化与通道序处理
fn rgb_to_nchw_with(img: &RgbImage, norm: Norm, channel: ChannelOrder) -> Vec<f32> {
    let (w, h) = img.dimensions();
    let n = (w * h) as usize;
    let mut data = vec![0.0f32; n * 3];
    for (i, p) in img.pixels().enumerate() {
        for c in 0..3 {
            data[c * n + i] = normalize_channel(channel_value(p, channel, c), norm, c);
        }
    }
    data
}

/// 图像 → NHWC 行主序 f32（H,W,C），按声明的归一化与通道序处理
fn rgb_to_nhwc_with(img: &RgbImage, norm: Norm, channel: ChannelOrder) -> Vec<f32> {
    let mut data = Vec::with_capacity((img.width() * img.height() * 3) as usize);
    for p in img.pixels() {
        for c in 0..3 {
            data.push(normalize_channel(channel_value(p, channel, c), norm, c));
        }
    }
    data
}

/// 概率 mask 张量 `[1,1,H,W]`（行主序，值域 [0,1]）→ 原图尺寸灰度概率图。
/// BiRefNet 等模型输出为 logits（值域可超 [0,1]），先整体判定：全部落在 [0,1] 视为概率
/// 直接使用，否则过 sigmoid 归一化（避免负 logit 被硬钳为 0、正 logit 硬切为 255）。
/// `letterbox` 提供模型输入的画布几何：mask 是整幅 letterbox 画布，须先按逆变换裁出内容区
/// 再等比还原到原图（直接整幅 resize 会因灰边产生几何畸变、mask 与人像错位）。
pub fn probability_map(
    out: &TensorData,
    w: u32,
    h: u32,
    letterbox: Option<&LetterBox>,
) -> CoreResult<GrayImage> {
    let n = out.shape.len();
    let out_w = out.dim(n - 1).max(1) as u32;
    let out_h = out.dim(n - 2).max(1) as u32;
    let is_prob = out.data.iter().all(|&v| (0.0..=1.0).contains(&v));
    let mut prob = GrayImage::new(out_w, out_h);
    for (i, p) in prob.pixels_mut().enumerate() {
        let raw = out.data.get(i).copied().unwrap_or(0.0);
        let v = if is_prob {
            raw
        } else {
            // sigmoid：logits → 概率
            1.0 / (1.0 + (-raw).exp())
        };
        p[0] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    if let Some(lb) = letterbox {
        // 逆 letterbox：裁出等比内容区（含 pad 偏移），再缩放到原图尺寸
        let cw = (lb.scale * w as f32).round().max(1.0) as u32;
        let ch = (lb.scale * h as f32).round().max(1.0) as u32;
        let cx = lb.pad_x.max(0.0) as u32;
        let cy = lb.pad_y.max(0.0) as u32;
        let sub = image::imageops::crop_imm(&prob, cx, cy, cw.min(out_w - cx), ch.min(out_h - cy))
            .to_image();
        Ok(image::imageops::resize(
            &sub,
            w,
            h,
            image::imageops::FilterType::Triangle,
        ))
    } else if prob.dimensions() != (w, h) {
        Ok(image::imageops::resize(
            &prob,
            w,
            h,
            image::imageops::FilterType::Triangle,
        ))
    } else {
        Ok(prob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 w×h 纯色 RGB 图（左上角标记像素便于定位）
    fn solid(w: u32, h: u32) -> RgbImage {
        RgbImage::from_pixel(w, h, image::Rgb([200, 100, 50]))
    }

    #[test]
    fn letterbox等比缩放灰边填充() {
        // 100x140 → 640x640：scale = 640/140 ≈ 4.571，缩放后 457x640，pad_x = (640-457)/2 ≈ 91
        let img = solid(100, 140);
        let (canvas, lb) = letterbox(&img, 640, 640);
        assert_eq!(canvas.dimensions(), (640, 640));
        assert!((lb.scale - 640.0 / 140.0).abs() < 1e-4);
        assert!((lb.pad_y - 0.0).abs() < 1e-3); // 高度恰好充满
        assert_eq!(lb.pad_x, 91.0); // (640-457)/2 = 91.5 向下取整
        // 灰边像素保持填充色
        assert_eq!(*canvas.get_pixel(0, 0), image::Rgb([114, 114, 114]));
        assert_eq!(*canvas.get_pixel(639, 639), image::Rgb([114, 114, 114]));
        // 内容区（中上，等比缩放后的左上区域）为原像素色
        assert_eq!(*canvas.get_pixel(91, 0), image::Rgb([200, 100, 50]));
    }

    #[test]
    fn nchw布局输入() {
        let img = solid(100, 140);
        let dims = vec![1, 3, 640, 640];
        let mi = build_input(&img, &dims, false).unwrap();
        assert_eq!(mi.tensor.shape, dims);
        assert_eq!(mi.tensor.data.len(), 640 * 640 * 3);
        // letterbox 逆变换参数已返回
        let lb = mi.letterbox.unwrap();
        assert!((lb.scale - 640.0 / 140.0).abs() < 1e-4);
        // 归一化：纯色像素 R=200 → 200/255，且 NCHW 布局下前 N 个元素均为 R 通道
        let n = 640 * 640;
        // NCHW 布局：C,H,W 行主序；灰边像素 R=114/255，内容区（缩放后左上）R=200/255
        assert!((mi.tensor.data[0] - 114.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[91] - 200.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[n + 91] - 100.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[2 * n + 91] - 50.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn nhwc布局输入() {
        let img = solid(200, 100);
        let dims = vec![1, 192, 192, 3];
        let mi = build_input(&img, &dims, false).unwrap();
        assert_eq!(mi.tensor.shape, dims);
        assert_eq!(mi.tensor.data.len(), 192 * 192 * 3);
        // NHWC 无需坐标逆变换
        assert!(mi.letterbox.is_none());
        // NHWC 布局：单像素三通道相邻（R,G,B 顺序）
        assert!((mi.tensor.data[0] - 200.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[1] - 100.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[2] - 50.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn 非法布局报错() {
        let img = solid(10, 10);
        assert!(build_input(&img, &[1, 3], false).is_err());
        assert!(build_input(&img, &[1, 3, 4, 5, 6], false).is_err());
    }

    #[test]
    fn retinaface预处理() {
        let img = solid(4, 1);
        let dims = vec![1, 3, 1, 4];
        let mi = build_input(&img, &dims, true).unwrap();
        // 4x1 → letterbox 到 1x4：内容像素落在 canvas(0,1)（data[1]）
        // 纯色 R=200 G=100 B=50，RGB 减均值 (104,117,123)
        assert!((mi.tensor.data[1] - (200.0 - 104.0)).abs() < 1e-6);
        assert!((mi.tensor.data[5] - (100.0 - 117.0)).abs() < 1e-6);
        assert!((mi.tensor.data[9] - (50.0 - 123.0)).abs() < 1e-6);
        // 灰边像素 114 减均值后为 10（R 通道）
        assert!((mi.tensor.data[0] - (114.0 - 104.0)).abs() < 1e-6);
        // 与普通归一化输入不同：不使用 /255 归一化
        let mi0 = build_input(&img, &dims, false).unwrap();
        assert!((mi0.tensor.data[1] - 200.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn 概率图尺寸对齐() {
        // 输出 2x2 概率，目标 4x4 → resize 回 4x4
        let t = TensorData::new(vec![1, 1, 2, 2], vec![1.0, 1.0, 1.0, 1.0]).unwrap();
        let m = probability_map(&t, 4, 4, None).unwrap();
        assert_eq!(m.dimensions(), (4, 4));
        assert!(m.pixels().all(|p| p[0] >= 250)); // 全 1 → 全 255
        // 输出与原图同尺寸 → 不 resize
        let t2 = TensorData::new(vec![1, 1, 2, 2], vec![0.5, 0.5, 0.5, 0.5]).unwrap();
        let m2 = probability_map(&t2, 2, 2, None).unwrap();
        assert_eq!(m2.dimensions(), (2, 2));
        assert!(m2.pixels().all(|p| p[0] == 128));
        // 超 [0,1] 值视为 logits → sigmoid(2.0) ≈ 0.8808 → 225
        let t3 = TensorData::new(vec![1, 1, 1, 1], vec![2.0]).unwrap();
        let m3 = probability_map(&t3, 1, 1, None).unwrap();
        assert_eq!(m3.get_pixel(0, 0)[0], 225);
    }

    #[test]
    fn logits输出先过sigmoid() {
        // BiRefNet logits：负 → 接近 0，正 → 接近 255（sigmoid 后按概率量化）
        let t = TensorData::new(vec![1, 1, 2, 2], vec![-3.0, 3.0, 0.0, 0.5]).unwrap();
        let m = probability_map(&t, 2, 2, None).unwrap();
        let s = |v: f32| (1.0 / (1.0 + (-v).exp()) * 255.0).round() as u8;
        assert_eq!(m.get_pixel(0, 0)[0], s(-3.0));
        assert_eq!(m.get_pixel(1, 0)[0], s(3.0));
        assert_eq!(m.get_pixel(0, 1)[0], s(0.0)); // sigmoid(0) = 0.5 → 128
        assert_eq!(m.get_pixel(1, 1)[0], s(0.5));
        // 全概率输入不受影响
        let t2 = TensorData::new(vec![1, 1, 1, 1], vec![0.8]).unwrap();
        let m2 = probability_map(&t2, 1, 1, None).unwrap();
        assert_eq!(m2.get_pixel(0, 0)[0], 204);
    }

    #[test]
    fn letterbox画布逆变换还原mask() {
        // 模型输入 100x100 letterbox（scale 0.5, pad_y 25）：内容区 50x50 在画布中央，前景全 1
        let mut img = GrayImage::new(50, 50);
        for p in img.pixels_mut() {
            p[0] = 255;
        }
        let mut canvas = GrayImage::from_pixel(100, 100, image::Luma([0u8]));
        image::imageops::replace(&mut canvas, &img, 0, 25);
        let t = TensorData::new(
            vec![1, 1, 100, 100],
            canvas.pixels().map(|p| p[0] as f32 / 255.0).collect(),
        )
        .unwrap();
        let lb = LetterBox {
            scale: 0.5,
            pad_x: 0.0,
            pad_y: 25.0,
        };
        let m = probability_map(&t, 100, 50, Some(&lb)).unwrap();
        assert_eq!(m.dimensions(), (100, 50));
        // 逆变换后前景应铺满原图（内容区 50x50 还原到 100x50）
        assert!(m.pixels().all(|p| p[0] >= 250), "前景未铺满原图");
    }

    /// 4x1 纯色图 letterbox 到 1x4：内容像素落在第 1 行，灰边落在第 0 行
    fn 小图输入(pp: &Preprocess) -> ModelInput {
        let img = solid(4, 1);
        build_input_with(&img, &[1, 3, 1, 4], pp).unwrap()
    }

    #[test]
    fn 声明式bgr通道序() {
        let pp = Preprocess {
            layout: Layout::Auto,
            resize: ResizeMode::Letterbox,
            norm: Norm::Unit,
            channel: ChannelOrder::Bgr,
        };
        let mi = 小图输入(&pp);
        // BGR：通道 0 取原图蓝(50)，通道 2 取原图红(200)
        assert!((mi.tensor.data[1] - 50.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[5] - 100.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[9] - 200.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn 声明式不归一化保留原始值() {
        let pp = Preprocess {
            layout: Layout::Auto,
            resize: ResizeMode::Letterbox,
            norm: Norm::None,
            channel: ChannelOrder::Rgb,
        };
        let mi = 小图输入(&pp);
        assert!((mi.tensor.data[1] - 200.0).abs() < 1e-6);
        assert!((mi.tensor.data[5] - 100.0).abs() < 1e-6);
        assert!((mi.tensor.data[9] - 50.0).abs() < 1e-6);
        // 灰边填充色 114 同样不做归一化
        assert!((mi.tensor.data[0] - 114.0).abs() < 1e-6);
    }

    #[test]
    fn 声明式减均值除标准差() {
        let pp = Preprocess {
            layout: Layout::Auto,
            resize: ResizeMode::Letterbox,
            norm: Norm::MeanStd {
                mean: [104.0, 117.0, 123.0],
                std: [58.0, 58.0, 58.0],
            },
            channel: ChannelOrder::Rgb,
        };
        let mi = 小图输入(&pp);
        assert!((mi.tensor.data[1] - (200.0 - 104.0) / 58.0).abs() < 1e-6);
        assert!((mi.tensor.data[5] - (100.0 - 117.0) / 58.0).abs() < 1e-6);
        assert!((mi.tensor.data[9] - (50.0 - 123.0) / 58.0).abs() < 1e-6);
    }

    #[test]
    fn 声明式nhwc按最后一维通道处理() {
        let pp = Preprocess {
            layout: Layout::Nhwc,
            resize: ResizeMode::Letterbox,
            norm: Norm::Unit,
            channel: ChannelOrder::Bgr,
        };
        let img = solid(200, 100);
        let mi = build_input_with(&img, &[1, 192, 192, 3], &pp).unwrap();
        assert_eq!(mi.tensor.shape, vec![1, 192, 192, 3]);
        assert!(mi.letterbox.is_none());
        // NHWC 直接 resize，不产生灰边；单像素 BGR 顺序
        assert!((mi.tensor.data[0] - 50.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[1] - 100.0 / 255.0).abs() < 1e-6);
        assert!((mi.tensor.data[2] - 200.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn 声明式拉伸缩放无灰边() {
        // 4x1 → 拉伸到 1x4：无灰边、无逆变换，mean_std 归一化按 0-255 值域仿射生效
        let pp = Preprocess {
            layout: Layout::Auto,
            resize: ResizeMode::Stretch,
            norm: Norm::MeanStd {
                mean: [127.5, 127.5, 127.5],
                std: [127.5, 127.5, 127.5],
            },
            channel: ChannelOrder::Rgb,
        };
        let img = solid(4, 1);
        let mi = build_input_with(&img, &[1, 3, 1, 4], &pp).unwrap();
        // 拉伸输入无 letterbox 逆变换（mask 输出与原图同一几何映射）
        assert!(mi.letterbox.is_none());
        // NCHW：通道 0 为 R、通道 1 为 G、通道 2 为 B（各 4 像素连块）
        // R=200 → (200−127.5)/127.5 ≈ 0.5686（等价于 (200/255 − 0.5)/0.5）
        assert!((mi.tensor.data[0] - (200.0 - 127.5) / 127.5).abs() < 1e-6);
        assert!((mi.tensor.data[4] - (100.0 - 127.5) / 127.5).abs() < 1e-6);
        assert!((mi.tensor.data[8] - (50.0 - 127.5) / 127.5).abs() < 1e-6);
    }

    #[test]
    fn 声明式布局与维度矛盾时报错() {
        // nhwc 声明配 NCHW 维度：按 dims[1]/dims[2] 当宽高会产出与 shape 不符的数据
        let img = solid(10, 10);
        let pp_nhwc = Preprocess {
            layout: Layout::Nhwc,
            ..Preprocess::default()
        };
        assert!(build_input_with(&img, &[1, 3, 640, 640], &pp_nhwc).is_err());
        // nchw 声明配 NHWC 维度同理
        let pp_nchw = Preprocess {
            layout: Layout::Nchw,
            ..Preprocess::default()
        };
        assert!(build_input_with(&img, &[1, 192, 192, 3], &pp_nchw).is_err());
    }
}
