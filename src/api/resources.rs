use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    db::{Resource, ResourceFile, Share},
    error::{AppError, Result},
    AppState,
};

// ─────────────────────────────────────────────
// 查询参数
// ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub keyword: Option<String>,
    #[serde(rename = "type")]
    pub resource_type: Option<String>,
    pub year: Option<i32>,
    #[serde(default)]
    pub skip: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    20
}

// ─────────────────────────────────────────────
// 响应结构体
// ─────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ResourceResponse {
    pub id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub resource_type: String,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<Resource> for ResourceResponse {
    fn from(r: Resource) -> Self {
        Self {
            id: r.id,
            title: r.title,
            original_title: r.original_title,
            year: r.year,
            resource_type: r.resource_type,
            tmdb_id: r.tmdb_id,
            imdb_id: r.imdb_id,
            overview: r.overview,
            poster_url: r.poster_url,
            status: r.status,
            created_at: r.created_at,
        }
    }
}

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

/// GET /api/resources?keyword=xxx&type=movie&year=2024
pub async fn list_resources(
    State(state): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<PagedResponse<ResourceResponse>>> {
    let limit = q.limit.min(100).max(1);
    let skip = q.skip.max(0);

    // 动态查询：支持关键词、类型、年份过滤
    let keyword = q.keyword.as_deref().map(|s| format!("%{}%", s));

    let total: i64 = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) FROM resources
        WHERE ($1::text IS NULL OR title ILIKE $1 OR original_title ILIKE $1)
          AND ($2::text IS NULL OR resource_type = $2)
          AND ($3::int IS NULL OR year = $3)
        "#,
        keyword,
        q.resource_type,
        q.year,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    let rows = sqlx::query_as!(
        Resource,
        r#"
        SELECT id, title, original_title, year, resource_type,
               tmdb_id, imdb_id, overview, poster_url, backdrop_url,
               status, created_at, updated_at
        FROM resources
        WHERE ($1::text IS NULL OR title ILIKE $1 OR original_title ILIKE $1)
          AND ($2::text IS NULL OR resource_type = $2)
          AND ($3::int IS NULL OR year = $3)
        ORDER BY created_at DESC
        LIMIT $4 OFFSET $5
        "#,
        keyword,
        q.resource_type,
        q.year,
        limit,
        skip,
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(PagedResponse {
        total,
        skip,
        limit,
        items: rows.into_iter().map(Into::into).collect(),
    }))
}

/// GET /api/resources/:id
pub async fn get_resource(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ResourceResponse>> {
    let resource = sqlx::query_as!(
        Resource,
        r#"
        SELECT id, title, original_title, year, resource_type,
               tmdb_id, imdb_id, overview, poster_url, backdrop_url,
               status, created_at, updated_at
        FROM resources WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("资源 #{} 不存在", id)))?;

    Ok(Json(resource.into()))
}

/// GET /api/resources/:id/shares
pub async fn get_resource_shares(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<Share>>> {
    // 确认资源存在
    let exists = sqlx::query_scalar!("SELECT 1 FROM resources WHERE id = $1", id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound(format!("资源 #{} 不存在", id)));
    }

    let shares = sqlx::query_as!(
        Share,
        r#"
        SELECT id, resource_id, share_url, pick_code, share_code,
               share_title, share_type, file_count, total_size,
               status, last_checked_at, created_at
        FROM shares WHERE resource_id = $1 AND status = 'active'
        ORDER BY created_at DESC
        "#,
        id
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(shares))
}

/// GET /api/resources/:id/files
pub async fn get_resource_files(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<ResourceFile>>> {
    let files = sqlx::query_as!(
        ResourceFile,
        r#"
        SELECT id, resource_id, file_name, file_path, file_size,
               file_ext, media_type, season, episode, quality,
               source, codec, audio, subtitle_info,
               cloud_file_id, pick_code, strm_path, created_at
        FROM resource_files WHERE resource_id = $1
        ORDER BY season NULLS LAST, episode NULLS LAST, file_name
        "#,
        id
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(files))
}
