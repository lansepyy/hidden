use std::time::Duration;

use anyhow::Result;
use tracing::{error, info, warn};

use crate::{
    services::{check_share_rate, record_share_created, Organizer, TmdbClient},
    utils::parse_file_name,
    AppState,
};

const TASK_QUEUE_KEY: &str = "hidden:task_queue";
/// 队列为空时等待时间（秒）
const IDLE_SLEEP_SECS: u64 = 3;

/// Worker 主循环：从 Redis 队列取任务并处理
pub async fn run_worker_loop(state: AppState) -> Result<()> {
    info!("⚙️  Worker 循环启动，等待任务...");

    loop {
        // BRPOP 阻塞等待（5 秒超时）
        let task_id = match pop_task(&state.redis).await {
            Ok(Some(id)) => id,
            Ok(None) => {
                // 队列为空
                tokio::time::sleep(Duration::from_secs(IDLE_SLEEP_SECS)).await;
                continue;
            }
            Err(e) => {
                error!("读取 Redis 队列失败：{:?}", e);
                tokio::time::sleep(Duration::from_secs(IDLE_SLEEP_SECS)).await;
                continue;
            }
        };

        info!("📤 获取任务 #{}，开始处理", task_id);

        // 检查任务是否应当处理（防止重复）
        let status = get_task_status(&state.db, task_id).await;
        match status.as_deref() {
            Some("pending") => {}
            Some(s) => {
                warn!("任务 #{} 状态为 {}，跳过", task_id, s);
                continue;
            }
            None => {
                warn!("任务 #{} 不存在，跳过", task_id);
                continue;
            }
        }

        // 处理任务（错误不应崩溃 Worker）
        if let Err(e) = process_task(state.clone(), task_id).await {
            error!("❌ 任务 #{} 处理失败：{:?}", task_id, e);
            mark_task_failed(&state.db, task_id, &e.to_string()).await;
        }
    }
}

// ─────────────────────────────────────────────
// 内部函数
// ─────────────────────────────────────────────

/// 从 Redis 队列弹出一个任务 ID（非阻塞 RPOP）
async fn pop_task(redis_client: &redis::Client) -> Result<Option<i64>> {
    let mut conn = redis_client.get_async_connection().await?;

    let result: Option<i64> = redis::cmd("RPOP")
        .arg(TASK_QUEUE_KEY)
        .query_async(&mut conn)
        .await?;

    Ok(result)
}

