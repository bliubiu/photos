//! photos-core 核心库：领域模型、流水线编排、图像与推理实现。
//!
//! 依赖方向：photos-cli / photos-api → photos-core；本 crate 不依赖任何 HTTP / Tauri。
//! 内部按 DDD 四层组织：领域（domain）、应用（pipeline）、基础设施（config/logging/storage/model/inference）、接口（错误类型）。

pub mod config;
pub mod error;
pub mod inference;
pub mod logging;
pub mod model;
pub mod pipeline;
pub mod storage;
pub mod vision;
