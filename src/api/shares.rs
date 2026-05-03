use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    db::Share,
    error::{AppError, Result},
    services::{check_share_rate, record_share_created},
    AppState,
};

// ─────────────────────────────────────────────
// 查询参数
// ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListSharesQuery {
    pub status: Option<String>,
    #[serde(default)]
    pub skip: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    20
}

// ─────────────────────────────────────────────
// GET /api/shares  →  分享列表（支持状态过滤）
// ─────────────────────────────────────────────

pub async fn list_shares(
    State(state): State<AppState>,
    Query(params): Query<ListSharesQuery>,
) -> Result<Json<serde_json::Value>> {
    let limit = params.limit.min(100).max(1);
    let skip = params.skip.max(0);

    let total: i64 = if let Some(ref status) = params.status {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM shares WHERE status = $1")
            .bind(status)
            .fetch_one(&state.db)
            .await?
    } else {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM shares")
            .fetch_one(&state.db)
            .await?
    };

    let rows: Vec<Share> = if let Some(ref status) = params.status {
        sqlx::query_as::<_, Share>(
            r#"
            SELECT id, resource_id, share_url, pick_code, share_code,
                   share_title, share_type, file_count, total_size,
                   status, last_checked_at, created_at
            FROM shares
            WHERE status = $1
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(status)
        .bind(limit)
        .bind(skip)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, Share>(
            r#"
            SELECT id, resource_id, share_url, pick_code, share_code,
                   share_title, share_type, file_count, total_size,
                   status, last_checked_at, created_at
            FROM shares
            ORDER BY created_at DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(skip)
        .fetch_all(&state.db)
        .await?
    };

    Ok(Json(serde_json::json!({
        "total": total,
        "skip": skip,
        "limit": limit,
        "items": rows
    })))
}

// ─────────────────────────────────────────────
// GET /api/shares/:id
// ─────────────────────────────────────────────

pub async fn get_share(State(state): State<AppState>, Path(id): Path<i64>) -> Result<Json<Share>> {
    let share = sqlx::query_as::<_, Share>(
        r#"
        SELECT id, resource_id, share_url, pick_code, share_code,
               share_title, share_type, file_count, total_size,
               status, last_checked_at, created_at
        FROM shares WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("分享 #{} 不存在", id)))?;

    Ok(Json(share))
}

// ─────────────────────────────────────────────
// POST /api/shares/:id/check  →  验证分享是否仍有效
// ─────────────────────────────────────────────

pub async fn check_share(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>> {
    let share = sqlx::query_as::<_, Share>(
        r#"
        SELECT id, resource_id, share_url, pick_code, share_code,
               share_title, share_type, file_count, total_size,
               status, last_checked_at, created_at
        FROM shares WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("分享 #{} 不存在", id)))?;

    let share_code = if let Some(ref code) = share.share_code {
        code.clone()
    } else {
        extract_share_id_from_url(&share.share_url)?
    };

    // 使用运行时 Cookie（支持 WebUI 热更新）
    let adapter = state.build_adapter().await.map_err(AppError::Internal)?;

    let alive = adapter
        .verify_share(&share_code, share.pick_code.as_deref())
        .await
        .unwrap_or(false);

    let new_status = if alive { "active" } else { "inactive" };
    let now = Utc::now();

    sqlx::query("UPDATE shares SET status = $1, last_checked_at = $2 WHERE id = $3")
        .bind(new_status)
        .bind(now)
        .bind(id)
        .execute(&state.db)
        .await?;

    tracing::info!("🔍 分享 #{} 检查：{} → {}", id, share.status, new_status);

    Ok(Json(serde_json::json!({
        "share_id": id,
        "share_url": share.share_url,
        "previous_status": share.status,
        "status": new_status,
        "alive": alive,
        "last_checked_at": now.to_rfc3339(),
    })))
}

// ─────────────────────────────────────────────
// POST /api/shares/:id/rebuild  →  重建失效分享
// ─────────────────────────────────────────────

pub async fn rebuild_share(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>> {
    let share = sqlx::query_as::<_, Share>(
        r#"
        SELECT id, resource_id, share_url, pick_code, share_code,
               share_title, share_type, file_count, total_size,
               status, last_checked_at, created_at
        FROM shares WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("分享 #{} 不存在", id)))?;

    if share.status == "active" {
        return Err(AppError::BadRequest("分享链接仍然有效，无需重建".to_string()));
    }

    let resource_id = share.resource_id.ok_or_else(|| {
        AppError::BadRequest("分享未关联资源，无法自动重建（请重新提交导入任务）".to_string())
    })?;

    let cloud_ids: Vec<String> = sqlx::query_scalar::<_, String>(
        "SELECT cloud_file_id FROM resource_files WHERE resource_id = $1 AND cloud_file_id IS NOT NULL",
    )
    .bind(resource_id)
    .fetch_all(&state.db)
    .await?;

    if cloud_ids.is_empty() {
        return Err(AppError::BadRequest(
            "关联资源没有云盘文件记录，无法重建分享".to_string(),
        ));
    }

    let runtime_config = state.runtime_config().await;
    let allowed = check_share_rate(
        &state.redis,
        runtime_config.share_max_create_per_minute,
        runtime_config.share_max_create_per_hour,
        runtime_config.share_max_create_per_day,
    )
    .await
    .unwrap_or(true);

    if !allowed {
        return Err(AppError::RateLimited);
    }

    let jitter =
        rand::random::<u64>() % (runtime_config.share_random_jitter_secs * 1000 + 1);
    tokio::time::sleep(tokio::time::Duration::from_millis(
        runtime_config.share_min_interval_secs * 1000 + jitter,
    ))
    .await;

    let adapter = state.build_adapter().await.map_err(AppError::Internal)?;

    let id_refs: Vec<&str> = cloud_ids.iter().map(|s| s.as_str()).collect();
    let new_share = adapter
        .create_share(&id_refs, share.share_title.as_deref(), 7)
        .await
        .map_err(|e| AppError::Api115(e.to_string()))?;

    let _ = record_share_created(&state.redis).await;

    let now = Utc::now();

    sqlx::query(
        r#"
        UPDATE shares
        SET share_url = $1, pick_code = $2, share_code = $3,
            status = 'active', last_checked_at = $4
        WHERE id = $5
        "#,
    )
    .bind(&new_share.share_url)
    .bind(&new_share.pick_code)
    .bind(&new_share.share_id)
    .bind(now)
    .bind(id)
    .execute(&state.db)
    .await?;

    tracing::info!("🔨 分享 #{} 重建完成 → {}", id, new_share.share_url);

    Ok(Json(serde_json::json!({
        "share_id": id,
        "new_share_url": new_share.share_url,
        "pick_code": new_share.pick_code,
        "status": "active",
        "rebuilt_at": now.to_rfc3339(),
    })))
}

// ─────────────────────────────────────────────
// 工具函数
// ─────────────────────────────────────────────

fn extract_share_id_from_url(url: &str) -> Result<String> {
    let (_, tail) = url
        .split_once("/s/")
        .ok_or_else(|| AppError::BadRequest(format!("无法从 URL 提取分享 ID：{}", url)))?;

    tail.split(['?', '#'])
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| AppError::BadRequest(format!("无法从 URL 提取分享 ID：{}", url)))
}
