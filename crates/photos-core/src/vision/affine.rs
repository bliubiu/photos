//! 同步几何纠偏：同一个仿射矩阵同时变换原图（线性插值）与 alpha mask（最近邻），
//! 避免「先旋转再抠图」导致的两次插值损失（架构文档 §3 Step 5）。
//! 纠偏支持「旋转 + 居中平移」复合变换：旋转（绕图像中心）后人像可能整体偏移，
//! 平移把人脸框中心送回画面中心区域（钳制上限内），一次 warp 完成、像素只插值一次。

use image::{GrayImage, Luma, Rgb, RgbImage};
use imageproc::geometric_transformations::{Interpolation, Projection, warp};

use crate::error::{CoreError, CoreResult};
use crate::vision::face::{FaceBox, FaceDetection};
use crate::vision::geometry::Point2;

/// 居中平移钳制上限 = 短边 × 该比例（相对旋转中心的最大校正量）
pub const CENTER_SHIFT_MAX_RATIO: f64 = 0.08;

/// 绕指定中心旋转的仿射投影（正角度为顺时针，图像 y 向下，与 warp 语义一致）
pub fn rotation_affine(cx: f64, cy: f64, deg: f64) -> Projection {
    let rad = deg.to_radians() as f32;
    Projection::translate(cx as f32, cy as f32)
        * Projection::rotate(rad)
        * Projection::translate(-(cx as f32), -(cy as f32))
}

/// 旋转（绕指定中心）后再整体平移的复合仿射投影
pub fn rotate_translate_affine(cx: f64, cy: f64, deg: f64, dx: f64, dy: f64) -> Projection {
    Projection::translate(dx as f32, dy as f32) * rotation_affine(cx, cy, deg)
}

/// 居中平移量：旋转后人脸框中心相对画面中心（= 旋转中心）的偏移，取负得校正平移量。
/// 平移分量任一路超过「短边 × [`CENTER_SHIFT_MAX_RATIO`]」钳制上限时返回 None（不做居中，
/// 避免把人像大量移出画面露出黑边）；人脸检测缺失时同样返回 None（无法定位主体）。
pub fn centering_shift(
    image_w: u32,
    image_h: u32,
    deg: f64,
    face_center: Point2,
) -> Option<(f64, f64)> {
    if image_w == 0 || image_h == 0 {
        return None;
    }
    let (cx, cy) = (image_w as f64 / 2.0, image_h as f64 / 2.0);
    let m = rotation_affine(cx, cy, deg);
    let (rx, ry) = m * (face_center.x as f32, face_center.y as f32);
    let dx = cx - rx as f64;
    let dy = cy - ry as f64;
    let cap = (image_w.min(image_h) as f64) * CENTER_SHIFT_MAX_RATIO;
    if dx.abs() > cap || dy.abs() > cap {
        return None;
    }
    Some((dx, dy))
}

/// 把人脸框 + 关键点变换到纠偏后坐标系（与图像变换用同一投影，保证后续掩膜/裁剪对位）
pub fn transform_face(face: &FaceDetection, m: Projection) -> FaceDetection {
    let corners = [
        (face.face.x1, face.face.y1),
        (face.face.x2, face.face.y1),
        (face.face.x1, face.face.y2),
        (face.face.x2, face.face.y2),
    ];
    let projected: Vec<(f32, f32)> = corners.into_iter().map(|p| m * p).collect();
    let x_min = projected.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    let y_min = projected.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let x_max = projected.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
    let y_max = projected.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
    let landmarks = face.landmarks.map(|p| {
        let (x, y) = m * (p.x as f32, p.y as f32);
        Point2::new(x as f64, y as f64)
    });
    FaceDetection {
        face: FaceBox {
            x1: x_min,
            y1: y_min,
            x2: x_max,
            y2: y_max,
            score: face.face.score,
        },
        landmarks,
    }
}

/// 用同一矩阵同步变换原图（双线性）与 mask（最近邻），输出与原图同尺寸
pub fn rotate_image_same(
    img: &RgbImage,
    mask: &GrayImage,
    deg: f64,
) -> CoreResult<(RgbImage, GrayImage)> {
    rotate_translate_image_same(img, mask, deg, 0.0, 0.0)
}

