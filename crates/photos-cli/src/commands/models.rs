//! `photos models`：输出模型注册表状态报告。

use anyhow::Result;
use photos_core::config::Config;
use photos_core::model::{CheckStatus, ModelStatus, check_models, ready_count};

use super::open_store;

/// 模型状态报告（表格或 JSON）
pub fn run(cfg: &Config, json: bool) -> Result<()> {
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
    println!("{:<16} {:<8} {}", "模型", "状态", "说明");
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
