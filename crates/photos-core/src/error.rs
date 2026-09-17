//! 统一错误类型：全部中文消息，错误链统一使用 thiserror。

use thiserror::Error;

/// 核心库错误（所有消息为中文，禁止含敏感信息）
#[derive(Debug, Error)]
pub enum CoreError {
    /// 配置文件解析失败（TOML 语法错误等）
    #[error("配置解析失败：{0}")]
    ConfigParse(String),

    /// 配置合并 / 反序列化失败
    #[error("配置合并失败：{0}")]
    ConfigMerge(String),

    /// 配置校验失败（模式/模型引用不合法等）
    #[error("配置校验失败：{0}")]
    ConfigValidate(String),

    /// 日志初始化失败
    #[error("日志初始化失败：{0}")]
    Logging(String),

    /// 存储操作失败（sqlite）
    #[error("存储操作失败：{0}")]
    Storage(String),

    /// 模型校验失败（缺失 / 哈希不符等）
    #[error("模型校验失败：{0}")]
    Model(String),

    /// 图像处理失败
    #[error("图像处理失败：{0}")]
    Image(String),

    /// 推理失败（ONNX Runtime）
    #[error("推理失败：{0}")]
    Inference(String),

    /// IO 错误
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
}

/// 核心库结果别名
pub type CoreResult<T> = Result<T, CoreError>;
