use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::info;

use crate::{
    error::{AppError, Result},
    AppState,
};

// ─────────────────────────────────────────────
// 响应类型
// ─────────────────────────────────────────────

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SettingItem {
    pub key: String,
    pub value: String,
    pub description: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// ─────────────────────────────────────────────
// GET /api/settings  →  所有配置项列表
// ─────────────────────────────────────────────

pub async fn list_settings(State(state): State<AppState>) -> Result<Json<Vec<SettingItem>>> {
    let rows = sqlx::query_as::<_, SettingItem>(
        "SELECT key, value, description, updated_at FROM settings ORDER BY key",
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(rows))
}

// ─────────────────────────────────────────────
// PUT /api/settings  →  批量更新配置项
// ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsBody {
    pub settings: HashMap<String, String>,
}

pub async fn update_settings(
    State(state): State<AppState>,
    Json(body): Json<UpdateSettingsBody>,
) -> Result<Json<serde_json::Value>> {
    let mut updated = 0usize;

    for (key, value) in &body.settings {
        sqlx::query(
            r#"
            INSERT INTO settings (key, value)
            VALUES ($1, $2)
            ON CONFLICT (key) DO UPDATE
                SET value = EXCLUDED.value, updated_at = NOW()
            "#,
        )
        .bind(key)
        .bind(value)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

        // 同步到内存缓存（使 cookie 等立即生效，无需重启）
        if value.is_empty() {
            state.settings.write().await.remove(key);
        } else {
            state.settings.write().await.insert(key.clone(), value.clone());
        }

        updated += 1;
    }

    info!("⚙️  更新了 {} 个配置项", updated);
    Ok(Json(serde_json::json!({ "ok": true, "updated": updated })))
}
