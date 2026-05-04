use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    db::{Resource, ResourceFile, Share},
    error::{AppError, Result},
    services::TmdbClient,
    AppState,
};

const TMDB_IMAGE_BASE: &str = "https://image.tmdb.org/t/p/w500";

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

#[derive(Debug, Deserialize)]
pub struct PopularQuery {
    #[serde(rename = "type")]
    pub resource_type: Option<String>,
    #[serde(default = "default_popular_limit")]
    pub limit: usize,
}

fn default_limit() -> i64 {
    20
}

fn default_popular_limit() -> usize {
    12
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
    pub backdrop_url: Option<String>,
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
            poster_url: normalize_tmdb_image_url(r.poster_url),
            backdrop_url: normalize_tmdb_image_url(r.backdrop_url),
            status: r.status,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PopularResourceItem {
    pub title: String,
    pub original_title: String,
    pub year: Option<i32>,
    pub resource_type: String,
    pub tmdb_id: i64,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
}

impl PopularResourceItem {
    fn from_movie(m: crate::services::tmdb::TmdbMovie) -> Self {
        Self {
            title: m.title,
            original_title: m.original_title,
            year: parse_year(m.release_date.as_deref()),
            resource_type: "movie".to_string(),
            tmdb_id: m.id,
            overview: m.overview,
            poster_url: normalize_tmdb_image_url(m.poster_path),
            backdrop_url: normalize_tmdb_image_url(m.backdrop_path),
        }
    }

    fn from_tv(t: crate::services::tmdb::TmdbTv) -> Self {
        Self {
            title: t.name,
            original_title: t.original_name,
            year: parse_year(t.first_air_date.as_deref()),
            resource_type: "tv".to_string(),
            tmdb_id: t.id,
            overview: t.overview,
            poster_url: normalize_tmdb_image_url(t.poster_path),
            backdrop_url: normalize_tmdb_image_url(t.backdrop_path),
        }
    }
}

fn parse_year(date: Option<&str>) -> Option<i32> {
    date.and_then(|d| d.split('-').next())
        .and_then(|y| y.parse().ok())
}

fn normalize_tmdb_image_url(url: Option<String>) -> Option<String> {
    let url = url?.trim().to_string();
    if url.is_empty() {
        return None;
    }

    if url.starts_with("http://") || url.starts_with("https://") {
        Some(url)
    } else if url.starts_with('/') {
        Some(format!("{}{}", TMDB_IMAGE_BASE, url))
    } else {
        Some(url)
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
///
/// 只展示 active 资源，避免已经被逻辑删除/清理的资源仍出现在资源库。
pub async fn list_resources(
    State(state): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<PagedResponse<ResourceResponse>>> {
    let limit = q.limit.min(100).max(1);
    let skip = q.skip.max(0);

    // 动态查询：支持关键词、类型、年份过滤
    let keyword = q
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", s));

    let resource_type = q
        .resource_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let total = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*) FROM resources r
        WHERE r.status = 'active'
          AND ($1::text IS NULL OR r.title ILIKE $1 OR r.original_title ILIKE $1)
          AND ($2::text IS NULL OR r.resource_type = $2)
          AND ($3::int IS NULL OR r.year = $3)
        "#,
    )
    .bind(keyword.as_deref())
    .bind(resource_type)
    .bind(q.year)
    .fetch_one(&state.db)
    .await?;

    let rows = sqlx::query_as::<_, Resource>(
        r#"
        SELECT r.id, r.title, r.original_title, r.year, r.resource_type,
               r.tmdb_id, r.imdb_id, r.overview, r.poster_url, r.backdrop_url,
               r.status, r.created_at, r.updated_at
        FROM resources r
        WHERE r.status = 'active'
          AND ($1::text IS NULL OR r.title ILIKE $1 OR r.original_title ILIKE $1)
          AND ($2::text IS NULL OR r.resource_type = $2)
          AND ($3::int IS NULL OR r.year = $3)
        ORDER BY r.created_at DESC
        LIMIT $4 OFFSET $5
        "#,
    )
    .bind(keyword.as_deref())
    .bind(resource_type)
    .bind(q.year)
    .bind(limit)
    .bind(skip)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(PagedResponse {
        total,
        skip,
        limit,
        items: rows.into_iter().map(Into::into).collect(),
    }))
}

/// GET /api/resources/popular?type=movie|tv&limit=12
///
/// 资源库首页的 TMDB 热门入口：点击热门条目后由前端带标题搜索本地数据库。
pub async fn popular_resources(
    State(state): State<AppState>,
    Query(q): Query<PopularQuery>,
) -> Result<Json<Vec<PopularResourceItem>>> {
    let limit = q.limit.clamp(1, 40);
    let runtime_config = state.runtime_config().await;
    let client = TmdbClient::new(&runtime_config).map_err(|e| AppError::TmdbApi(e.to_string()))?;

    let items = match q.resource_type.as_deref().unwrap_or("movie") {
        "tv" => client
            .popular_tv(limit)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .map(PopularResourceItem::from_tv)
            .collect(),
        "movie" | "" => client
            .popular_movies(limit)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .map(PopularResourceItem::from_movie)
            .collect(),
        other => {
            return Err(AppError::BadRequest(format!(
                "不支持的热门类型：{}，仅支持 movie/tv",
                other
            )))
        }
    };

    Ok(Json(items))
}

/// GET /api/resources/:id
pub async fn get_resource(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ResourceResponse>> {
    let resource = sqlx::query_as::<_, Resource>(
        r#"
        SELECT id, title, original_title, year, resource_type,
               tmdb_id, imdb_id, overview, poster_url, backdrop_url,
               status, created_at, updated_at
        FROM resources WHERE id = $1 AND status = 'active'
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("资源 #{} 不存在", id)))?;

    Ok(Json(resource.into()))
}

/// DELETE /api/resources/:id
///
/// 删除数据库资源，并联动取消/删除关联分享链接。
/// 注意：这里不删除 115 成品目录中的媒体文件，避免误删用户网盘内容；
/// 只取消分享链接并清理本地数据库记录。
pub async fn delete_resource(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>> {
    let exists = sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM resources WHERE id = $1 AND status = 'active'",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    if exists.is_none() {
        return Err(AppError::NotFound(format!("资源 #{} 不存在", id)));
    }

    let shares = sqlx::query_as::<_, Share>(
        r#"
        SELECT id, resource_id, share_url, pick_code, share_code,
               share_title, share_type, file_count, total_size,
               status, last_checked_at, created_at
        FROM shares WHERE resource_id = $1
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let adapter = match state.build_adapter().await {
        Ok(adapter) => Some(adapter),
        Err(e) => {
            tracing::warn!("构建 115 Adapter 失败，资源删除时将仅清理本地记录：{:?}", e);
            None
        }
    };

    let mut canceled_share_count = 0usize;
    let mut cancel_failed_count = 0usize;

    if let Some(adapter) = adapter.as_ref() {
        for share in &shares {
            let code_opt = share
                .share_code
                .clone()
                .or_else(|| extract_share_id_from_url(&share.share_url).ok());

            if let Some(code) = code_opt {
                match adapter.cancel_share(&code).await {
                    Ok(_) => canceled_share_count += 1,
                    Err(e) => {
                        cancel_failed_count += 1;
                        tracing::warn!(
                            "资源 #{} 删除时取消分享 #{} 失败（继续清理本地记录）：{:?}",
                            id,
                            share.id,
                            e
                        );
                    }
                }
            }
        }
    }

    // 先删 shares，避免 resources 外键 ON DELETE SET NULL 后留下孤儿分享记录。
    let deleted_share_rows = sqlx::query("DELETE FROM shares WHERE resource_id = $1")
        .bind(id)
        .execute(&state.db)
        .await?
        .rows_affected();

    // resource_files 由外键 ON DELETE CASCADE 自动删除。
    let deleted_resource_rows = sqlx::query("DELETE FROM resources WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?
        .rows_affected();

    tracing::info!(
        "🗑️ 删除资源 #{}：resources={} shares={} canceled={} cancel_failed={}",
        id,
        deleted_resource_rows,
        deleted_share_rows,
        canceled_share_count,
        cancel_failed_count
    );

    Ok(Json(serde_json::json!({
        "deleted": deleted_resource_rows > 0,
        "resource_id": id,
        "shares_deleted": deleted_share_rows,
        "shares_canceled": canceled_share_count,
        "share_cancel_failed": cancel_failed_count,
    })))
}

/// GET /api/resources/:id/shares
pub async fn get_resource_shares(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<Share>>> {
    // 确认资源存在
    let exists = sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM resources WHERE id = $1 AND status = 'active'",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    if exists.is_none() {
        return Err(AppError::NotFound(format!("资源 #{} 不存在", id)));
    }

    let shares = sqlx::query_as::<_, Share>(
        r#"
        SELECT id, resource_id, share_url, pick_code, share_code,
               share_title, share_type, file_count, total_size,
               status, last_checked_at, created_at
        FROM shares WHERE resource_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(shares))
}

/// GET /api/resources/:id/files
pub async fn get_resource_files(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<ResourceFile>>> {
    let exists = sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM resources WHERE id = $1 AND status = 'active'",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    if exists.is_none() {
        return Err(AppError::NotFound(format!("资源 #{} 不存在", id)));
    }

    let files = sqlx::query_as::<_, ResourceFile>(
        r#"
        SELECT id, resource_id, file_name, file_path, file_size,
               file_ext, media_type, season, episode, quality,
               source, codec, audio, subtitle_info,
               cloud_file_id, pick_code, strm_path, created_at
        FROM resource_files WHERE resource_id = $1
        ORDER BY season NULLS LAST, episode NULLS LAST, file_name
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(files))
}

// ─────────────────────────────────────────────
// GET /api/folders?cid=0  →  列出 115 子目录（文件夹选择器专用）
// ─────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct FolderItem {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct FolderQuery {
    #[serde(default = "default_folder_cid")]
    pub cid: String,
}

fn default_folder_cid() -> String {
    "0".to_string()
}

pub async fn list_folders(
    State(state): State<AppState>,
    Query(params): Query<FolderQuery>,
) -> Result<Json<Vec<FolderItem>>> {
    let adapter = state
        .build_adapter()
        .await
        .map_err(|e| AppError::Config(e.to_string()))?;

    let entries = adapter
        .list_files(&params.cid)
        .await
        .map_err(|e| AppError::Api115(e.to_string()))?;

    let folders: Vec<FolderItem> = entries
        .into_iter()
        .filter(|f| f.is_dir)
        .map(|f| FolderItem {
            id: f.file_id.unwrap_or_default(),
            name: f.name,
        })
        .collect();

    Ok(Json(folders))
}

// ─────────────────────────────────────────────
// GET /api/folder-name?cid=xxx  →  查询单个目录名称（设置页显示用）
// ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FolderNameQuery {
    pub cid: String,
}

pub async fn get_folder_name(
    State(state): State<AppState>,
    Query(params): Query<FolderNameQuery>,
) -> Result<Json<serde_json::Value>> {
    if params.cid.is_empty() || params.cid == "0" {
        return Ok(Json(serde_json::json!({ "cid": params.cid, "name": "根目录" })));
    }
    let adapter = state
        .build_adapter()
        .await
        .map_err(|e| AppError::Config(e.to_string()))?;

    let name = adapter
        .get_folder_name(&params.cid)
        .await
        .map_err(|e| AppError::Api115(e.to_string()))?;

    Ok(Json(serde_json::json!({ "cid": params.cid, "name": name })))
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
