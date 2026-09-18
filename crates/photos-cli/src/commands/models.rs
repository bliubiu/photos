//! `photos models`：输出模型注册表状态报告 / 一键下载模型。

use anyhow::Result;
use photos_core::config::Config;
use photos_core::model::{CheckStatus, ModelStatus, check_models, download_model, ready_count};

use crate::cli::{ModelsArgs, ModelsCommand};

use super::open_store;

/// 模型状态报告（表格或 JSON）；存在子命令时优先执行子命令
pub fn run(cfg: &Config, args: &ModelsArgs) -> Result<()> {
    if let Some(cmd) = &args.command {
        match cmd {
            ModelsCommand::Download { target } => download(cfg, target)?,
        }
        return Ok(());
    }
    report(cfg, args.json)
}

/// 一键下载：`all` 下载全部已启用模型，否则下载指定 id
fn download(cfg: &Config, target: &str) -> Result<()> {
    let ids: Vec<String> = if target == "all" {
        cfg.models
            .iter()
            .filter(|(_, s)| s.enabled)
            .map(|(id, _)| id.clone())
            .collect()
    } else {
        vec![target.to_string()]
    };
    if ids.is_empty() {
        anyhow::bail!("未找到可下载的模型（全部未启用？）");
    }
    for id in &ids {
        let spec = cfg.model_spec(id)?;
        let has_url = spec
            .download
            .as_ref()
            .and_then(|d| d.url.as_deref())
            .filter(|u| !u.is_empty())
            .is_some();
        if !has_url {
            println!("模型“{id}”未配置下载地址（[models.{id}].download.url），跳过。");
            continue;
        }
        println!("开始下载模型“{id}”...");
        download_model(cfg, id)?;
        println!("模型“{id}”下载完成。");
    }
    println!("全部下载完成。");
    Ok(())
}

/// 模型状态报告
fn report(cfg: &Config, json: bool) -> Result<()> {
    let store = open_store(cfg)?;
    let statuses = check_models(cfg, &store)?;

    if json {
        let rows: Vec<_> = statuses
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "path": s.path.display().to_string(),
                    "check_status": s.check_status.code(),
                    "message": s.message,
                })
            })
            .collect();
        let summary = serde_json::json!({
            "ready": ready_count(&statuses),
            "total": statuses.len(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "summary": summary, "models": rows })
            )?
        );
        return Ok(());
    }

    println!(
        "模型状态报告（就绪 {}/{}）",
        ready_count(&statuses),
        statuses.len()
    );
    println!("{:<16} {:<8} 说明", "模型", "状态");
    for s in &statuses {
        println!("{:<16} {:<8} {}", s.id, s.check_status.as_str(), s.message);
    }
    print_hint(&statuses);
    Ok(())
}

/// 缺失/失败时的操作指引
fn print_hint(statuses: &[ModelStatus]) {
    let need = statuses
        .iter()
        .filter(|s| !matches!(s.check_status, CheckStatus::Ready | CheckStatus::CachedOk))
        .count();
    if need > 0 {
        println!();
        println!(
            "提示：有 {need} 个模型未就绪。请按 docs/04-模型清单.md 放置模型文件到 models/ 目录，或启用 [models.download] 一键下载。"
        );
    }
}
