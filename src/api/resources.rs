use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    db::{Resource, ResourceFile, Share},
    error::{AppError, Result},
    services::{organizer::build_standard_name, TmdbClient},
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

/// GET /api/resources/popular?type=movie|tv|now_playing|upcoming|top_rated_movie|airing_today|top_rated_tv&limit=20
///
/// 资源库首页的 TMDB 热门入口：点击热门条目后由前端带标题搜索本地数据库。
pub async fn popular_resources(
    State(state): State<AppState>,
    Query(q): Query<PopularQuery>,
) -> Result<Json<Vec<PopularResourceItem>>> {
    let limit = q.limit.clamp(1, 100);
    let runtime_config = state.runtime_config().await;
    let client = TmdbClient::new(&runtime_config).map_err(|e| AppError::TmdbApi(e.to_string()))?;

    let items = match q.resource_type.as_deref().unwrap_or("movie") {
        "tv" | "tv_popular" => client
            .popular_tv(limit)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .map(PopularResourceItem::from_tv)
            .collect(),
        "airing_today" => client
            .airing_today_tv(limit)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .map(PopularResourceItem::from_tv)
            .collect(),
        "top_rated_tv" => client
            .top_rated_tv(limit)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .map(PopularResourceItem::from_tv)
            .collect(),
        "now_playing" => client
            .now_playing_movies(limit)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .map(PopularResourceItem::from_movie)
            .collect(),
        "upcoming" => client
            .upcoming_movies(limit)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .map(PopularResourceItem::from_movie)
            .collect(),
        "top_rated_movie" => client
            .top_rated_movies(limit)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .map(PopularResourceItem::from_movie)
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
                "不支持的热门类型：{}",
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

// ─────────────────────────────────────────────
// POST /api/resources/:id/reorganize  →  手动整理（修正标题/TMDB ID/类型）
// ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ReorganizeRequest {
    /// 修正后的显示标题（必填）
    pub title: String,
    /// TMDB ID（可选，填写后自动从 TMDB 拉取海报/简介/年份）
    pub tmdb_id: Option<i64>,
    /// 资源类型：movie / tv / anime / variety / documentary / other
    pub resource_type: Option<String>,
    /// 年份（可选，覆盖 TMDB 年份）
    pub year: Option<i32>,
    /// 是否同时重命名关联的 115 文件（默认 false，避免误操作）
    pub rename_files: Option<bool>,
}

/// POST /api/resources/:id/reorganize
///
/// 手动整理：修正数据库中的标题/TMDB 元数据/类型；
/// 若 rename_files=true 则同时尝试重命名 115 上对应的文件。
pub async fn reorganize_resource(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ReorganizeRequest>,
) -> Result<Json<ResourceResponse>> {
    let title = req.title.trim().to_string();
    if title.is_empty() {
        return Err(AppError::BadRequest("title 不能为空".to_string()));
    }

    // 确认资源存在
    let resource = sqlx::query_as::<_, Resource>(
        "SELECT id, title, original_title, year, resource_type, tmdb_id, imdb_id,
                overview, poster_url, backdrop_url, status, created_at, updated_at
         FROM resources WHERE id = $1 AND status = 'active'",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("资源 #{} 不存在", id)))?;

    // 如果提供了 tmdb_id，从 TMDB 拉取最新元数据
    let mut poster_url = resource.poster_url.clone();
    let mut backdrop_url = resource.backdrop_url.clone();
    let mut overview = resource.overview.clone();
    let mut original_title = resource.original_title.clone();
    let mut year = req.year.or(resource.year);
    let resource_type = req
        .resource_type
        .clone()
        .unwrap_or_else(|| resource.resource_type.clone());
    let tmdb_id = req.tmdb_id.or(resource.tmdb_id);

    if let Some(tid) = req.tmdb_id {
        let runtime_config = state.runtime_config().await;
        if let Ok(client) = TmdbClient::new(&runtime_config) {
            // 根据类型决定调哪个接口
            let is_tv = matches!(resource_type.as_str(), "tv" | "anime");
            if is_tv {
                if let Ok(tv) = client.tv_detail(tid).await {
                    original_title = Some(tv.original_name.clone());
                    if year.is_none() {
                        year = tv.first_air_date.as_deref()
                            .and_then(|d| d.split('-').next())
                            .and_then(|y| y.parse().ok());
                    }
                    overview = tv.overview.clone();
                    poster_url = tv.poster_path.as_deref()
                        .filter(|p| !p.is_empty())
                        .map(|p| format!("https://image.tmdb.org/t/p/w500{}", p));
                    backdrop_url = tv.backdrop_path.as_deref()
                        .filter(|p| !p.is_empty())
                        .map(|p| format!("https://image.tmdb.org/t/p/w500{}", p));
                }
            } else {
                if let Ok(mv) = client.movie_detail(tid).await {
                    original_title = Some(mv.original_title.clone());
                    if year.is_none() {
                        year = mv.release_date.as_deref()
                            .and_then(|d| d.split('-').next())
                            .and_then(|y| y.parse().ok());
                    }
                    overview = mv.overview.clone();
                    poster_url = mv.poster_path.as_deref()
                        .filter(|p| !p.is_empty())
                        .map(|p| format!("https://image.tmdb.org/t/p/w500{}", p));
                    backdrop_url = mv.backdrop_path.as_deref()
                        .filter(|p| !p.is_empty())
                        .map(|p| format!("https://image.tmdb.org/t/p/w500{}", p));
                }
            }
        }
    }

    // 更新数据库
    sqlx::query(
        "UPDATE resources SET
            title = $1, original_title = $2, year = $3, resource_type = $4,
            tmdb_id = $5, overview = $6, poster_url = $7, backdrop_url = $8,
            updated_at = NOW()
         WHERE id = $9",
    )
    .bind(&title)
    .bind(&original_title)
    .bind(year)
    .bind(&resource_type)
    .bind(tmdb_id)
    .bind(&overview)
    .bind(&poster_url)
    .bind(&backdrop_url)
    .bind(id)
    .execute(&state.db)
    .await?;

    // 可选：重命名 115 文件
    if req.rename_files.unwrap_or(false) {
        let files = sqlx::query_as::<_, ResourceFile>(
            "SELECT id, resource_id, file_name, file_path, file_size, file_ext,
                    media_type, season, episode, quality, source, codec, audio,
                    subtitle_info, cloud_file_id, pick_code, strm_path, created_at
             FROM resource_files WHERE resource_id = $1",
        )
        .bind(id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        if !files.is_empty() {
            if let Ok(adapter) = state.build_adapter().await {
                for file in &files {
                    let Some(ref cloud_id) = file.cloud_file_id else { continue };
                    let new_name = build_standard_name(&file.file_name, None)
                        .unwrap_or_else(|| file.file_name.clone());
                    if new_name != file.file_name {
                        let _ = adapter.rename_file(cloud_id, &new_name).await;
                        let _ = sqlx::query(
                            "UPDATE resource_files SET file_name = $1 WHERE id = $2",
                        )
                        .bind(&new_name)
                        .bind(file.id)
                        .execute(&state.db)
                        .await;
                    }
                }
            }
        }
    }

    // 返回更新后的资源
    let updated = sqlx::query_as::<_, Resource>(
        "SELECT id, title, original_title, year, resource_type, tmdb_id, imdb_id,
                overview, poster_url, backdrop_url, status, created_at, updated_at
         FROM resources WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;

    tracing::info!("✏️  手动整理资源 #{}: {} → {}", id, resource.title, title);

    Ok(Json(updated.into()))
}

/// GET /api/resources/search-tmdb?q=xxx&type=movie|tv
///
/// 搜索 TMDB 供手动整理时选择正确条目。
#[derive(Debug, Deserialize)]
pub struct TmdbSearchQuery {
    pub q: String,
    #[serde(rename = "type", default = "default_tmdb_search_type")]
    pub resource_type: String,
    pub year: Option<i32>,
}

fn default_tmdb_search_type() -> String {
    "movie".to_string()
}

#[derive(Debug, Serialize)]
pub struct TmdbSearchItem {
    pub tmdb_id: i64,
    pub title: String,
    pub original_title: String,
    pub year: Option<i32>,
    pub resource_type: String,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
}

pub async fn search_tmdb(
    State(state): State<AppState>,
    Query(q): Query<TmdbSearchQuery>,
) -> Result<Json<Vec<TmdbSearchItem>>> {
    let query = q.q.trim().to_string();
    if query.is_empty() {
        return Err(AppError::BadRequest("q 不能为空".to_string()));
    }
    let runtime_config = state.runtime_config().await;
    let client = TmdbClient::new(&runtime_config).map_err(|e| AppError::TmdbApi(e.to_string()))?;

    let items: Vec<TmdbSearchItem> = if q.resource_type == "tv" {
        client
            .search_tv(&query, q.year)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .take(10)
            .map(|t| TmdbSearchItem {
                tmdb_id: t.id,
                title: t.name.clone(),
                original_title: t.original_name.clone(),
                year: t.first_air_date.as_deref()
                    .and_then(|d| d.split('-').next())
                    .and_then(|y| y.parse().ok()),
                resource_type: "tv".to_string(),
                overview: t.overview.clone(),
                poster_url: t.poster_path.as_deref()
                    .filter(|p| !p.is_empty())
                    .map(|p| format!("https://image.tmdb.org/t/p/w500{}", p)),
            })
            .collect()
    } else {
        client
            .search_movie(&query, q.year)
            .await
            .map_err(|e| AppError::TmdbApi(e.to_string()))?
            .into_iter()
            .take(10)
            .map(|m| TmdbSearchItem {
                tmdb_id: m.id,
                title: m.title.clone(),
                original_title: m.original_title.clone(),
                year: m.release_date.as_deref()
                    .and_then(|d| d.split('-').next())
                    .and_then(|y| y.parse().ok()),
                resource_type: "movie".to_string(),
                overview: m.overview.clone(),
                poster_url: m.poster_path.as_deref()
                    .filter(|p| !p.is_empty())
                    .map(|p| format!("https://image.tmdb.org/t/p/w500{}", p)),
            })
            .collect()
    };

    Ok(Json(items))
}
