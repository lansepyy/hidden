use axum::{routing::get, Router};

use crate::AppState;

pub mod health;
pub mod resources;
pub mod settings;
pub mod shares;
pub mod tasks;
pub mod webui;

/// 构建全部 API 路由
pub fn router() -> Router<AppState> {
    Router::new()
        // 健康检查
        .route("/health", get(health::health_check))
        .route("/stats", get(health::get_stats))
        .route("/logs", get(health::get_logs))
        .route("/logs/level", get(health::get_log_level).put(health::set_log_level))
        // 运行时配置
        .route(
            "/settings",
            get(settings::list_settings).put(settings::update_settings),
        )
        // 任务管理
        .route("/tasks", get(tasks::list_tasks).post(tasks::create_task))
        .route(
            "/tasks/import-share",
            axum::routing::post(tasks::create_task),
        )
        .route("/tasks/:id", get(tasks::get_task).delete(tasks::delete_task))
        .route("/tasks/:id/retry", axum::routing::post(tasks::retry_task))
        .route(
            "/tasks/:id/cancel",
            axum::routing::post(tasks::cancel_task),
        )
        // 资源搜索
        .route("/resources", get(resources::list_resources))
        .route("/resources/popular", get(resources::popular_resources))
        .route("/resources/search-tmdb", get(resources::search_tmdb))
        .route(
            "/resources/:id",
            get(resources::get_resource).delete(resources::delete_resource),
        )
        .route("/resources/:id/shares", get(resources::get_resource_shares))
        .route("/resources/:id/files", get(resources::get_resource_files))
        .route(
            "/resources/:id/reorganize",
            axum::routing::post(resources::reorganize_resource),
        )
        // 115 文件夹浏览（设置页用）
        .route("/folders", get(resources::list_folders))
        .route("/folder-name", get(resources::get_folder_name))
        // 分享管理
        .route("/shares", get(shares::list_shares))
        .route("/shares/:id", get(shares::get_share).delete(shares::delete_share))
        .route(
            "/shares/:id/check",
            axum::routing::post(shares::check_share),
        )
        .route(
            "/shares/:id/rebuild",
            axum::routing::post(shares::rebuild_share),
        )
}