/// 旋转 + 居中平移后同一矩阵同步变换原图（双线性）与 mask（最近邻）
pub fn rotate_translate_image_same(
    img: &RgbImage,
    mask: &GrayImage,
    deg: f64,
    shift_x: f64,
    shift_y: f64,
) -> CoreResult<(RgbImage, GrayImage)> {
    if img.dimensions() != mask.dimensions() {
        return Err(CoreError::Image(format!(
            "原图与 mask 尺寸不一致：{}x{} vs {}x{}",
            img.width(),
            img.height(),
            mask.width(),
            mask.height()
        )));
    }
    if deg == 0.0 && shift_x == 0.0 && shift_y == 0.0 {
        return Ok((img.clone(), mask.clone()));
    }
    let (w, h) = img.dimensions();
    let m = rotate_translate_affine(
        w as f64 / 2.0,
        h as f64 / 2.0,
        deg,
        shift_x,
        shift_y,
    );
    let rotated_img = warp(img, &m, Interpolation::Bilinear, Rgb([0, 0, 0]));
    // mask 也须双线性插值：最近邻会把半透明发丝断成硬边锯齿；双线性保持 alpha 连续性
    let rotated_mask = warp(mask, &m, Interpolation::Bilinear, Luma([0u8]));
    Ok((rotated_img, rotated_mask))
}

/// 仅变换原图（双线性），用于工作流未启用抠图步骤、无 mask 可同步变换时的纠偏
pub fn rotate_image(img: &RgbImage, deg: f64) -> RgbImage {
    rotate_translate_image(img, deg, 0.0, 0.0)
}

