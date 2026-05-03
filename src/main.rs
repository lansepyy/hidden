use std::collections::HashMap;
use std::sync::Arc;

use axum::{routing::get, Router};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tokio::signal;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

mod adapters;
mod api;
mod config;
mod db;
mod error;
mod services;
mod utils;
mod workers;

use config::Config;
use db::create_pool;

/// 全局应用状态，注入到所有请求处理器
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub config: Arc<Config>,
    pub redis: redis::Client,
    /// 运行时配置缓存（从 settings 表加载，可通过 WebUI 热更新）
    pub settings: Arc<RwLock<HashMap<String, String>>>,
    /// 动态日志级别控制
    pub log_level_setter: Arc<dyn Fn(String) -> anyhow::Result<()> + Send + Sync>,
    pub log_level_getter: Arc<dyn Fn() -> String + Send + Sync>,
}

impl AppState {
    /// 读取运行时设置值（DB 值 > 空值则回退到 env config）
    pub async fn get_setting(&self, key: &str) -> Option<String> {
        self.settings
            .read()
            .await
            .get(key)
            .filter(|v| !v.is_empty())
            .cloned()
    }

    /// 构建运行时配置快照（DB settings 非空值 > 启动时 env 配置）
    ///
    /// WebUI 保存的配置会进入 settings 缓存；后台任务和 API 在每次处理前
    /// 调用该方法获取最新快照，避免必须重启服务。
    pub async fn runtime_config(&self) -> Arc<Config> {
        let mut config = (*self.config).clone();
        let settings = self.settings.read().await;

        fn override_string(
            settings: &HashMap<String, String>,
            key: &str,
            target: &mut String,
        ) {
            if let Some(value) = settings.get(key).filter(|v| !v.is_empty()) {
                *target = value.clone();
            }
        }

        fn override_parse<T>(settings: &HashMap<String, String>, key: &str, target: &mut T)
        where
            T: std::str::FromStr,
        {
            if let Some(value) = settings.get(key).filter(|v| !v.is_empty()) {
                if let Ok(parsed) = value.parse::<T>() {
                    *target = parsed;
                }
            }
        }

        override_string(&settings, "account_115_cookie", &mut config.account_115_cookie);
        override_string(
            &settings,
            "account_115_root_folder_id",
            &mut config.account_115_root_folder_id,
        );
        override_string(
            &settings,
            "account_115_temp_folder_id",
            &mut config.account_115_temp_folder_id,
        );
        override_parse(
            &settings,
            "account_115_request_interval_ms",
            &mut config.account_115_request_interval_ms,
        );
        override_parse(
            &settings,
            "account_115_retry_times",
            &mut config.account_115_retry_times,
        );

        override_string(&settings, "tmdb_api_key", &mut config.tmdb_api_key);
        override_string(&settings, "tmdb_language", &mut config.tmdb_language);

        override_parse(
            &settings,
            "transfer_max_size_gb",
            &mut config.transfer_max_size_gb,
        );
        override_parse(
            &settings,
            "transfer_max_file_count",
            &mut config.transfer_max_file_count,
        );
        override_parse(
            &settings,
            "transfer_min_free_space_gb",
            &mut config.transfer_min_free_space_gb,
        );

        override_parse(
            &settings,
            "share_max_create_per_minute",
            &mut config.share_max_create_per_minute,
        );
        override_parse(
            &settings,
            "share_max_create_per_hour",
            &mut config.share_max_create_per_hour,
        );
        override_parse(
            &settings,
            "share_max_create_per_day",
            &mut config.share_max_create_per_day,
        );
        override_parse(
            &settings,
            "share_min_interval_secs",
            &mut config.share_min_interval_secs,
        );
        override_parse(
            &settings,
            "share_random_jitter_secs",
            &mut config.share_random_jitter_secs,
        );

        override_parse(
            &settings,
            "clean_min_video_size_mb",
            &mut config.clean_min_video_size_mb,
        );

        Arc::new(config)
    }

