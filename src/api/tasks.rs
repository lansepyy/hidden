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

#[derive(Debug, Deserialize)]
pub struct DeleteTaskQuery {
    pub force: Option<bool>,
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
    let raw_url = req.share_url.trim().to_string();
    if raw_url.is_empty() {
        return Err(AppError::BadRequest("share_url 不能为空".to_string()));
    }
    // 宽松校验：必须包含 "/s/" 且来自 115 相关域名，避免误拒绝常见子域/镜像
    let valid_host = raw_url.contains("/s/") && (raw_url.contains("115.com") || raw_url.contains("115cdn.com"));
    if !valid_host {
        return Err(AppError::BadRequest(
            "分享链接格式无效，仅支持 115 的分享链接".to_string(),
        ));
    }

    // 自动从 URL 的查询参数提取提取码（常见参数名：password / pwd / pick_code / code）
    let pick_code = req.pick_code.clone().or_else(|| {
        raw_url.split_once('?').and_then(|(_, qs)| {
            qs.split('&').find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                match k {
                    "password" | "pwd" | "pick_code" | "code" => {
                        if !v.is_empty() { Some(v.to_string()) } else { None }
                    }
                    _ => None,
                }
            })
        })
    });

    // 存入数据库时移除常见提取码参数（避免泄露）
    let clean_url = raw_url
        .split_once('?')
        .map(|(base, qs)| {
            let filtered: Vec<&str> = qs
                .split('&')
                .filter(|p| {
                    !(p.starts_with("password=") || p.starts_with("pwd=") || p.starts_with("pick_code=") || p.starts_with("code="))
                })
                .collect();
            if filtered.is_empty() {
                base.to_string()
            } else {
                format!("{}?{}", base, filtered.join("&"))
            }
        })
        .unwrap_or(raw_url);

    // 数据库中已存在同一个源分享链接时直接跳过：返回已有任务，不重复入队/转存/创建分享。
    if let Some(existing_task) = sqlx::query_as::<_, ImportTask>(
        r#"
        SELECT id, source_share_url, source_pick_code, status,
               total_size, total_files, current_step, error_message,
               priority, category, remark, created_at, updated_at
        FROM import_tasks
        WHERE source_share_url = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(&clean_url)
    .fetch_optional(&state.db)
    .await?
    {
        tracing::info!(
            "⏭️  分享链接已存在，跳过重复提交：任务 #{} {}",
            existing_task.id,
            existing_task.source_share_url
        );
        return Ok(Json(existing_task.into()));
    }

    let task = sqlx::query_as::<_, ImportTask>(
        r#"
        INSERT INTO import_tasks
            (source_share_url, source_pick_code, status, priority, category, remark)
        VALUES ($1, $2, 'pending', $3, $4, $5)
        RETURNING
            id, source_share_url, source_pick_code, status,
            total_size, total_files, current_step, error_message,
            priority, category, remark, created_at, updated_at
        "#,
    )
    .bind(clean_url)
    .bind(pick_code)
    .bind(req.priority)
    .bind(req.category)
    .bind(req.remark)
    .fetch_one(&state.db)
    .await?;

    // 预检查：尝试构建 115 适配器并验证会话有效性，防止提交后 Worker 立刻失败
    let adapter = state.build_adapter().await.map_err(|e| AppError::Internal(e))?;
    match adapter.check_session().await {
        Ok(true) => {}
        Ok(false) => return Err(AppError::BadRequest("115 会话无效或 Cookie 配置错误，请在系统设置中检查并重新保存 Cookie".to_string())),
        Err(e) => return Err(AppError::Internal(e)),
    }

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
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM import_tasks WHERE status = $1")
            .bind(status)
            .fetch_one(&state.db)
            .await?
    } else {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM import_tasks")
            .fetch_one(&state.db)
            .await?
    };

    // 查询分页数据
    let tasks: Vec<ImportTask> = if let Some(ref status) = q.status {
        sqlx::query_as::<_, ImportTask>(
            r#"
            SELECT id, source_share_url, source_pick_code, status,
                   total_size, total_files, current_step, error_message,
                   priority, category, remark, created_at, updated_at
            FROM import_tasks
            WHERE status = $1
            ORDER BY priority DESC, created_at DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(status)
        .bind(limit)
        .bind(skip)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, ImportTask>(
            r#"
            SELECT id, source_share_url, source_pick_code, status,
                   total_size, total_files, current_step, error_message,
                   priority, category, remark, created_at, updated_at
            FROM import_tasks
            ORDER BY priority DESC, created_at DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(skip)
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
    let task = sqlx::query_as::<_, ImportTask>(
        r#"
        SELECT id, source_share_url, source_pick_code, status,
               total_size, total_files, current_step, error_message,
               priority, category, remark, created_at, updated_at
        FROM import_tasks WHERE id = $1
        "#,
    )
    .bind(id)
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
    let task = sqlx::query_as::<_, ImportTask>(
        r#"
        SELECT id, source_share_url, source_pick_code, status,
               total_size, total_files, current_step, error_message,
               priority, category, remark, created_at, updated_at
        FROM import_tasks WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("任务 #{} 不存在", id)))?;

    if !matches!(task.status.as_str(), "failed" | "transfer_failed" | "skipped") {
        return Err(AppError::BadRequest(format!(
            "任务状态为 {}，无法重试",
            task.status
        )));
    }

    sqlx::query(
        "UPDATE import_tasks SET status = 'pending', error_message = NULL, current_step = NULL WHERE id = $1",
    )
    .bind(id)
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
    let result = sqlx::query(
        "UPDATE import_tasks SET status = 'skipped' WHERE id = $1 AND status IN ('pending', 'waiting_space')",
    )
    .bind(id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::BadRequest(
            "任务不存在或状态不允许取消（只可取消待处理或空间等待中的任务）".to_string(),
        ));
    }

    tracing::info!("🚫 取消任务 #{}", id);

    Ok(Json(serde_json::json!({
        "status": "cancelled",
        "task_id": id,
        "timestamp": Utc::now().to_rfc3339(),
    })))
}

/// DELETE /api/tasks/:id
/// 删除任务记录（仅允许删除已完成/失败/跳过状态的任务）
pub async fn delete_task(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<DeleteTaskQuery>,
) -> Result<Json<serde_json::Value>> {
    let task = sqlx::query_as::<_, ImportTask>(
        r#"
        SELECT id, source_share_url, source_pick_code, status,
               total_size, total_files, current_step, error_message,
               priority, category, remark, created_at, updated_at
        FROM import_tasks WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("任务 #{} 不存在", id)))?;

    // 运行中的任务默认不允许删除（waiting_space 是卡住的空间等待，允许删除）
    let force = q.force.unwrap_or(false);
    if !force && matches!(task.status.as_str(), "parsing" | "transferring" | "organizing" | "sharing") {
        return Err(AppError::BadRequest(format!(
            "任务正在运行中（{}），请先取消再删除；如确需删除可使用 ?force=true",
            task.status
        )));
    }

    if force {
        tracing::warn!("⚠️  强制删除任务 #{}（status={}）", id, task.status);
    }

    sqlx::query("DELETE FROM import_tasks WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    tracing::info!("🗑️ 删除任务 #{}", id);

    Ok(Json(serde_json::json!({ "deleted": true, "task_id": id })))
}
