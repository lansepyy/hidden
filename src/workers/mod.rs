pub mod import_worker;

use anyhow::Result;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info};

use crate::AppState;

// Redis 任务队列 Key
const TASK_QUEUE_KEY: &str = "hidden:task_queue";

/// 将任务 ID 压入 Redis 队列（供 Worker 消费）
pub async fn enqueue_task(
    redis_client: &redis::Client,
    task_id: i64,
) -> crate::error::Result<()> {
    let mut conn = redis_client
        .get_async_connection()
        .await
        .map_err(crate::error::AppError::Redis)?;

    redis::cmd("LPUSH")
        .arg(TASK_QUEUE_KEY)
        .arg(task_id)
        .query_async::<_, ()>(&mut conn)
        .await
        .map_err(crate::error::AppError::Redis)?;

    info!("📥 任务 #{} 已入队", task_id);
    Ok(())
}

/// 启动后台调度器和 Worker 循环
pub async fn start_scheduler(state: AppState) -> Result<()> {
    // ─── 启动任务消费 Worker ───────────────────────
    {
        let worker_state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = import_worker::run_worker_loop(worker_state).await {
                error!("❌ Worker 循环退出：{:?}", e);
            }
        });
        info!("🚀 导入 Worker 已启动");
    }

    // ─── 启动定时调度器 ───────────────────────────
    let sched = JobScheduler::new().await?;

    // 每小时检查一次分享链接健康状态
    {
        let check_state = state.clone();
        let job = Job::new_async("0 0 * * * *", move |_id, _lock| {
            let s = check_state.clone();
            Box::pin(async move {
                info!("⏰ [定时] 开始分享链接健康检查");
                if let Err(e) = run_share_health_check(s).await {
                    error!("分享健康检查失败：{:?}", e);
                }
            })
        })?;
        sched.add(job).await?;
    }

    // 每 30 分钟检查账号存储配额
    {
        let quota_state = state.clone();
        let job = Job::new_async("0 */30 * * * *", move |_id, _lock| {
            let s = quota_state.clone();
            Box::pin(async move {
                info!("⏰ [定时] 检查账号配额");
                if let Err(e) = run_quota_check(s).await {
                    error!("配额检查失败：{:?}", e);
                }
            })
        })?;
        sched.add(job).await?;
    }

    // 每天凌晨 3 点清理临时文件夹
    {
        let cleanup_state = state.clone();
        let job = Job::new_async("0 0 3 * * *", move |_id, _lock| {
            let s = cleanup_state.clone();
            Box::pin(async move {
                info!("⏰ [定时] 清理临时文件夹");
                if let Err(e) = run_temp_cleanup(s).await {
                    error!("临时清理失败：{:?}", e);
                }
            })
        })?;
        sched.add(job).await?;
    }

    sched.start().await?;
    info!("📅 定时调度器已启动");

    // 保持运行（不要提前退出）
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
    }
}

// ─────────────────────────────────────────────
// 定时任务实现
// ─────────────────────────────────────────────

/// 对所有 active 分享链接进行存活检查
async fn run_share_health_check(state: AppState) -> Result<()> {
    let adapter = state.build_adapter().await?;

    let shares = sqlx::query_as::<_, (i64, String, Option<String>)>(
        "SELECT id, share_url, pick_code FROM shares WHERE status = 'active'",
    )
    .fetch_all(&state.db)
    .await?;

    info!("🔍 共需检查 {} 条分享链接", shares.len());

    for share in shares {
        // 从 share_url 中提取 share_id（格式：https://115.com/s/{id}）
        let share_id = match extract_share_id(&share.1) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("分享 #{} URL 无效，标记为失效：{}", share.0, e);
                String::new()
            }
        };

        let alive = if share_id.is_empty() {
            false
        } else {
            adapter
                .verify_share(&share_id, share.2.as_deref())
                .await
                .unwrap_or(false)
        };

        let new_status = if alive { "active" } else { "inactive" };
        let now = chrono::Utc::now();

        sqlx::query("UPDATE shares SET status = $1, last_checked_at = $2 WHERE id = $3")
            .bind(new_status)
            .bind(now)
            .bind(share.0)
            .execute(&state.db)
            .await?;

        if !alive {
            info!("⚠️  分享 #{} 已失效", share.0);
        }
    }

    Ok(())
}

/// 检查账号配额，不足时记录警告
async fn run_quota_check(state: AppState) -> Result<()> {
    let adapter = state.build_adapter().await?;
    let quota = adapter.get_quota().await?;

    let free_gb = quota.free / (1024 * 1024 * 1024);
    let used_pct = if quota.total > 0 {
        (quota.used * 100) / quota.total
    } else {
        0
    };

    info!(
        "💾 账号配额：已用 {}%，剩余 {} GB",
        used_pct, free_gb
    );

    if used_pct >= 90 {
        tracing::warn!("⚠️  存储空间不足！已用 {}%", used_pct);
    }

    Ok(())
}

fn extract_share_id(url: &str) -> Result<String> {
    let (_, tail) = url
        .split_once("/s/")
        .ok_or_else(|| anyhow::anyhow!("无效的 115 分享 URL：{}", url))?;

    tail.split(['?', '#'])
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("无效的 115 分享 URL：{}", url))
}

/// 清理 115 中设定的临时文件夹下由完成任务可能残留的空展目录
async fn run_temp_cleanup(state: AppState) -> Result<()> {
    let runtime_config = state.runtime_config().await;
    let temp_folder_id = &runtime_config.account_115_temp_folder_id;

    if temp_folder_id.is_empty() || temp_folder_id == "0" {
        info!("🗑️  临时目录未配置，跳过清理");
        return Ok(());
    }

    info!("🗑️  临时清理开始（临时目录 ID: {}）", temp_folder_id);

    let adapter = match state.build_adapter().await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("清理任务初始化适配器失败，跳过：{:?}", e);
            return Ok(());
        }
    };

    // 列出临时目录下的所有子目录
    let entries = match adapter.list_files(temp_folder_id).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("列举临时目录失败：{:?}", e);
            return Ok(());
        }
    };

    // 只处理 hidden-task-* 命名的子目录
    let task_dirs: Vec<_> = entries
        .iter()
        .filter(|f| f.is_dir && f.name.starts_with("hidden-task-"))
        .collect();

    if task_dirs.is_empty() {
        info!("🗑️  无需清理的临时任务目录");
        return Ok(());
    }

    // 查询已完成任务的 task_id 集合
    let completed_ids: std::collections::HashSet<String> = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM import_tasks WHERE status IN ('completed', 'failed', 'skipped')",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|id| format!("hidden-task-{}", id))
    .collect();

    let mut deleted = 0usize;
    for dir in task_dirs {
        if completed_ids.contains(&dir.name) {
            if let Some(ref dir_id) = dir.file_id {
                // 先列举子目录内容，确认小于 5 个文件时才删除（防止误删）
                let children = adapter.list_files(dir_id).await.unwrap_or_default();
                if children.len() < 5 {
                    if let Err(e) = adapter.delete_files(&[dir_id.as_str()]).await {
                        tracing::warn!("删除临时目录 {} 失败：{:?}", dir.name, e);
                    } else {
                        info!("🗑️  已清理临时目录：{}", dir.name);
                        deleted += 1;
                    }
                } else {
                    tracing::warn!("临时目录 {} 仍有 {} 个文件，跳过删除", dir.name, children.len());
                }
            }
        }
    }

    info!("🗑️  清理完成，删除 {} 个空临时目录", deleted);
    Ok(())
}