    /// 构建 115 适配器，自动使用最新 Cookie、请求间隔和重试次数（DB 设置 > env 变量）
    pub async fn build_adapter(&self) -> anyhow::Result<crate::adapters::Adapter115> {
        let config = self.runtime_config().await;
        let cookie = config.account_115_cookie.clone();
        crate::adapters::Adapter115::new(&cookie, config)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载 .env 文件
    dotenvy::dotenv().ok();

    // 初始化日志/追踪系统（支持运行时通过 WebUI 动态调整日志级别）
    let default_filter_str = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "hidden=debug,tower_http=warn,sqlx=warn".to_string());
    let env_filter = tracing_subscriber::EnvFilter::try_new(&default_filter_str)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let (filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);
    let reload_handle = Arc::new(reload_handle);
    let current_log_level = Arc::new(std::sync::RwLock::new(default_filter_str.clone()));

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_target(true))
        .init();

    info!("🏔️  洞天福地 (Hidden) v{} 启动中...", env!("CARGO_PKG_VERSION"));

    // 加载配置
    let config = Arc::new(Config::from_env()?);
    info!("✅ 配置加载完成");

    // 连接数据库（带重试，防止 postgres 初始化期间短暂不可用）
    let db = {
        let mut last_err = None;
        let mut connected = None;
        for attempt in 1..=10 {
            match create_pool(&config.database_url).await {
                Ok(pool) => {
                    connected = Some(pool);
                    break;
                }
                Err(e) => {
                    tracing::warn!("数据库连接失败（第 {}/10 次）：{}，3 秒后重试...", attempt, e);
                    last_err = Some(e);
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }
        connected.ok_or_else(|| {
            last_err.unwrap_or_else(|| anyhow::anyhow!("数据库连接失败，但未捕获到具体错误"))
        })?
    };
    // 运行数据库迁移
    sqlx::migrate!("./migrations").run(&db).await?;
    info!("✅ 数据库已连接，迁移完成");

    // 连接 Redis（带重试）
    let redis = {
        let client = redis::Client::open(config.redis_url.clone())
            .map_err(|e| anyhow::anyhow!("Redis URL 无效: {}", e))?;
        let mut last_err = None;
        for attempt in 1..=10 {
            match client.get_async_connection().await {
                Ok(mut conn) => {
                    match redis::cmd("PING").query_async::<_, String>(&mut conn).await {
                        Ok(pong) if pong.eq_ignore_ascii_case("PONG") => {
                            last_err = None;
                            break;
                        }
                        Ok(other) => {
                            let e = redis::RedisError::from((
                                redis::ErrorKind::ResponseError,
                                "Redis PING 返回异常",
                                other,
                            ));
                            tracing::warn!("Redis PING 失败（第 {}/10 次）：{}，3 秒后重试...", attempt, e);
                            last_err = Some(e);
                        }
                        Err(e) => {
                            tracing::warn!("Redis PING 失败（第 {}/10 次）：{}，3 秒后重试...", attempt, e);
                            last_err = Some(e);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Redis 连接失败（第 {}/10 次）：{}，3 秒后重试...", attempt, e);
                    last_err = Some(e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        if let Some(e) = last_err {
            return Err(anyhow::anyhow!("Redis 连接最终失败: {}", e));
        }
        client
    };
    info!("✅ Redis 已连接");

    // 从 DB 加载运行时设置缓存（覆盖 env 配置，如 Cookie）
    let settings: Arc<RwLock<HashMap<String, String>>> =
        Arc::new(RwLock::new(HashMap::new()));
    {
        let rows = sqlx::query_as::<_, (String, String)>("SELECT key, value FROM settings")
            .fetch_all(&db)
            .await
            .unwrap_or_default();
        let mut map = settings.write().await;
        for (key, value) in rows {
            if !value.is_empty() {
                map.insert(key, value);
            }
        }
        info!("✅ 运行时设置已加载（{} 条非空配置）", map.len());
    }

    // 构建日志级别动态控制闭包
    let reload_handle_w = Arc::clone(&reload_handle);
    let current_log_level_w = Arc::clone(&current_log_level);
    let log_level_setter: Arc<dyn Fn(String) -> anyhow::Result<()> + Send + Sync> =
        Arc::new(move |s: String| {
            let new_filter = tracing_subscriber::EnvFilter::try_new(&s)
                .map_err(|e| anyhow::anyhow!("无效的日志级别: {}", e))?;
            reload_handle_w
                .reload(new_filter)
                .map_err(|e| anyhow::anyhow!("更新日志级别失败: {}", e))?;
            *current_log_level_w
                .write()
                .map_err(|_| anyhow::anyhow!("日志级别状态锁已中毒"))? = s;
            Ok(())
        });
    let log_level_getter: Arc<dyn Fn() -> String + Send + Sync> = Arc::new(move || {
        current_log_level
            .read()
            .map(|level| level.clone())
            .unwrap_or_else(|_| "unknown".to_string())
    });

    // 构建应用状态
    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        redis: redis.clone(),
        settings,
        log_level_setter,
        log_level_getter,
    };

    // 启动后台 Worker 和定时任务
    let worker_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = workers::start_scheduler(worker_state).await {
            tracing::error!("调度器启动失败: {:?}", e);
        }
    });
    info!("✅ 后台 Worker 已启动");

    // 构建 Axum Router
    let app = Router::new()
        .route("/", get(api::webui::serve_index))
        .nest("/api", api::router())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // 监听地址
    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!("🚀 服务器已启动 → http://{}", addr);
    info!("📖 健康检查 → http://{}/api/health", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("🛑 服务器已关闭");
    Ok(())
}

/// 监听 Ctrl+C 优雅退出
async fn shutdown_signal() {
    signal::ctrl_c()
        .await
        .expect("安装 Ctrl+C 处理器失败");
    info!("🛑 收到关闭信号，正在优雅退出...");
}
