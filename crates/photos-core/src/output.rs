//! 产物输出：图片格式编码（JPG 质量可调 / WebP）、透明底 PNG 与排版 PDF 写出，以及
//! 任务多产物落盘（命名规约集中于此，CLI 与 API 共用）。
//!
//! 设计约束：图片编码一律使用 `image` 的纯 Rust 编码器（WebP 为 VP8L **无损**，不引入
//! 任何 C/C++ 绑定）；PDF 由本模块手写最小单页文档（DCTDecode 直接嵌入 JPEG 字节），
//! 不新增第三方依赖。

use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbImage, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::config::{Config, LayoutSpec};
use crate::error::{CoreError, CoreResult};
use crate::pipeline::PipelineResult;

/// JPG 默认压缩质量
pub const DEFAULT_JPG_QUALITY: u8 = 90;

/// 图片产物格式（透明底固定 PNG，不参与本枚举）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// JPEG（有损，质量可调）
    #[default]
    Jpg,
    /// WebP（VP8L 无损，体积通常大于同图 JPG）
    Webp,
}

impl OutputFormat {
    /// 解析格式 id（`jpg` | `jpeg` | `webp`，大小写不敏感）
    pub fn parse(s: &str) -> CoreResult<Self> {
        match s.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Ok(Self::Jpg),
            "webp" => Ok(Self::Webp),
            other => Err(CoreError::ConfigValidate(format!(
                "未知输出格式“{other}”，可选：jpg、webp"
            ))),
        }
    }

    /// 文件扩展名
    pub fn ext(self) -> &'static str {
        match self {
            Self::Jpg => "jpg",
            Self::Webp => "webp",
        }
    }
}

/// 落盘选项（格式 / JPG 质量 / 是否额外输出排版 PDF）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputOptions {
    /// 图片格式
    pub format: OutputFormat,
    /// JPG 压缩质量（1..=100；WebP 为无损编码，不受此项影响）
    pub jpg_quality: u8,
    /// 排版相纸是否额外输出 PDF（需同时指定 `layout`）
    pub pdf: bool,
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Jpg,
            jpg_quality: DEFAULT_JPG_QUALITY,
            pdf: false,
        }
    }
}

impl OutputOptions {
    /// 取配置 `[output]` 默认值
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            format: cfg.output.format,
            jpg_quality: cfg.output.jpg_quality,
            pdf: cfg.output.pdf,
        }
    }

    /// 校验质量取值（1..=100）
    pub fn validate(&self) -> CoreResult<()> {
        if !(1..=100).contains(&self.jpg_quality) {
            return Err(CoreError::ConfigValidate(format!(
                "JPG 压缩质量需在 1..=100 内，收到 {}",
                self.jpg_quality
            )));
        }
        Ok(())
    }
}

/// 按格式与质量编码 RGB 图（`quality` 仅对 JPG 生效）
pub fn encode_rgb(img: &RgbImage, format: OutputFormat, quality: u8) -> CoreResult<Vec<u8>> {
    let (w, h) = img.dimensions();
    let mut buf: Vec<u8> = Vec::new();
    match format {
        OutputFormat::Jpg => {
            JpegEncoder::new_with_quality(&mut buf, quality)
                .encode_image(img)
                .map_err(|e| CoreError::Image(format!("JPG 编码失败：{e}")))?;
        }
        OutputFormat::Webp => {
            WebPEncoder::new_lossless(&mut buf)
                .write_image(img.as_raw(), w, h, ExtendedColorType::Rgb8)
                .map_err(|e| CoreError::Image(format!("WebP 编码失败：{e}")))?;
        }
    }
    Ok(buf)
}

/// 编码透明底 PNG（RGBA）
pub fn encode_rgba_png(img: &RgbaImage) -> CoreResult<Vec<u8>> {
    let (w, h) = img.dimensions();
    let mut buf: Vec<u8> = Vec::new();
    PngEncoder::new(&mut buf)
        .write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8)
        .map_err(|e| CoreError::Image(format!("PNG 编码失败：{e}")))?;
    Ok(buf)
}

