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
    let adapter = state.build_adapter().await?;;

    let shares = sqlx::query!(
        "SELECT id, share_url, pick_code FROM shares WHERE status = 'active'"
    )
    .fetch_all(&state.db)
    .await?;

    info!("🔍 共需检查 {} 条分享链接", shares.len());

    for share in shares {
        // 从 share_url 中提取 share_id（格式：https://115.com/s/{id}）
        let share_id = share
            .share_url
            .split("/s/")
            .last()
            .unwrap_or("")
            .to_string();

        let alive = adapter
            .verify_share(&share_id, share.pick_code.as_deref())
            .await
            .unwrap_or(false);

        let new_status = if alive { "active" } else { "inactive" };
        let now = chrono::Utc::now();

        sqlx::query!(
            "UPDATE shares SET status = $1, last_checked_at = $2 WHERE id = $3",
            new_status,
            now,
            share.id,
        )
        .execute(&state.db)
        .await?;

        if !alive {
            info!("⚠️  分享 #{} 已失效", share.id);
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

/// 清理 115 中设定的临时文件夹
async fn run_temp_cleanup(state: AppState) -> Result<()> {
    // 目前仅记录日志，具体清理逻辑留待扩展
    info!(
        "🗑️  临时清理开始（临时目录 ID: {}）",
        state.config.account_115_temp_folder_id
    );
    Ok(())
}
