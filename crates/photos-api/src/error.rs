//! API 统一错误体：`{ "code": "...", "message": "中文" }`（契约 §1）。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// API 错误（message 必为中文且脱敏）
#[derive(Debug)]
pub enum ApiError {
    /// 参数非法（400）
    InvalidParams(String),
    /// 自定义模型注册声明非法（400）
    ModelRegisterInvalid(String),
    /// 模型版本不存在（400）
    ModelVersionUnknown(String),
    /// 模型缺失 / 未就绪（503）
    ModelMissing(String),
    /// 文件过大（413）
    FileTooLarge(String),
    /// 媒体类型不支持（415）
    UnsupportedMedia(String),
    /// 任务不存在（404）
    TaskNotFound,
    /// 产物不存在（404）
    ArtifactNotFound(String),
    /// 内部错误（500）
    Internal(String),
}

/// 错误响应体
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    /// 错误码（契约 §4 枚举）
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::InvalidParams(_) => "INVALID_PARAMS",
            ApiError::ModelRegisterInvalid(_) => "MODEL_REGISTER_INVALID",
            ApiError::ModelVersionUnknown(_) => "MODEL_VERSION_UNKNOWN",
            ApiError::ModelMissing(_) => "MODEL_MISSING",
            ApiError::FileTooLarge(_) => "FILE_TOO_LARGE",
            ApiError::UnsupportedMedia(_) => "UNSUPPORTED_MEDIA",
            ApiError::TaskNotFound => "TASK_NOT_FOUND",
            ApiError::ArtifactNotFound(_) => "ARTIFACT_NOT_FOUND",
            ApiError::Internal(_) => "INTERNAL",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            ApiError::InvalidParams(_) => StatusCode::BAD_REQUEST,
            ApiError::ModelRegisterInvalid(_) | ApiError::ModelVersionUnknown(_) => {
                StatusCode::BAD_REQUEST
            }
            ApiError::ModelMissing(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::FileTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::UnsupportedMedia(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ApiError::TaskNotFound | ApiError::ArtifactNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> String {
        match self {
            ApiError::InvalidParams(m) => m.clone(),
            ApiError::ModelRegisterInvalid(m) => m.clone(),
            ApiError::ModelVersionUnknown(m) => m.clone(),
            ApiError::ModelMissing(m) => m.clone(),
            ApiError::FileTooLarge(m) => m.clone(),
            ApiError::UnsupportedMedia(m) => m.clone(),
            ApiError::TaskNotFound => "任务不存在".into(),
            ApiError::ArtifactNotFound(m) => m.clone(),
            ApiError::Internal(m) => m.clone(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            code: self.code(),
            message: self.message(),
        };
        (self.status(), Json(body)).into_response()
    }
}

impl From<photos_core::error::CoreError> for ApiError {
    fn from(e: photos_core::error::CoreError) -> Self {
        // 模型缺失/校验类错误映射为 MODEL_MISSING；其余按内部错误（中文消息透传）
        match &e {
            photos_core::error::CoreError::Model(_)
            | photos_core::error::CoreError::Download(_) => ApiError::ModelMissing(e.to_string()),
            _ => ApiError::Internal(e.to_string()),
        }
    }
}

/// 便捷构造：模型未就绪（校验失败/缺失）
pub fn model_missing(id: &str, reason: &str) -> ApiError {
    ApiError::ModelMissing(format!("模型“{id}”未就绪：{reason}"))
}
