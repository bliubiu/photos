//! 排版引擎：将证件照小图按相纸规格（6 寸 / A4）铺版成整版相纸。
//!
//! 规则：边距 + 间距 + 行列居中；行列数向下取整，证件照必能放下至少一张。

use image::{Rgb, RgbImage};

use crate::config::LayoutSpec;
use crate::error::{CoreError, CoreResult};

/// 毫米转像素（每毫米像素数 = dpi / 25.4）
pub fn mm_to_px(mm: f64, dpi: u32) -> u32 {
    (mm * dpi as f64 / 25.4).round() as u32
}

/// 相纸像素尺寸（宽, 高）
pub fn paper_px(layout: &LayoutSpec, dpi: u32) -> (u32, u32) {
    (
        mm_to_px(layout.width_mm, dpi),
        mm_to_px(layout.height_mm, dpi),
    )
}

/// 计算可容纳的行列数（向下取整；结果至少 1x1）
pub fn grid_size(
    photo_w: u32,
    photo_h: u32,
    paper_w: u32,
    paper_h: u32,
    margin_px: u32,
    gap_px: u32,
) -> (usize, usize) {
    if photo_w == 0 || photo_h == 0 {
        return (0, 0);
    }
    let avail_w = paper_w.saturating_sub(2 * margin_px);
    let avail_h = paper_h.saturating_sub(2 * margin_px);
    let cols = ((avail_w + gap_px) / (photo_w + gap_px)).max(1) as usize;
    let rows = ((avail_h + gap_px) / (photo_h + gap_px)).max(1) as usize;
    (cols, rows)
}

/// 铺版：将单张证件照复制铺满相纸（白底、居中、边距与间距按规格）。
/// `dpi` 决定相纸像素密度（通常取尺寸标准的 dpi，如 300）。
pub fn compose(photo: &RgbImage, layout: &LayoutSpec, dpi: u32) -> CoreResult<RgbImage> {
    if photo.width() == 0 || photo.height() == 0 {
        return Err(CoreError::Image("排版输入证件照尺寸为零".into()));
    }
    let (pw, ph) = paper_px(layout, dpi);
    if pw == 0 || ph == 0 {
        return Err(CoreError::Image("相纸像素尺寸为零".into()));
    }
    let margin = mm_to_px(layout.margin_mm, dpi);
    let gap = mm_to_px(layout.gap_mm, dpi);
    let (cols, rows) = grid_size(photo.width(), photo.height(), pw, ph, margin, gap);

    let mut canvas = RgbImage::from_pixel(pw, ph, Rgb([255, 255, 255]));
    let grid_w = cols as u32 * photo.width() + cols.saturating_sub(1) as u32 * gap;
    let grid_h = rows as u32 * photo.height() + rows.saturating_sub(1) as u32 * gap;
    let start_x = pw.saturating_sub(grid_w) / 2;
    let start_y = ph.saturating_sub(grid_h) / 2;

    for r in 0..rows {
        for c in 0..cols {
            let x = start_x + c as u32 * (photo.width() + gap);
            let y = start_y + r as u32 * (photo.height() + gap);
            image::imageops::replace(&mut canvas, photo, x as i64, y as i64);
        }
    }
    Ok(canvas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 毫米像素换算() {
        // 25.4mm @300dpi = 300px；1mm ≈ 11.81px → 12px
        assert_eq!(mm_to_px(25.4, 300), 300);
        assert_eq!(mm_to_px(1.0, 300), 12);
        // 102mm ≈ 1204.7 → 1205；152mm ≈ 1795.3 → 1795
        assert_eq!(mm_to_px(102.0, 300), 1205);
        assert_eq!(mm_to_px(152.0, 300), 1795);
    }

    #[test]
    fn 相纸像素尺寸() {
        let layout = LayoutSpec {
            name: "6寸相纸".into(),
            width_mm: 102.0,
            height_mm: 152.0,
            margin_mm: 3.0,
            gap_mm: 2.0,
        };
        assert_eq!(paper_px(&layout, 300), (1205, 1795));
    }

    #[test]
    fn 六寸排一寸照三行四列() {
        // 一寸 295x413、6 寸 1205x1795、边距 3mm(35px)、间距 2mm(24px)
        let layout = LayoutSpec {
            name: "6寸相纸".into(),
            width_mm: 102.0,
            height_mm: 152.0,
            margin_mm: 3.0,
            gap_mm: 2.0,
        };
        let (pw, ph) = paper_px(&layout, 300);
        let (cols, rows) = grid_size(295, 413, pw, ph, 35, 24);
        assert_eq!((cols, rows), (3, 4));
    }

    #[test]
    fn 放不下时至少一张() {
        // 相纸比证件照还小 → 至少 1x1
        assert_eq!(grid_size(500, 800, 400, 600, 10, 10), (1, 1));
        // 零尺寸输入返回 0
        assert_eq!(grid_size(0, 100, 1000, 1000, 10, 10), (0, 0));
    }

    #[test]
    fn 铺版画布尺寸与居中对齐() {
        let layout = LayoutSpec {
            name: "6寸相纸".into(),
            width_mm: 102.0,
            height_mm: 152.0,
            margin_mm: 3.0,
            gap_mm: 2.0,
        };
        let photo = RgbImage::from_pixel(295, 413, Rgb([10, 200, 30]));
        let canvas = compose(&photo, &layout, 300).unwrap();
        assert_eq!(canvas.dimensions(), (1205, 1795));
        // 网格居中：grid 933x1724，起点 (136, 35)
        let grid_w = 3 * 295 + 2 * 24;
        let grid_h = 4 * 413 + 3 * 24;
        let start_x = (1205 - grid_w) / 2;
        let start_y = (1795 - grid_h) / 2;
        assert_eq!((start_x, start_y), (136, 35));
        // 首张照片左上角像素为照片色，网格外像素为白色
        assert_eq!(canvas.get_pixel(start_x, start_y), &Rgb([10, 200, 30]));
        assert_eq!(
            canvas.get_pixel(start_x + 294, start_y + 412),
            &Rgb([10, 200, 30])
        );
        assert_eq!(canvas.get_pixel(0, 0), &Rgb([255, 255, 255]));
        // 相邻照片起点间距 = 照片宽 + gap
        let next_x = start_x + 295 + 24;
        assert_eq!(canvas.get_pixel(next_x, start_y), &Rgb([10, 200, 30]));
        // 最末张右下角仍在画布内（未溢出）
        let last_x = start_x + 2 * (295 + 24);
        let last_y = start_y + 3 * (413 + 24);
        assert_eq!(
            canvas.get_pixel(last_x + 294, last_y + 412),
            &Rgb([10, 200, 30])
        );
    }

    #[test]
    fn 排版输入尺寸为零报错() {
        let layout = LayoutSpec {
            name: "A4".into(),
            width_mm: 210.0,
            height_mm: 297.0,
            margin_mm: 8.0,
            gap_mm: 3.0,
        };
        let empty = RgbImage::new(0, 0);
        assert!(compose(&empty, &layout, 300).is_err());
    }
}
