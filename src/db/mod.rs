use sqlx::{postgres::PgPoolOptions, PgPool};
use tracing::info;

pub mod models;
pub use models::*;

/// 创建数据库连接池
pub async fn create_pool(database_url: &str) -> anyhow::Result<PgPool> {
    info!("正在连接数据库...");
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(database_url)
        .await?;
    Ok(pool)
}
