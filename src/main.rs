use std::collections::HashMap;
use std::sync::Arc;

use axum::{routing::get, Router};
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

    /// 构建 115 适配器，自动使用最新 Cookie（DB 设置 > env 变量）
    pub async fn build_adapter(&self) -> anyhow::Result<crate::adapters::Adapter115> {
        let cookie = self
            .get_setting("account_115_cookie")
            .await
            .unwrap_or_else(|| self.config.account_115_cookie.clone());
        crate::adapters::Adapter115::new(&cookie, Arc::clone(&self.config))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载 .env 文件
    dotenvy::dotenv().ok();

    // 初始化日志/追踪系统
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("hidden=debug,tower_http=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .pretty()
        .init();

    info!("🏔️  洞天福地 (Hidden) v{} 启动中...", env!("CARGO_PKG_VERSION"));

    // 加载配置
    let config = Arc::new(Config::from_env()?);
    info!("✅ 配置加载完成");

    // 连接数据库
    let db = create_pool(&config.database_url).await?;
    // 运行数据库迁移
    sqlx::migrate!("./migrations").run(&db).await?;
    info!("✅ 数据库已连接，迁移完成");

    // 连接 Redis
    let redis = redis::Client::open(config.redis_url.clone())
        .map_err(|e| anyhow::anyhow!("Redis 连接失败: {}", e))?;
    {
        let mut conn = redis.get_async_connection().await?;
        redis::cmd("PING").query_async::<_, ()>(&mut conn).await?;
    }
    info!("✅ Redis 已连接");

    // 从 DB 加载运行时设置缓存（覆盖 env 配置，如 Cookie）
    let settings: Arc<RwLock<HashMap<String, String>>> =
        Arc::new(RwLock::new(HashMap::new()));
    {
        let rows = sqlx::query!("SELECT key, value FROM settings")
            .fetch_all(&db)
            .await
            .unwrap_or_default();
        let mut map = settings.write().await;
        for row in rows {
            if !row.value.is_empty() {
                map.insert(row.key, row.value);
            }
        }
        info!("✅ 运行时设置已加载（{} 条非空配置）", map.len());
    }

    // 构建应用状态
    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        redis: redis.clone(),
        settings,
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
