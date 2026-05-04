use axum::{extract::State, Json};
use chrono::Utc;
use serde_json::{json, Value};

use crate::{error::Result, AppState};

/// 编译期注入的构建信息（由 Dockerfile ARG 传入）
const BUILD_SHA:     &str = match option_env!("BUILD_SHA")     { Some(v) => v, None => "dev" };
const BUILD_TIME:    &str = match option_env!("BUILD_TIME")    { Some(v) => v, None => "unknown" };
const BUILD_VERSION: &str = match option_env!("BUILD_VERSION") { Some(v) => v, None => env!("CARGO_PKG_VERSION") };

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
    let resources = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM resources")
        .fetch_one(&state.db)
        .await?;

    let shares_total = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM shares")
        .fetch_one(&state.db)
        .await?;

    let shares_active =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM shares WHERE status = 'active'")
            .fetch_one(&state.db)
            .await?;

    let tasks_total = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM import_tasks")
        .fetch_one(&state.db)
        .await?;

    let tasks_pending =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM import_tasks WHERE status IN ('pending', 'waiting_space')")
            .fetch_one(&state.db)
            .await?;

    let tasks_failed =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM import_tasks WHERE status = 'failed'")
            .fetch_one(&state.db)
            .await?;

    // 获取 115 存储配额（失败时不阻断响应）
    let quota = match state.build_adapter().await {
        Ok(adapter) => match adapter.get_quota().await {
            Ok(q) => json!({
                "total": q.total,
                "used": q.used,
                "free": q.free,
            }),
            Err(_) => json!(null),
        },
        Err(_) => json!(null),
    };

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
        },
        "quota": quota
    })))
}

/// GET /api/logs  —  最近任务活动流水（用于 WebUI 日志页）
pub async fn get_logs(State(state): State<AppState>) -> Result<Json<serde_json::Value>> {
    let rows = sqlx::query_as::<
        _,
        (
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            chrono::DateTime<Utc>,
            chrono::DateTime<Utc>,
        ),
    >(
        r#"
        SELECT id, source_share_url, status, current_step, error_message,
               created_at, updated_at
        FROM import_tasks
        ORDER BY updated_at DESC
        LIMIT 200
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    let entries: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "task_id":   r.0,
                "url":       r.1,
                "status":    r.2,
                "step":      r.3,
                "error":     r.4,
                "created_at": r.5,
                "updated_at": r.6,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "entries": entries, "total": entries.len() })))
}

/// GET /api/logs/level  —  获取当前日志级别
pub async fn get_log_level(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "level": (state.log_level_getter)() }))
}

/// PUT /api/logs/level  —  动态修改日志级别
#[derive(serde::Deserialize)]
pub struct SetLogLevelBody {
    pub level: String,
}

pub async fn set_log_level(
    State(state): State<AppState>,
    Json(body): Json<SetLogLevelBody>,
) -> Result<Json<Value>> {
    (state.log_level_setter)(body.level.clone())
        .map_err(|e| crate::error::AppError::BadRequest(e.to_string()))?;
    tracing::info!("日志级别已动态更新为: {}", body.level);
    Ok(Json(json!({ "level": body.level, "ok": true })))
}
