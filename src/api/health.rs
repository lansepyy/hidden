use axum::{extract::State, Json};
use chrono::Utc;
use serde_json::{json, Value};

use crate::{error::Result, AppState};

/// 编译期注入的构建信息（由 Dockerfile ARG 传入）
const BUILD_SHA:     &str = option_env!("BUILD_SHA")    .unwrap_or("dev");
const BUILD_TIME:    &str = option_env!("BUILD_TIME")   .unwrap_or("unknown");
const BUILD_VERSION: &str = option_env!("BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));

/// GET /api/health
pub async fn health_check() -> Json<Value> {
    Json(json!({
        "status":    "healthy",
        "app":       "洞天福地 (Hidden)",
        "version":   BUILD_VERSION,
        "commit":    BUILD_SHA,
        "build_time": BUILD_TIME,
        "timestamp": Utc::now().to_rfc3339(),
    }))
}

/// GET /api/stats  —  仪表盘统计数据
pub async fn get_stats(State(state): State<AppState>) -> Result<Json<Value>> {
    let resources = sqlx::query_scalar!("SELECT COUNT(*) FROM resources")
        .fetch_one(&state.db)
        .await?
        .unwrap_or(0);

    let shares_total = sqlx::query_scalar!("SELECT COUNT(*) FROM shares")
        .fetch_one(&state.db)
        .await?
        .unwrap_or(0);

    let shares_active = sqlx::query_scalar!("SELECT COUNT(*) FROM shares WHERE status = 'active'")
        .fetch_one(&state.db)
        .await?
        .unwrap_or(0);

    let tasks_total = sqlx::query_scalar!("SELECT COUNT(*) FROM import_tasks")
        .fetch_one(&state.db)
        .await?
        .unwrap_or(0);

    let tasks_pending = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM import_tasks WHERE status = 'pending'"
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    let tasks_failed = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM import_tasks WHERE status = 'failed'"
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    Ok(Json(json!({
        "resources": resources,
        "shares": {
            "total": shares_total,
            "active": shares_active,
            "inactive": shares_total - shares_active
        },
        "tasks": {
            "total": tasks_total,
            "pending": tasks_pending,
            "failed": tasks_failed
        }
    })))
}