/// 旋转 + 居中平移后仅变换原图（双线性）
pub fn rotate_translate_image(img: &RgbImage, deg: f64, shift_x: f64, shift_y: f64) -> RgbImage {
    if deg == 0.0 && shift_x == 0.0 && shift_y == 0.0 {
        return img.clone();
    }
    let (w, h) = img.dimensions();
    let m = rotate_translate_affine(
        w as f64 / 2.0,
        h as f64 / 2.0,
        deg,
        shift_x,
        shift_y,
    );
    warp(img, &m, Interpolation::Bilinear, Rgb([0, 0, 0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 中心点旋转后位置不变() {
        let p = rotation_affine(50.0, 50.0, 30.0);
        let (x, y) = p * (50.0, 50.0);
        assert!((x - 50.0).abs() < 1e-3 && (y - 50.0).abs() < 1e-3);
    }

    #[test]
    fn 旋转180度像素位置对应() {
        // 5x5 图，几何中心 (2.5,2.5)：像素 (1,1) 旋转 180° → (4,4)
        let mut img = RgbImage::new(5, 5);
        img.put_pixel(1, 1, Rgb([255, 255, 255]));
        let mask = GrayImage::from_pixel(5, 5, Luma([0u8]));
        let (rot, _) = rotate_image_same(&img, &mask, 180.0).unwrap();
        let p = rot.get_pixel(4, 4);
        assert!(p[0] > 200, "期望 (4,4) 接近白色，实际 {:?}", p);
        assert!(rot.get_pixel(1, 1)[0] < 10);
    }

    #[test]
    fn 尺寸校验() {
        let img = RgbImage::new(4, 4);
        let mask = GrayImage::new(5, 5);
        assert!(rotate_image_same(&img, &mask, 10.0).is_err());
    }

    #[test]
    fn 零角度原样返回() {
        let img = RgbImage::from_pixel(2, 2, Rgb([9, 9, 9]));
        let mask = GrayImage::from_pixel(2, 2, Luma([9u8]));
        let (i, m) = rotate_image_same(&img, &mask, 0.0).unwrap();
        assert_eq!(i, img);
        assert_eq!(m, mask);
    }

    #[test]
    fn 居中平移把人脸中心送回画面中心() {
        // 100x100 图，脸中心 (53,55) 偏离画面中心 (50,50)，距离 √(9+25)=5.83 < 8%×100 钳制上限：
        // 旋转后位置 + 平移应恰好回到画面中心
        let shift = centering_shift(100, 100, 20.0, Point2::new(53.0, 55.0));
        let (dx, dy) = shift.unwrap();
        let (rx, ry) = rotation_affine(50.0, 50.0, 20.0) * (53.0f32, 55.0f32);
        assert!((rx as f64 + dx - 50.0).abs() < 1e-2);
        assert!((ry as f64 + dy - 50.0).abs() < 1e-2);
        // 人脸不在中心：校正量非零
        assert!(dx.abs() > 1e-3 || dy.abs() > 1e-3);
    }

    #[test]
    fn 人脸中心就是画面中心时不平移() {
        let shift = centering_shift(100, 100, 30.0, Point2::new(50.0, 50.0));
        let (dx, dy) = shift.unwrap();
        assert!(dx.abs() < 1e-3 && dy.abs() < 1e-3);
    }

    #[test]
    fn 平移超钳制上限拒绝居中() {
        // 脸中心 (20,20) 远离画面中心，绕中心旋转 90° 后偏移量大概率超 8% 短边上限
        assert!(centering_shift(100, 100, 90.0, Point2::new(20.0, 20.0)).is_none());
        // 零尺寸图无法居中
        assert!(centering_shift(0, 100, 90.0, Point2::new(20.0, 20.0)).is_none());
    }

    #[test]
    fn 人脸框随纠偏矩阵同步变换() {
        let face = FaceDetection {
            face: FaceBox {
                x1: 40.0,
                y1: 40.0,
                x2: 80.0,
                y2: 80.0,
                score: 0.99,
            },
            landmarks: [
                Point2::new(50.0, 50.0),
                Point2::new(70.0, 50.0),
                Point2::new(60.0, 60.0),
                Point2::new(50.0, 70.0),
                Point2::new(70.0, 70.0),
            ],
        };
        let m = rotation_affine(60.0, 60.0, 90.0);
        let t = transform_face(&face, m);
        // 绕 (60,60) 顺时针 90°：(x,y) → (60-(y-60), 60+(x-60)) = (120-y, x)
        // 角点 (40,40) → (80,40)、(80,40) → (80,80)、(40,80) → (40,40)、(80,80) → (40,80)
        // 包络框 (40,40)-(80,80)
        assert!((t.face.x1 - 40.0).abs() < 1e-3 && (t.face.y1 - 40.0).abs() < 1e-3);
        assert!((t.face.x2 - 80.0).abs() < 1e-3 && (t.face.y2 - 80.0).abs() < 1e-3);
        assert_eq!(t.face.score, 0.99);
        // 关键点同步变换：左眼 (50,50) → (70,50)
        assert!(
            (t.landmarks[0].x - 70.0).abs() < 1e-3 && (t.landmarks[0].y - 50.0).abs() < 1e-3
        );
    }

    #[test]
    fn 旋转加平移单矩阵一次完成() {
        // 5x5 图，中心 (2.5,2.5)：像素 (1,1) 旋转 180° → (4,4)，再平移 (-1,0) → (3,4)
        let mut img = RgbImage::new(5, 5);
        img.put_pixel(1, 1, Rgb([255, 255, 255]));
        let mask = GrayImage::from_pixel(5, 5, Luma([0u8]));
        let (rot, _) = rotate_translate_image_same(&img, &mask, 180.0, -1.0, 0.0).unwrap();
        assert!(rot.get_pixel(3, 4)[0] > 200, "(3,4) 应为白");
        assert!(rot.get_pixel(4, 4)[0] < 10, "(4,4) 应保持黑");
        assert!(rot.get_pixel(4, 3)[0] < 10, "(4,3) 应保持黑");
    }

    #[test]
    fn 复合仿射先旋后移() {
        // 复合 = 先旋后移。绕 (50,50) 转 90° 后平移 (+10,0)：
        // 验证「先旋转」尾序成立——对比复合结果与「旋转仿射结果再只平移」一致
        let m = rotate_translate_affine(50.0, 50.0, 90.0, 10.0, 0.0);
        let (x, y) = m * (60.0f32, 60.0f32);
        let (rx, ry) = rotation_affine(50.0, 50.0, 90.0) * (60.0f32, 60.0f32);
        assert!((x as f64 - (rx as f64 + 10.0)).abs() < 1e-3);
        assert!((y as f64 - ry as f64).abs() < 1e-3);
    }
}
