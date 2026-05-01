use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde_json::json;
use thiserror::Error;

/// 应用统一错误类型
#[derive(Debug, Error)]
pub enum AppError {
    #[error("未找到: {0}")]
    NotFound(String),

    #[error("请求参数无效: {0}")]
    BadRequest(String),

    #[error("未授权")]
    Unauthorized,

    #[error("权限不足")]
    Forbidden,

    #[error("请求频率超限")]
    RateLimited,

    #[error("数据库错误: {0}")]
    Database(#[from] sqlx::Error),

    #[error("HTTP 请求错误: {0}")]
    Http(#[from] reqwest::Error),

    #[error("115 API 错误: {0}")]
    Api115(String),

    #[error("TMDB API 错误: {0}")]
    TmdbApi(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("Redis 错误: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("序列化错误: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("内部错误: {0}")]
    Internal(#[from] anyhow::Error),
}

/// 将 AppError 转为 HTTP 响应
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "未授权".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "权限不足".to_string()),
            AppError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "请求频率超限，请稍后重试".to_string(),
            ),
            AppError::Database(e) => {
                tracing::error!("数据库错误: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "数据库操作失败".to_string(),
                )
            }
            AppError::Http(e) => {
                tracing::error!("HTTP 请求错误: {:?}", e);
                (
                    StatusCode::BAD_GATEWAY,
                    "外部服务请求失败".to_string(),
                )
            }
            AppError::Api115(msg) => (StatusCode::BAD_GATEWAY, format!("115 API: {}", msg)),
            AppError::TmdbApi(msg) => (StatusCode::BAD_GATEWAY, format!("TMDB API: {}", msg)),
            AppError::Config(msg) => {
                tracing::error!("配置错误: {}", msg);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "服务器配置错误".to_string(),
                )
            }
            AppError::Redis(e) => {
                tracing::error!("Redis 错误: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "缓存服务错误".to_string(),
                )
            }
            AppError::Serialize(e) => {
                tracing::error!("序列化错误: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "数据处理失败".to_string(),
                )
            }
            AppError::Internal(e) => {
                tracing::error!("内部错误: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "服务器内部错误".to_string(),
                )
            }
        };

        let body = Json(json!({
            "status_code": status.as_u16(),
            "message": message,
            "timestamp": Utc::now().to_rfc3339(),
        }));

        (status, body).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
