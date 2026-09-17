//! `photos process`：单图证件照处理（M1 最小闭环）。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use photos_core::config::Config;
use photos_core::inference::{default_engine, ensure_models_ready};
use photos_core::pipeline::{ProcessRequest, run_pipeline};
use photos_core::storage::NewTask;

use crate::cli::ProcessArgs;
use crate::commands::open_store;

/// 单图处理入口：模型就绪校验 → 流水线 → 落盘 → 落库 task_history
pub fn run(cfg: &Config, args: &ProcessArgs) -> Result<()> {
    let mode = args
        .mode
        .clone()
        .unwrap_or_else(|| cfg.general.default_mode.clone());
    let size = args.size.clone().unwrap_or_else(|| "one_inch".to_string());
    let bg = args.bg.clone().unwrap_or_else(|| "white".to_string());

    // 模型就绪校验（缺失给出中文指引）
    let mut engine = default_engine();
    ensure_models_ready(cfg, engine.as_mut(), &mode).context(
        "模型检查未通过：请按 docs/04-模型清单.md §6 放置模型文件到 models/ 目录，或启用一键下载",
    )?;

    let store = open_store(cfg)?;
    let started = std::time::Instant::now();
    let task_id = store.insert_task(&NewTask {
        input_path: args.input.display().to_string(),
        mode: mode.clone(),
        size: size.clone(),
        backgrounds: bg.clone(),
        beauty: "{}".into(),
        rotate: args.rotate,
        outputs: String::new(),
        status: "running".into(),
        message: "开始处理".into(),
        warnings: String::new(),
        elapsed_ms: None,
    })?;

    let req = ProcessRequest {
        input: args.input.clone(),
        mode,
        size: size.clone(),
        bg: bg.clone(),
        rotate: args.rotate,
    };
    match run_pipeline(cfg, engine.as_mut(), &req) {
        Ok(r) => {
            let out_dir = args
                .out
                .clone()
                .unwrap_or_else(|| PathBuf::from(&cfg.general.data_dir).join("out"));
            std::fs::create_dir_all(&out_dir)?;
            let out_path = out_dir.join(format!("task_{task_id}_{size}_{bg}.jpg"));
            r.image
                .save(&out_path)
                .with_context(|| format!("保存输出失败：{}", out_path.display()))?;
            let outputs = serde_json::to_string(&vec![out_path.display().to_string()])?;
            let warnings = serde_json::to_string(&r.warnings)?;
            store.update_task(
                task_id,
                "succeeded",
                "处理完成",
                &outputs,
                &warnings,
                Some(started.elapsed().as_millis() as i64),
            )?;
            println!("已生成证件照：{}", out_path.display());
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
                    "{msg}\n当前构建未启用 ONNX 推理（M1 链路以 mock 回放打通）。启用真实推理：cargo build --features photos-core/ort 并重新放置模型"
                );
            }
            bail!(msg)
        }
    }
}