/// 毫米转 PDF 用户单位（pt，1pt = 1/72 英寸）
fn mm_to_pt(mm: f64) -> f64 {
    mm * 72.0 / 25.4
}

/// 自研最小单页 PDF：整页 JPEG 以 DCTDecode 图像对象嵌入，页面尺寸按相纸物理毫米设定。
/// 打印店可直接按原始物理尺寸出图（图像不重采样、不二次压缩）。
pub fn layout_pdf(jpeg: &[u8], px_w: u32, px_h: u32, page_w_mm: f64, page_h_mm: f64) -> Vec<u8> {
    let w_pt = mm_to_pt(page_w_mm);
    let h_pt = mm_to_pt(page_h_mm);
    let content = format!("q {w_pt:.2} 0 0 {h_pt:.2} 0 0 cm /Im0 Do Q\n");

    let mut out: Vec<u8> = Vec::with_capacity(jpeg.len() + 1024);
    // 对象 1..=5（目录 / 页面树 / 页面 / 图像 / 内容流）的字节偏移
    let mut offsets: Vec<usize> = Vec::with_capacity(5);

    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    offsets.push(out.len());
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w_pt:.2} {h_pt:.2}] \
             /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n"
        )
        .as_bytes(),
    );

    offsets.push(out.len());
    out.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {px_w} /Height {px_h} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(jpeg);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(format!("5 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    out.extend_from_slice(content.as_bytes());
    out.extend_from_slice(b"endstream\nendobj\n");

    // 交叉引用表（每条固定 20 字节：10 位偏移 + 空格 + 5 位代次 + 空格 + 类型 + 空格 + 换行）
    let xref_off = out.len();
    let total = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {total}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// 排版相纸 → PDF 字节（页面按相纸物理毫米尺寸设定，内嵌按 `quality` 编码的 JPEG）
pub fn layout_pdf_bytes(
    layout: &LayoutSpec,
    canvas: &RgbImage,
    quality: u8,
) -> CoreResult<Vec<u8>> {
    let jpeg = encode_rgb(canvas, OutputFormat::Jpg, quality)?;
    Ok(layout_pdf(
        &jpeg,
        canvas.width(),
        canvas.height(),
        layout.width_mm,
        layout.height_mm,
    ))
}

/// 写文件（统一中文错误消息）
fn write_file(path: &Path, bytes: &[u8]) -> CoreResult<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

/// 保存单个任务的全部产物，返回产物路径列表。
///
/// 命名规约：证件照 `task_{id}_{size}_{底色}.{ext}`、效果图 `task_{id}_effect_{底色}.{ext}`、
/// 排版 `task_{id}_layout_{相纸}.{ext}`（`opts.pdf` 为真且给出 `layout` 规格时额外输出同名
/// `.pdf`）、透明底固定 `task_{id}_{size}_transparent.png`。
#[allow(clippy::too_many_arguments)]
pub fn save_task_outputs(
    dir: &Path,
    task_id: i64,
    size: &str,
    layout: Option<&LayoutSpec>,
    layout_id: Option<&str>,
    result: &PipelineResult,
    opts: &OutputOptions,
) -> CoreResult<Vec<PathBuf>> {
    opts.validate()?;
    std::fs::create_dir_all(dir)?;
    let ext = opts.format.ext();
    let mut outputs: Vec<PathBuf> = Vec::new();

    for photo in &result.photos {
        let path = dir.join(format!("task_{task_id}_{size}_{}.{ext}", photo.bg));
        write_file(
            &path,
            &encode_rgb(&photo.image, opts.format, opts.jpg_quality)?,
        )?;
        outputs.push(path);
    }
    for eff in &result.effects {
        let path = dir.join(format!("task_{task_id}_effect_{}.{ext}", eff.bg));
        write_file(
            &path,
            &encode_rgb(&eff.image, opts.format, opts.jpg_quality)?,
        )?;
        outputs.push(path);
    }
    if let Some(canvas) = &result.layout {
        let id = layout_id.unwrap_or("layout");
        let path = dir.join(format!("task_{task_id}_layout_{id}.{ext}"));
        write_file(&path, &encode_rgb(canvas, opts.format, opts.jpg_quality)?)?;
        outputs.push(path);
        if opts.pdf {
            if let Some(spec) = layout {
                let path = dir.join(format!("task_{task_id}_layout_{id}.pdf"));
                write_file(&path, &layout_pdf_bytes(spec, canvas, opts.jpg_quality)?)?;
                outputs.push(path);
            }
        }
    }
    if let Some(rgba) = &result.transparent {
        let path = dir.join(format!("task_{task_id}_{size}_transparent.png"));
        write_file(&path, &encode_rgba_png(rgba)?)?;
        outputs.push(path);
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn solid(w: u32, h: u32) -> RgbImage {
        RgbImage::from_pixel(w, h, Rgb([200, 40, 60]))
    }

    #[test]
    fn 格式解析与扩展名() {
        assert_eq!(OutputFormat::parse("JPG").unwrap(), OutputFormat::Jpg);
        assert_eq!(OutputFormat::parse("jpeg").unwrap(), OutputFormat::Jpg);
        assert_eq!(OutputFormat::parse("WebP").unwrap(), OutputFormat::Webp);
        assert_eq!(OutputFormat::Webp.ext(), "webp");
        assert_eq!(OutputFormat::Jpg.ext(), "jpg");
        assert!(OutputFormat::parse("tiff").is_err());
    }

    #[test]
    fn jpg质量越低字节越少() {
        let img = solid(64, 64);
        let hi = encode_rgb(&img, OutputFormat::Jpg, 95).unwrap();
        let lo = encode_rgb(&img, OutputFormat::Jpg, 30).unwrap();
        assert!(
            hi.len() > lo.len(),
            "高质量应更大：{} vs {}",
            hi.len(),
            lo.len()
        );
        // JPEG 魔数
        assert_eq!(&hi[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn webp输出为无损且可回读() {
        let img = solid(32, 24);
        let bytes = encode_rgb(&img, OutputFormat::Webp, 90).unwrap();
        // RIFF....WEBP
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (32, 24));
        assert_eq!(decoded.get_pixel(0, 0), &Rgb([200, 40, 60]));
    }

    #[test]
    fn 透明底png保留alpha() {
        let rgba = RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 128]));
        let bytes = encode_rgba_png(&rgba).unwrap();
        assert_eq!(&bytes[..4], &[0x89, b'P', b'N', b'G']);
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0[3], 128);
    }

    #[test]
    fn pdf结构含目录页面与图像对象() {
        let jpeg = encode_rgb(&solid(20, 10), OutputFormat::Jpg, 90).unwrap();
        let pdf = layout_pdf(&jpeg, 20, 10, 152.0, 102.0);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/Subtype /Image"));
        assert!(text.contains("/Filter /DCTDecode"));
        // 152mm × 102mm → 430.87pt × 289.13pt
        assert!(text.contains("/MediaBox [0 0 430.87 289.13]"));
        assert!(text.ends_with("%%EOF\n"));
        // 起始交叉引用偏移指向 "xref"
        let start: usize = text
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(&pdf[start..start + 4], b"xref");
    }

    #[test]
    fn 质量越界被拦截() {
        let opts = OutputOptions {
            jpg_quality: 0,
            ..Default::default()
        };
        assert!(opts.validate().is_err());
        let opts = OutputOptions {
            jpg_quality: 101,
            ..Default::default()
        };
        assert!(opts.validate().is_err());
    }
}
