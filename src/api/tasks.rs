use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    db::ImportTask,
    error::{AppError, Result},
    workers,
    AppState,
};

// ─────────────────────────────────────────────
// 请求和响应结构体
// ─────────────────────────────────────────────

/// 创建任务请求体
#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub share_url: String,
    pub pick_code: Option<String>,
    pub category: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: i32,
    pub remark: Option<String>,
}

fn default_priority() -> i32 {
    5
}

/// 任务列表查询参数
#[derive(Debug, Deserialize)]
pub struct ListTasksQuery {
    #[serde(default)]
    pub skip: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    pub status: Option<String>,
}

fn default_limit() -> i64 {
    20
}

/// 任务响应结构
#[derive(Debug, Serialize)]
pub struct TaskResponse {
    pub id: i64,
    pub source_share_url: String,
    pub source_pick_code: Option<String>,
    pub status: String,
    pub total_size: Option<i64>,
    pub total_files: Option<i32>,
    pub current_step: Option<String>,
    pub error_message: Option<String>,
    pub priority: i32,
    pub category: Option<String>,
    pub remark: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

impl From<ImportTask> for TaskResponse {
    fn from(t: ImportTask) -> Self {
        Self {
            id: t.id,
            source_share_url: t.source_share_url,
            source_pick_code: t.source_pick_code,
            status: t.status,
            total_size: t.total_size,
            total_files: t.total_files,
            current_step: t.current_step,
            error_message: t.error_message,
            priority: t.priority,
            category: t.category,
            remark: t.remark,
            created_at: t.created_at,
            updated_at: t.updated_at,
        }
    }
}

/// 分页响应
#[derive(Debug, Serialize)]
pub struct PagedResponse<T: Serialize> {
    pub total: i64,
    pub skip: i64,
    pub limit: i64,
    pub items: Vec<T>,
}

// ─────────────────────────────────────────────
// API 处理器
// ─────────────────────────────────────────────

/// POST /api/tasks/import-share 或 POST /api/tasks
/// 提交一个 115 分享链接导入任务
pub async fn create_task(
    State(state): State<AppState>,
    Json(req): Json<CreateTaskRequest>,
) -> Result<Json<TaskResponse>> {
    // 简单校验
    if req.share_url.trim().is_empty() {
        return Err(AppError::BadRequest("share_url 不能为空".to_string()));
    }

    let task = sqlx::query_as!(
        ImportTask,
        r#"
        INSERT INTO import_tasks
            (source_share_url, source_pick_code, status, priority, category, remark)
        VALUES ($1, $2, 'pending', $3, $4, $5)
        RETURNING
            id, source_share_url, source_pick_code, status,
            total_size, total_files, current_step, error_message,
            priority, category, remark, created_at, updated_at
        "#,
        req.share_url.trim(),
        req.pick_code,
        req.priority,
        req.category,
        req.remark,
    )
    .fetch_one(&state.db)
    .await?;

    tracing::info!("✅ 创建导入任务 #{}: {}", task.id, task.source_share_url);

    // 将任务 ID 推入 Redis 队列，由 Worker 消费
    workers::enqueue_task(&state.redis, task.id).await?;

    Ok(Json(task.into()))
}

/// GET /api/tasks
/// 获取任务列表（分页）
pub async fn list_tasks(
    State(state): State<AppState>,
    Query(q): Query<ListTasksQuery>,
) -> Result<Json<PagedResponse<TaskResponse>>> {
    let limit = q.limit.min(100).max(1);
    let skip = q.skip.max(0);

    // 汇总总数
    let total: i64 = if let Some(ref status) = q.status {
        sqlx::query_scalar!(
            "SELECT COUNT(*) FROM import_tasks WHERE status = $1",
            status
        )
        .fetch_one(&state.db)
        .await?
        .unwrap_or(0)
    } else {
        sqlx::query_scalar!("SELECT COUNT(*) FROM import_tasks")
            .fetch_one(&state.db)
            .await?
            .unwrap_or(0)
    };

    // 查询分页数据
    let tasks: Vec<ImportTask> = if let Some(ref status) = q.status {
        sqlx::query_as!(
            ImportTask,
            r#"
            SELECT id, source_share_url, source_pick_code, status,
                   total_size, total_files, current_step, error_message,
                   priority, category, remark, created_at, updated_at
            FROM import_tasks
            WHERE status = $1
            ORDER BY priority DESC, created_at DESC
            LIMIT $2 OFFSET $3
            "#,
            status,
            limit,
            skip
        )
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as!(
            ImportTask,
            r#"
            SELECT id, source_share_url, source_pick_code, status,
                   total_size, total_files, current_step, error_message,
                   priority, category, remark, created_at, updated_at
            FROM import_tasks
            ORDER BY priority DESC, created_at DESC
            LIMIT $1 OFFSET $2
            "#,
            limit,
            skip
        )
        .fetch_all(&state.db)
        .await?
    };

    Ok(Json(PagedResponse {
        total,
        skip,
        limit,
        items: tasks.into_iter().map(Into::into).collect(),
    }))
}

/// GET /api/tasks/:id
/// 获取任务详情
pub async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<TaskResponse>> {
    let task = sqlx::query_as!(
        ImportTask,
        r#"
        SELECT id, source_share_url, source_pick_code, status,
               total_size, total_files, current_step, error_message,
               priority, category, remark, created_at, updated_at
        FROM import_tasks WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("任务 #{} 不存在", id)))?;

    Ok(Json(task.into()))
}

/// POST /api/tasks/:id/retry
/// 重试失败任务
pub async fn retry_task(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>> {
    let task = sqlx::query_as!(
        ImportTask,
        r#"
        SELECT id, source_share_url, source_pick_code, status,
               total_size, total_files, current_step, error_message,
               priority, category, remark, created_at, updated_at
        FROM import_tasks WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("任务 #{} 不存在", id)))?;

    if task.status != "failed" && task.status != "transfer_failed" {
        return Err(AppError::BadRequest(format!(
            "任务状态为 {}，无法重试",
            task.status
        )));
    }

    sqlx::query!(
        "UPDATE import_tasks SET status = 'pending', error_message = NULL, current_step = NULL WHERE id = $1",
        id
    )
    .execute(&state.db)
    .await?;

    workers::enqueue_task(&state.redis, id).await?;

    tracing::info!("🔄 重试任务 #{}", id);

    Ok(Json(serde_json::json!({
        "status": "retrying",
        "task_id": id,
        "timestamp": Utc::now().to_rfc3339(),
    })))
}

/// POST /api/tasks/:id/cancel
/// 取消待处理任务
pub async fn cancel_task(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>> {
    let result = sqlx::query!(
        "UPDATE import_tasks SET status = 'skipped' WHERE id = $1 AND status = 'pending'",
        id
    )
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::BadRequest(
            "任务不存在或状态不允许取消".to_string(),
        ));
    }

    tracing::info!("🚫 取消任务 #{}", id);

    Ok(Json(serde_json::json!({
        "status": "cancelled",
        "task_id": id,
        "timestamp": Utc::now().to_rfc3339(),
    })))
}
