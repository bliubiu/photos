//! `photos process`：单图 / 文件夹批量证件照处理（M2 多产物）。
//!
//! 一次请求产出：每底色各一张证件照（1..N）+ 可选通用效果图 + 可选排版相纸。
//! 输出命名规约：`task_{id}_{size}_{bg}.jpg` / `task_{id}_effect_{bg}.jpg` / `task_{id}_layout_{相纸}.jpg`。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use photos_core::config::Config;
use photos_core::inference::InferenceEngine;
use photos_core::pipeline::{
    BeautyParams, DressParams, GarmentSet, ProcessRequest, demo_balanced_engine, run_pipeline,
};
use photos_core::storage::{NewTask, Store};

use crate::cli::ProcessArgs;
use crate::commands::open_store;

/// 处理入口：解析参数 → 收集输入文件 → 逐个处理（真实引擎复用一次装载）
pub fn run(cfg: &Config, args: &ProcessArgs) -> Result<()> {
    let mode = args
        .mode
        .clone()
        .unwrap_or_else(|| cfg.general.default_mode.clone());

    let files = collect_images(&args.input)?;
    if files.is_empty() {
        bail!("未找到可处理的图片：{}", args.input.display());
    }

    // 真实引擎一次装载复用；demo 引擎按图尺寸构造（FakeEngine 无装载成本）
    let mut real_engine: Option<Box<dyn InferenceEngine>> = None;
    if !args.demo {
        // 禁止静默 demo：无 ort 时直接失败（--demo 才走 mock 回放）
        if !photos_core::inference::ORT_BUILT {
            anyhow::bail!(
                "当前构建未启用 ONNX 推理，无法真实处理图片。\n\
                 启用真实推理：cargo build -p photos-cli（默认 features 含 ort）\n\
                 或显式：cargo build -p photos-cli --features photos-core/ort\n\
                 或显式演示：photos process --demo"
            );
        }
        let mut engine = photos_core::inference::default_engine();
        photos_core::inference::ensure_models_ready(cfg, engine.as_mut(), &mode).context(
            "模型检查未通过：请按 docs/04-模型清单.md §6 放置模型文件到 models/ 目录，或启用一键下载",
        )?;
        real_engine = Some(engine);
    }

    let store = open_store(cfg)?;
    let out_dir = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(&cfg.general.data_dir).join("out"));
    std::fs::create_dir_all(&out_dir)?;

    let mut ok_count = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut costs: Vec<u128> = Vec::new();
    for input in &files {
        let t0 = std::time::Instant::now();
        let res = if args.demo {
            let (w, h) = image::image_dimensions(input)
                .with_context(|| format!("读取图片尺寸失败：{}", input.display()))?;
            // 与流水线内部预缩放对齐：demo 引擎按缩放后尺寸构造
            let (w, h) =
                photos_core::pipeline::limited_dimensions(w, h, cfg.general.max_input_side);
            let mut engine = demo_balanced_engine(w, h);
            process_one(cfg, args, &store, &out_dir, input, &mut engine)
        } else {
            process_one(
                cfg,
                args,
                &store,
                &out_dir,
                input,
                real_engine.as_deref_mut().expect("真实引擎已装载"),
            )
        };
        match res {
            Ok(()) => {
                ok_count += 1;
                costs.push(t0.elapsed().as_millis());
            }
            Err(e) => errors.push(format!("{}：{e:#}", input.display())),
        }
    }

    for e in &errors {
        eprintln!("处理失败：{e}");
    }
    let total_ms: u128 = costs.iter().sum();
    let avg_ms = if costs.is_empty() {
        0
    } else {
        total_ms / costs.len() as u128
    };
    let rate = files.len() as f64;
    let success_rate = if rate > 0.0 {
        ok_count as f64 / rate * 100.0
    } else {
        0.0
    };
    println!(
        "批量处理完成：成功 {ok_count} / {}（成功率 {success_rate:.1}%），失败 {}，总耗时 {total_ms}ms，平均 {avg_ms}ms/张",
        files.len(),
        errors.len()
    );
    if !errors.is_empty() {
        bail!("存在处理失败的文件，详见上方错误输出");
    }
    Ok(())
}

/// 解析底色列表（逗号分隔，去空白；空列表报错）
fn parse_backgrounds(raw: &Option<String>) -> Result<Vec<String>> {
    let bgs: Vec<String> = match raw {
        Some(s) => s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        None => vec!["white".to_string()],
    };
    if bgs.is_empty() {
        bail!("底色列表不能为空（--backgrounds 以逗号分隔，如 red,blue,white）");
    }
    Ok(bgs)
}

/// 收集输入文件：单图直接使用；目录递归收集常见图片格式
fn collect_images(input: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if input.is_dir() {
        collect_dir(input, &mut files)?;
    } else if input.is_file() {
        files.push(input.to_path_buf());
    } else {
        bail!("输入路径不存在：{}", input.display());
    }
    files.sort();
    Ok(files)
}

fn collect_dir(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("读取目录失败：{}", dir.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            collect_dir(&path, files)?;
        } else if is_image(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_image(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "bmp")
    )
}