/// 获取任务当前状态
async fn get_task_status(db: &sqlx::PgPool, task_id: i64) -> Option<String> {
    sqlx::query_scalar!("SELECT status FROM import_tasks WHERE id = $1", task_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

/// 将任务标记为失败
async fn mark_task_failed(db: &sqlx::PgPool, task_id: i64, msg: &str) {
    let _ = sqlx::query!(
        "UPDATE import_tasks SET status = 'failed', error_message = $1, current_step = NULL WHERE id = $2",
        msg,
        task_id
    )
    .execute(db)
    .await;
}

/// 更新任务状态和当前步骤
async fn update_task_step(
    db: &sqlx::PgPool,
    task_id: i64,
    status: &str,
    step: &str,
) {
    let _ = sqlx::query!(
        "UPDATE import_tasks SET status = $1, current_step = $2 WHERE id = $3",
        status,
        step,
        task_id,
    )
    .execute(db)
    .await;
}

// ─────────────────────────────────────────────
// 任务处理状态机
// ─────────────────────────────────────────────

/// 完整处理一个导入任务
///
/// 状态流转：pending → parsing → waiting_space → transferring → organizing → sharing → completed
async fn process_task(state: AppState, task_id: i64) -> Result<()> {
    let adapter = state.build_adapter().await?;

    // ── Step 1: 解析分享链接 ─────────────────────
    update_task_step(&state.db, task_id, "parsing", "解析分享链接").await;

    let task = sqlx::query!(
        "SELECT source_share_url, source_pick_code FROM import_tasks WHERE id = $1",
        task_id
    )
    .fetch_one(&state.db)
    .await?;

    // 从 URL 提取 share_id
    let share_id = extract_share_id(&task.source_share_url)?;
    let share_info = adapter
        .parse_share(&share_id, task.source_pick_code.as_deref())
        .await?;

    info!(
        "📋 任务 #{} 解析完成：{} 个文件，{} bytes",
        task_id, share_info.file_count, share_info.total_size
    );

    // 更新任务总大小和文件数
    sqlx::query!(
        "UPDATE import_tasks SET total_size = $1, total_files = $2 WHERE id = $3",
        share_info.total_size,
        share_info.file_count as i32,
        task_id,
    )
    .execute(&state.db)
    .await?;

    // ── Step 2: 检查配额 ─────────────────────────
    update_task_step(&state.db, task_id, "waiting_space", "检查存储空间").await;

    let quota = adapter.get_quota().await?;
    if quota.free < share_info.total_size + (1024 * 1024 * 1024) {
        // 剩余空间不足（要求至少多 1GB 余量）
        return Err(anyhow::anyhow!(
            "存储空间不足：剩余 {} bytes，需要 {} bytes",
            quota.free,
            share_info.total_size
        ));
    }

    // ── Step 3: 转存文件 ─────────────────────────
    update_task_step(&state.db, task_id, "transferring", "转存文件到云盘").await;

    let file_ids: Vec<&str> = share_info
        .tree
        .iter()
        .filter_map(|f| f.file_id.as_deref())
        .collect();

    let transfer_ok = adapter
        .transfer_share(
            &share_id,
            task.source_pick_code.as_deref(),
            &file_ids,
            &state.config.account_115_temp_folder_id,
        )
        .await?;

    if !transfer_ok {
        return Err(anyhow::anyhow!("文件转存失败"));
    }

    info!("✅ 任务 #{} 转存完成", task_id);

    // ── Step 4: 整理（去广告 + TMDB 匹配 + 重命名 + 移动）────
    update_task_step(&state.db, task_id, "organizing", "整理文件结构").await;

    // 列出转存后临时目录中的真实文件（获取云盘实际 ID）
    let temp_files = adapter
        .list_files(&state.config.account_115_temp_folder_id)
        .await
        .unwrap_or_else(|e| {
            warn!("列举临时目录失败，退回原始文件列表：{:?}", e);
            share_info.tree.clone()
        });

    // 从文件名推断标题和年份（取第一个视频文件）
    let inferred = temp_files
        .iter()
        .filter(|f| !f.is_dir && crate::utils::is_video_file(&f.name))
        .map(|f| parse_file_name(&f.name))
        .find(|p| !p.title.is_empty());

    // TMDB 匹配
    let tmdb_result = if let Some(ref parsed) = inferred {
        match TmdbClient::new(&state.config) {
            Ok(client) => {
                let r = client.smart_search(&parsed.title, parsed.year).await;
                if let Some(ref t) = r {
                    info!("🎬 TMDB 匹配成功：{} ({:?})", t.title(), t.year());
                }
                r
            }
            Err(e) => {
                warn!("TMDB 客户端初始化失败（跳过匹配）：{:?}", e);
                None
            }
        }
    } else {
        None
    };

    // 执行整理（删广告 + 重命名 + 移动）
    let organize_results = Organizer::new(&adapter, &state.config)
        .organize(&temp_files, tmdb_result.as_ref())
        .await
        .unwrap_or_else(|e| {
            warn!("整理步骤出错（非致命，继续执行）：{:?}", e);
            vec![]
        });

    // 将资源元数据写入 resources 表（若 TMDB 匹配到）
    let resource_id: Option<i64> = if let Some(ref tmdb) = tmdb_result {
        match sqlx::query_scalar!(
            r#"
            INSERT INTO resources
                (title, original_title, year, resource_type,
                 tmdb_id, overview, poster_url, backdrop_url, status)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active')
            RETURNING id
            "#,
            tmdb.title(),
            tmdb.original_title(),
            tmdb.year(),
            tmdb.media_type(),
            tmdb.tmdb_id(),
            tmdb.overview(),
            tmdb.poster_url(),
            tmdb.backdrop_url(),
        )
        .fetch_one(&state.db)
        .await
        {
            Ok(rid) => {
                info!("📝 资源 #{} 已写入数据库：{}", rid, tmdb.title());
                // 关联整理后的文件
                for r in &organize_results {
                    let _ = sqlx::query!(
                        r#"
                        INSERT INTO resource_files
                            (resource_id, file_name, cloud_file_id)
                        VALUES ($1, $2, $3)
                        "#,
                        rid,
                        r.final_name,
                        r.file_id,
                    )
                    .execute(&state.db)
                    .await;
                }
                Some(rid)
            }
            Err(e) => {
                warn!("资源入库失败（非致命）：{:?}", e);
                None
            }
        }
    } else {
        None
    };

    // ── Step 5: 创建新分享 ───────────────────────
    update_task_step(&state.db, task_id, "sharing", "创建分享链接").await;

    let share_file_ids: Vec<String> = if !organize_results.is_empty() {
        organize_results.iter().map(|r| r.file_id.clone()).collect()
    } else {
        // 整理失败时退回原始文件 ID
        file_ids.iter().map(|s| s.to_string()).collect()
    };

    if !share_file_ids.is_empty() {
        // 检查分享限速
        let allowed = check_share_rate(
            &state.redis,
            state.config.share_max_create_per_minute,
            state.config.share_max_create_per_hour,
            state.config.share_max_create_per_day,
        )
        .await
        .unwrap_or(true);

        if !allowed {
            warn!("任务 #{} 创建分享被限速，延迟后重试", task_id);
            let wait = state.config.share_min_interval_secs * 1000
                + rand::random::<u64>() % (state.config.share_random_jitter_secs * 1000 + 1);
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }

        let id_refs: Vec<&str> = share_file_ids.iter().map(|s| s.as_str()).collect();
        let share_title = tmdb_result
            .as_ref()
            .map(|t| {
                let year_str = t.year().map(|y| format!(" ({})", y)).unwrap_or_default();
                format!("{}{}", t.title(), year_str)
            })
            .unwrap_or_else(|| "Hidden 导入资源".to_string());

        match adapter.create_share(&id_refs, Some(&share_title), 7).await {
            Ok(share_result) => {
                let _ = record_share_created(&state.redis).await;

                sqlx::query!(
                    r#"
                    INSERT INTO shares
                        (resource_id, share_url, pick_code, share_code, share_title,
                         share_type, file_count, total_size, status)
                    VALUES ($1, $2, $3, $4, $5, 'folder',
                            $6, $7, 'active')
                    "#,
                    resource_id,
                    share_result.share_url,
                    share_result.pick_code,
                    share_result.share_id,
                    share_title,
                    share_file_ids.len() as i32,
                    share_info.total_size,
                )
                .execute(&state.db)
                .await?;

                info!("🔗 任务 #{} 分享链接已创建：{}", task_id, share_result.share_url);
            }
            Err(e) => {
                warn!("任务 #{} 创建分享失败（非致命）：{:?}", task_id, e);
            }
        }
    }

    // ── Step 6: 标记完成 ─────────────────────────
    sqlx::query!(
        "UPDATE import_tasks SET status = 'completed', current_step = '完成' WHERE id = $1",
        task_id
    )
    .execute(&state.db)
    .await?;

    info!("🎉 任务 #{} 已完成", task_id);
    Ok(())
}

/// 从分享 URL 提取 share_id
/// 支持格式：
/// - https://115.com/s/xxxxx
/// - https://115.com/s/xxxxx?password=yyyyy
fn extract_share_id(url: &str) -> Result<String> {
    url.split("/s/")
        .last()
        .map(|s| s.split('?').next().unwrap_or(s).to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("无效的 115 分享 URL：{}", url))
}
