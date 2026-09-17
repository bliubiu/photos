//! 子命令实现。

pub mod gui;
pub mod models;
pub mod process;
pub mod serve;

use photos_core::config::Config;
use photos_core::error::CoreResult;
use photos_core::storage::Store;
use std::path::Path;

/// 打开数据存储（data_dir/photos.db）
pub fn open_store(cfg: &Config) -> CoreResult<Store> {
    let db_path = Path::new(&cfg.general.data_dir).join("photos.db");
    Store::open(&db_path)
}