/// 单文件处理：落库 → 流水线 → 多产物落盘 → 更新任务状态
fn process_one(
    cfg: &Config,
    args: &ProcessArgs,
    store: &Store,
    out_dir: &Path,
    input: &Path,
    engine: &mut dyn InferenceEngine,
) -> Result<()> {
    let mode = args
        .mode
        .clone()
        .unwrap_or_else(|| cfg.general.default_mode.clone());
    let size = args.size.clone().unwrap_or_else(|| "one_inch".to_string());
    // 尺寸/底色支持自定义形式（`px:295x413` / `mm:35x45@300` / `#RRGGBB`），统一归一化为
    // 文件名安全的 id 供落库与产物命名
    let (size, _) = cfg.resolve_size(&size)?;
    let bgs = parse_backgrounds(&args.backgrounds)?
        .iter()
        .map(|b| cfg.resolve_background(b).map(|(id, _)| id))
        .collect::<std::result::Result<Vec<String>, _>>()?;
    let started = std::time::Instant::now();
    let beauty = if args.beauty {
        "{\"enabled\":true}"
    } else {
        "{}"
    };
    let dress = if args.dress.is_some()
        || args.dress_style.is_some()
        || args.dress_top.is_some()
        || args.dress_bottom.is_some()
        || args.dress_shoes.is_some()
    {
        Some(DressParams {
            enabled: true,
            garment: args.dress.clone(),
            style: args.dress_style.clone(),
            garments: if args.dress_top.is_some()
                || args.dress_bottom.is_some()
                || args.dress_shoes.is_some()
            {
                Some(GarmentSet {
                    top: args.dress_top.clone(),
                    bottom: args.dress_bottom.clone(),
                    shoes: args.dress_shoes.clone(),
                })
            } else {
                None
            },
        })
    } else {
        None
    };
    let dress_json = serde_json::to_string(&dress).unwrap_or_else(|_| "{}".into());
    let task_id = store.insert_task(&NewTask {
        input_path: input.display().to_string(),
        mode: mode.clone(),
        size: size.clone(),
        backgrounds: bgs.join(","),
        beauty: beauty.into(),
        dress: dress_json,
        rotate: args.rotate,
        outputs: String::new(),
        status: "running".into(),
        message: "开始处理".into(),
        warnings: String::new(),
        elapsed_ms: None,
    })?;

    let req = ProcessRequest {
        input: input.to_path_buf(),
        mode: mode.clone(),
        size: size.clone(),
        bgs: bgs.clone(),
        rotate: args.rotate,
        effect: args.effect,
        layout: args.layout.clone(),
        beauty: if args.beauty {
            Some(BeautyParams {
                enabled: true,
                skin_smooth: args.beauty_smooth,
                brighten: args.beauty_brighten,
                whiten: args.beauty_whiten,
            })
        } else {
            None
        },
        dress,
        transparent: args.transparent,
        bg_image: args.bg_image.clone(),
    };
    match run_pipeline(cfg, engine, &req) {
        Ok(r) => {
            // 证件照：task_{id}_{size}_{bg}.jpg
            let mut outputs = Vec::new();
            for photo in &r.photos {
                let out_path = out_dir.join(format!("task_{task_id}_{size}_{}.jpg", photo.bg));
                photo
                    .image
                    .save(&out_path)
                    .with_context(|| format!("保存输出失败：{}", out_path.display()))?;
                outputs.push(out_path.display().to_string());
            }
            // 效果图：task_{id}_effect_{bg}.jpg
            for eff in &r.effects {
                let out_path = out_dir.join(format!("task_{task_id}_effect_{}.jpg", eff.bg));
                eff.image
                    .save(&out_path)
                    .with_context(|| format!("保存输出失败：{}", out_path.display()))?;
                outputs.push(out_path.display().to_string());
            }
            // 排版相纸：task_{id}_layout_{相纸}.jpg
            if let Some(canvas) = &r.layout {
                let layout_id = args.layout.as_deref().unwrap_or("layout");
                let out_path = out_dir.join(format!("task_{task_id}_layout_{layout_id}.jpg"));
                canvas
                    .save(&out_path)
                    .with_context(|| format!("保存输出失败：{}", out_path.display()))?;
                outputs.push(out_path.display().to_string());
            }
            // 透明底证件照：task_{id}_{size}_transparent.png
            if let Some(rgba) = &r.transparent {
                let out_path = out_dir.join(format!("task_{task_id}_{size}_transparent.png"));
                rgba.save(&out_path)
                    .with_context(|| format!("保存输出失败：{}", out_path.display()))?;
                outputs.push(out_path.display().to_string());
            }
            let outputs_json = serde_json::to_string(&outputs)?;
            let warnings = serde_json::to_string(&r.warnings)?;
            store.update_task(
                task_id,
                "succeeded",
                "处理完成",
                &outputs_json,
                &warnings,
                Some(started.elapsed().as_millis() as i64),
            )?;
            println!("已生成 {}：{}", input.display(), outputs.join("、"));
            for w in &r.warnings {
                println!("告警：{w}");
            }
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            store.update_task(
                task_id,
                "failed",
                &msg,
                "[]",
                "[]",
                Some(started.elapsed().as_millis() as i64),
            )?;
            // 模型已就位但当前构建未启用 ONNX 推理时给出迁移指引
            if msg.contains("无可用推理输出") {
                bail!(
                    "{msg}\n当前构建未启用 ONNX 推理。启用真实推理：cargo build -p photos-cli（默认含 ort），或 --features photos-core/ort"
                );
            }
            bail!(msg)
        }
    }
}
