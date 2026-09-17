//! 同步几何纠偏：同一个仿射矩阵同时变换原图（线性插值）与 alpha mask（最近邻），
//! 避免「先旋转再抠图」导致的两次插值损失（架构文档 §3 Step 5）。

use image::{GrayImage, Luma, Rgb, RgbImage};
use imageproc::geometric_transformations::{Interpolation, Projection, warp};

use crate::error::{CoreError, CoreResult};

/// 绕指定中心旋转的仿射投影（正角度为顺时针，图像 y 向下，与 warp 语义一致）
pub fn rotation_affine(cx: f64, cy: f64, deg: f64) -> Projection {
    let rad = deg.to_radians() as f32;
    Projection::translate(cx as f32, cy as f32)
        * Projection::rotate(rad)
        * Projection::translate(-(cx as f32), -(cy as f32))
}

/// 用同一矩阵同步变换原图（双线性）与 mask（最近邻），输出与原图同尺寸
pub fn rotate_image_same(
    img: &RgbImage,
    mask: &GrayImage,
    deg: f64,
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
    if deg == 0.0 {
        return Ok((img.clone(), mask.clone()));
    }
    let (w, h) = img.dimensions();
    let m = rotation_affine(w as f64 / 2.0, h as f64 / 2.0, deg);
    let rotated_img = warp(img, &m, Interpolation::Bilinear, Rgb([0, 0, 0]));
    let rotated_mask = warp(mask, &m, Interpolation::Nearest, Luma([0u8]));
    Ok((rotated_img, rotated_mask))
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
}
