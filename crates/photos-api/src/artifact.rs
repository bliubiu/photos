//! 产物定位与打包下载：从任务记录 `outputs`（完整路径 JSON 数组）解析产物清单，
//! 供 `GET /tasks/{id}` 的 `artifacts` 与 `GET /tasks/{id}/output` 下载定位使用。

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::Result;
use zip::write::SimpleFileOptions;

/// 单个产物（文件名与落盘路径）
#[derive(Debug, Clone)]
pub struct Artifact {
    /// kind：id_photo | effect | layout（契约 §1.2）
    pub kind: &'static str,
    /// 底色 id（id_photo / effect 时）
    pub background: Option<String>,
    /// 排版 id（layout 时，如 6inch | a4）
    pub layout: Option<String>,
    /// 文件名（与契约 artifacts[].filename 一致，同时供下载定位）
    pub filename: String,
    /// 落盘完整路径
    pub path: PathBuf,
}

/// 解析任务记录 outputs（完整路径 JSON 数组）→ 产物清单
pub fn parse_outputs(outputs_json: &str) -> Vec<Artifact> {
    let paths: Vec<String> = serde_json::from_str(outputs_json).unwrap_or_default();
    paths
        .iter()
        .filter_map(|p| {
            let path = PathBuf::from(p);
            let filename = path.file_name()?.to_string_lossy().to_string();
            Some(parse_filename(&path, &filename))
        })
        .collect()
}

/// 按命名规约解析单个文件名 → 产物
fn parse_filename(path: &Path, filename: &str) -> Artifact {
    let stem = filename
        .rsplit_once('.')
        .map(|(s, _)| s.to_string())
        .unwrap_or_else(|| filename.to_string());
    if stem.contains("_layout_") {
        let layout = stem.rsplit('_').next().map(String::from);
        Artifact {
            kind: "layout",
            background: None,
            layout,
            filename: filename.to_string(),
            path: path.to_path_buf(),
        }
    } else if stem.contains("_effect_") {
        let bg = stem.rsplit('_').next().map(String::from);
        Artifact {
            kind: "effect",
            background: bg,
            layout: None,
            filename: filename.to_string(),
            path: path.to_path_buf(),
        }
    } else {
        let bg = stem.rsplit('_').next().map(String::from);
        Artifact {
            kind: "id_photo",
            background: bg,
            layout: None,
            filename: filename.to_string(),
            path: path.to_path_buf(),
        }
    }
}

/// 按下载查询（artifact / background / layout）挑选产物
pub fn pick<'a>(
    artifacts: &'a [Artifact],
    artifact: &str,
    background: Option<&str>,
    layout: Option<&str>,
) -> Option<&'a Artifact> {
    match artifact {
        "id_photo" => artifacts
            .iter()
            .find(|a| a.kind == "id_photo" && a.background.as_deref() == background),
        "layout" => artifacts
            .iter()
            .find(|a| a.kind == "layout" && a.layout.as_deref() == layout),
        "effect" => artifacts.iter().find(|a| a.kind == "effect"),
        _ => None,
    }
}

/// 将任务全部产物打包为 zip 字节（bundle 下载）
pub fn bundle_zip(artifacts: &[Artifact]) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for a in artifacts {
            let bytes = std::fs::read(&a.path)?;
            zip.start_file(a.filename.clone(), options)?;
            zip.write_all(&bytes)?;
        }
        zip.finish()?;
    }
    Ok(buf.into_inner())
}

/// 产物媒体类型（按扩展名；默认 application/octet-stream）
pub fn content_type(filename: &str) -> &'static str {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "bmp" => "image/bmp",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

use std::io::Write;

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(filename: &str) -> Artifact {
        parse_filename(Path::new(&format!("data/out/{filename}")), filename)
    }

    #[test]
    fn 解析三类产物() {
        let a = artifact("task_1_one_inch_white.jpg");
        assert_eq!(a.kind, "id_photo");
        assert_eq!(a.background.as_deref(), Some("white"));

        let e = artifact("task_1_effect_blue.jpg");
        assert_eq!(e.kind, "effect");
        assert_eq!(e.background.as_deref(), Some("blue"));

        let l = artifact("task_1_layout_6inch.jpg");
        assert_eq!(l.kind, "layout");
        assert_eq!(l.layout.as_deref(), Some("6inch"));
    }

    #[test]
    fn 按查询挑选产物() {
        let arts = vec![
            artifact("task_1_one_inch_white.jpg"),
            artifact("task_1_one_inch_blue.jpg"),
            artifact("task_1_effect_white.jpg"),
            artifact("task_1_layout_6inch.jpg"),
        ];
        assert_eq!(
            pick(&arts, "id_photo", Some("blue"), None)
                .unwrap()
                .filename,
            "task_1_one_inch_blue.jpg"
        );
        assert_eq!(
            pick(&arts, "layout", None, Some("6inch")).unwrap().filename,
            "task_1_layout_6inch.jpg"
        );
        assert_eq!(
            pick(&arts, "effect", None, None).unwrap().filename,
            "task_1_effect_white.jpg"
        );
        assert!(pick(&arts, "id_photo", Some("red"), None).is_none());
        assert!(pick(&arts, "bundle", None, None).is_none());
    }

    #[test]
    fn bundle打包全部产物() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("task_1_one_inch_white.jpg");
        let p2 = dir.path().join("task_1_layout_6inch.jpg");
        std::fs::write(&p1, b"jpeg-bytes").unwrap();
        std::fs::write(&p2, b"layout-bytes").unwrap();
        let arts = vec![
            Artifact {
                kind: "id_photo",
                background: Some("white".into()),
                layout: None,
                filename: "task_1_one_inch_white.jpg".into(),
                path: p1,
            },
            Artifact {
                kind: "layout",
                background: None,
                layout: Some("6inch".into()),
                filename: "task_1_layout_6inch.jpg".into(),
                path: p2,
            },
        ];
        let bytes = bundle_zip(&arts).unwrap();
        assert!(bytes.len() > 4);
        // zip 魔数 PK\x03\x04
        assert_eq!(&bytes[..2], b"PK");
    }
}
