use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

// ─────────────────────────────────────────────
// 枚举类型
// ─────────────────────────────────────────────

/// 资源类型
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, PartialEq)]
#[sqlx(type_name = "resource_type", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ResourceType {
    Movie,
    Tv,
    Anime,
    Variety,
    Documentary,
    Other,
}

impl Default for ResourceType {
    fn default() -> Self {
        Self::Other
    }
}

/// 任务状态
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, PartialEq)]
#[sqlx(type_name = "task_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Parsing,
    WaitingSpace,
    Transferring,
    TransferFailed,
    Organizing,
    Sharing,
    Verifying,
    Completed,
    Failed,
    Skipped,
}

impl Default for TaskStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// 分享链接状态
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, PartialEq)]
#[sqlx(type_name = "share_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ShareStatus {
    Active,
    Inactive,
    Failed,
    Deleted,
}

impl Default for ShareStatus {
    fn default() -> Self {
        Self::Active
    }
}

/// 账号状态
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, PartialEq)]
#[sqlx(type_name = "account_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AccountStatus {
    Active,
    Inactive,
    Banned,
    CookieExpired,
}

// ─────────────────────────────────────────────
// 数据库实体模型
// ─────────────────────────────────────────────

/// 资源元数据表
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Resource {
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 资源文件表
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ResourceFile {
    pub id: i64,
    pub resource_id: i64,
    pub file_name: String,
    pub file_path: Option<String>,
    pub file_size: Option<i64>,
    pub file_ext: Option<String>,
    pub media_type: Option<String>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub quality: Option<String>,
    pub source: Option<String>,
    pub codec: Option<String>,
    pub audio: Option<String>,
    pub subtitle_info: Option<String>,
    pub cloud_file_id: Option<String>,
    pub pick_code: Option<String>,
    pub strm_path: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// 分享链接表
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Share {
    pub id: i64,
    pub resource_id: Option<i64>,
    pub share_url: String,
    pub pick_code: Option<String>,
    pub share_code: Option<String>,
    pub share_title: Option<String>,
    pub share_type: Option<String>,
    pub file_count: Option<i32>,
    pub total_size: Option<i64>,
    pub status: String,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 导入任务表
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ImportTask {
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 导入批次表
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ImportBatch {
    pub id: i64,
    pub task_id: i64,
    pub batch_index: i32,
    pub status: String,
    pub file_count: Option<i32>,
    pub total_size: Option<i64>,
    pub temp_folder_id: Option<String>,
    pub target_folder_id: Option<String>,
    pub share_id: Option<i64>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 115 账号表
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub cookie_encrypted: String, // 加密存储
    pub root_folder_id: Option<String>,
    pub temp_folder_id: Option<String>,
    pub total_size: Option<i64>,
    pub used_size: Option<i64>,
    pub free_size: Option<i64>,
    pub status: String,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_failed_at: Option<DateTime<Utc>>,
    pub failure_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 审计日志表
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AuditLog {
    pub id: i64,
    pub user_id: Option<i64>,
    pub action: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<i64>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub ip_address: Option<String>,
    pub timestamp: DateTime<Utc>,
}
