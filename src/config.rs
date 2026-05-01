use anyhow::Context;

/// 应用全局配置，从环境变量加载
#[derive(Debug, Clone)]
pub struct Config {
    // 服务器
    pub host: String,
    pub port: u16,
    pub app_env: String,

    // 数据库
    pub database_url: String,

    // Redis
    pub redis_url: String,

    // 115 账号
    pub account_115_cookie: String,
    pub account_115_root_folder_id: String,
    pub account_115_temp_folder_id: String,
    pub account_115_request_interval_ms: u64,
    pub account_115_retry_times: u32,

    // TMDB
    pub tmdb_api_key: String,
    pub tmdb_language: String,

    // JWT
    pub jwt_secret: String,
    pub jwt_expiration_hours: u64,

    // 转存限制
    pub transfer_max_size_gb: u64,
    pub transfer_max_file_count: u32,
    pub transfer_min_free_space_gb: u64,

    // 分享限速（风控规避）
    pub share_max_create_per_minute: u32,
    pub share_max_create_per_hour: u32,
    pub share_max_create_per_day: u32,
    pub share_min_interval_secs: u64,
    pub share_random_jitter_secs: u64,

    // 清理规则
    pub clean_min_video_size_mb: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            // 服务器
            host: env_str("HOST").unwrap_or_else(|| "0.0.0.0".to_string()),
            port: env_parse("PORT").unwrap_or(8080),
            app_env: env_str("APP_ENV").unwrap_or_else(|| "development".to_string()),

            // 数据库
            database_url: env_require("DATABASE_URL")
                .context("DATABASE_URL 未配置")?,

            // Redis
            redis_url: env_str("REDIS_URL")
                .unwrap_or_else(|| "redis://127.0.0.1:6379".to_string()),

            // 115
            account_115_cookie: env_str("ACCOUNT_115_COOKIE")
                .unwrap_or_default(),
            account_115_root_folder_id: env_str("ACCOUNT_115_ROOT_FOLDER_ID")
                .unwrap_or_default(),
            account_115_temp_folder_id: env_str("ACCOUNT_115_TEMP_FOLDER_ID")
                .unwrap_or_default(),
            account_115_request_interval_ms: env_parse("ACCOUNT_115_REQUEST_INTERVAL_MS")
                .unwrap_or(1500),
            account_115_retry_times: env_parse("ACCOUNT_115_RETRY_TIMES")
                .unwrap_or(3),

            // TMDB
            tmdb_api_key: env_str("TMDB_API_KEY").unwrap_or_default(),
            tmdb_language: env_str("TMDB_LANGUAGE")
                .unwrap_or_else(|| "zh-CN".to_string()),

            // JWT
            jwt_secret: env_str("JWT_SECRET")
                .unwrap_or_else(|| "hidden-secret-change-in-production".to_string()),
            jwt_expiration_hours: env_parse("JWT_EXPIRATION_HOURS").unwrap_or(24),

            // 转存限制
            transfer_max_size_gb: env_parse("TRANSFER_MAX_SIZE_GB").unwrap_or(80),
            transfer_max_file_count: env_parse("TRANSFER_MAX_FILE_COUNT").unwrap_or(200),
            transfer_min_free_space_gb: env_parse("TRANSFER_MIN_FREE_SPACE_GB").unwrap_or(20),

            // 分享限速
            share_max_create_per_minute: env_parse("SHARE_MAX_CREATE_PER_MINUTE").unwrap_or(2),
            share_max_create_per_hour: env_parse("SHARE_MAX_CREATE_PER_HOUR").unwrap_or(60),
            share_max_create_per_day: env_parse("SHARE_MAX_CREATE_PER_DAY").unwrap_or(300),
            share_min_interval_secs: env_parse("SHARE_MIN_INTERVAL_SECS").unwrap_or(30),
            share_random_jitter_secs: env_parse("SHARE_RANDOM_JITTER_SECS").unwrap_or(10),

            // 清理规则
            clean_min_video_size_mb: env_parse("CLEAN_MIN_VIDEO_SIZE_MB").unwrap_or(100),
        })
    }

    pub fn is_production(&self) -> bool {
        self.app_env == "production"
    }
}

fn env_str(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn env_require(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| anyhow::anyhow!("缺少必要环境变量: {}", key))
}

fn env_parse<T>(key: &str) -> Option<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Debug,
{
    std::env::var(key).ok()?.parse().ok()
}
