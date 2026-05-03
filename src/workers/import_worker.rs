use std::time::Duration;

use anyhow::Result;
use tracing::{error, info, warn};

use crate::{
    adapters::Adapter115,
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
    sqlx::query_scalar::<_, String>("SELECT status FROM import_tasks WHERE id = $1")
        .bind(task_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

/// 将任务标记为失败
async fn mark_task_failed(db: &sqlx::PgPool, task_id: i64, msg: &str) {
    let _ = sqlx::query(
        "UPDATE import_tasks SET status = 'failed', error_message = $1, current_step = NULL WHERE id = $2",
    )
    .bind(msg)
    .bind(task_id)
    .execute(db)
    .await;
}

/// 更新任务状态和当前步骤
async fn update_task_step(db: &sqlx::PgPool, task_id: i64, status: &str, step: &str) {
    let _ = sqlx::query("UPDATE import_tasks SET status = $1, current_step = $2 WHERE id = $3")
        .bind(status)
        .bind(step)
        .bind(task_id)
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
    let runtime_config = state.runtime_config().await;
    let adapter = Adapter115::new(
        &runtime_config.account_115_cookie,
        runtime_config.clone(),
    )?;

    // ── Step 1: 解析分享链接 ─────────────────────
    update_task_step(&state.db, task_id, "parsing", "解析分享链接").await;

    let (source_share_url, source_pick_code) =
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT source_share_url, source_pick_code FROM import_tasks WHERE id = $1",
        )
        .bind(task_id)
        .fetch_one(&state.db)
        .await?;

    // 从 URL 提取 share_id
    let share_id = extract_share_id(&source_share_url)?;
    let share_info = adapter
        .parse_share(&share_id, source_pick_code.as_deref())
        .await?;

    info!(
        "📋 任务 #{} 解析完成：{} 个文件，{} bytes",
        task_id, share_info.file_count, share_info.total_size
    );

    // 更新任务总大小和文件数
    sqlx::query("UPDATE import_tasks SET total_size = $1, total_files = $2 WHERE id = $3")
        .bind(share_info.total_size)
        .bind(share_info.file_count as i32)
        .bind(task_id)
        .execute(&state.db)
        .await?;

    // ── Step 2: 检查转存限制与配额 ─────────────────
    update_task_step(&state.db, task_id, "waiting_space", "检查转存限制与存储空间").await;

    if runtime_config.transfer_max_size_gb > 0 {
        let max_size_bytes = (runtime_config.transfer_max_size_gb as i64)
            .saturating_mul(1024)
            .saturating_mul(1024)
            .saturating_mul(1024);
        if share_info.total_size > max_size_bytes {
            return Err(anyhow::anyhow!(
                "分享文件总大小超出单次转存上限：{} bytes > {} bytes（{} GB）",
                share_info.total_size,
                max_size_bytes,
                runtime_config.transfer_max_size_gb
            ));
        }
    }

    if runtime_config.transfer_max_file_count > 0 {
        let max_file_count = runtime_config.transfer_max_file_count as usize;
        if share_info.file_count > max_file_count {
            return Err(anyhow::anyhow!(
                "分享文件数量超出单次转存上限：{} > {}",
                share_info.file_count,
                max_file_count
            ));
        }
    }

    let quota = adapter.get_quota().await?;
    let min_free_bytes = runtime_config.transfer_min_free_space_gb as i64 * 1024 * 1024 * 1024;
    if quota.free < share_info.total_size + min_free_bytes {
        // 剩余空间不足（要求保留配置指定的最小剩余空间）
        return Err(anyhow::anyhow!(
            "存储空间不足：剩余 {} bytes，需要转存 {} bytes 并保留 {} bytes 余量",
            quota.free,
            share_info.total_size,
            min_free_bytes
        ));
    }

    // ── Step 3: 转存文件 ─────────────────────────
    update_task_step(&state.db, task_id, "transferring", "转存文件到云盘").await;

    let file_ids: Vec<&str> = share_info
        .tree
        .iter()
        .filter_map(|f| f.file_id.as_deref())
        .collect();

    if file_ids.is_empty() {
        return Err(anyhow::anyhow!(
            "分享解析成功但没有可转存的文件/文件夹 ID，请检查 115 API 返回结构"
        ));
    }

    let task_temp_folder_name = format!("hidden-task-{}", task_id);
    let task_temp_folder = adapter
        .create_folder(
            &runtime_config.account_115_temp_folder_id,
            &task_temp_folder_name,
        )
        .await
        .map_err(|e| anyhow::anyhow!("创建任务临时目录失败：{}", e))?;

    let transfer_ok = adapter
        .transfer_share(
            &share_id,
            source_pick_code.as_deref(),
            &file_ids,
            &task_temp_folder,
        )
        .await?;

    if !transfer_ok {
        return Err(anyhow::anyhow!("文件转存失败"));
    }

    info!("✅ 任务 #{} 转存完成，临时目录：{}", task_id, task_temp_folder);

    // ── Step 4: 整理（去广告 + TMDB 匹配 + 重命名 + 移动）────
    update_task_step(&state.db, task_id, "organizing", "整理文件结构").await;

    // 列出转存后任务专属临时目录中的真实文件（获取云盘实际 ID）
    let temp_files = adapter
        .list_files(&task_temp_folder)
        .await
        .map_err(|e| anyhow::anyhow!("列举任务临时目录失败：{}", e))?;

    // 从文件名推断标题和年份（取第一个视频文件）
    let inferred = temp_files
        .iter()
        .filter(|f| !f.is_dir && crate::utils::is_video_file(&f.name))
        .map(|f| parse_file_name(&f.name))
        .find(|p| !p.title.is_empty());

    // TMDB 匹配
    let tmdb_result = if let Some(ref parsed) = inferred {
        match TmdbClient::new(&runtime_config) {
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
    let organize_results = Organizer::new(&adapter, &runtime_config)
        .organize(&temp_files, tmdb_result.as_ref())
        .await
        .unwrap_or_else(|e| {
            warn!("整理步骤出错（非致命，继续执行）：{:?}", e);
            vec![]
        });

    // 将资源元数据写入 resources 表（若 TMDB 匹配到）
    let resource_id: Option<i64> = if let Some(ref tmdb) = tmdb_result {
        match sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO resources
                (title, original_title, year, resource_type,
                 tmdb_id, overview, poster_url, backdrop_url, status)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active')
            RETURNING id
            "#,
        )
        .bind(tmdb.title())
        .bind(tmdb.original_title())
        .bind(tmdb.year())
        .bind(tmdb.media_type())
        .bind(tmdb.tmdb_id())
        .bind(tmdb.overview())
        .bind(tmdb.poster_url())
        .bind(tmdb.backdrop_url())
        .fetch_one(&state.db)
        .await
        {
            Ok(rid) => {
                info!("📝 资源 #{} 已写入数据库：{}", rid, tmdb.title());
                // 关联整理后的文件
                for r in &organize_results {
                    let _ = sqlx::query(
                        r#"
                        INSERT INTO resource_files
                            (resource_id, file_name, cloud_file_id)
                        VALUES ($1, $2, $3)
                        "#,
                    )
                    .bind(rid)
                    .bind(&r.final_name)
                    .bind(&r.file_id)
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
        // 整理失败或源分享顶层为文件夹时，退回任务临时目录中的真实云盘条目 ID。
        // 这里不能使用源分享文件 ID；同时不能过滤目录，否则文件夹型分享会完成但不生成新分享。
        temp_files.iter().filter_map(|f| f.file_id.clone()).collect()
    };

    if !share_file_ids.is_empty() {
        // 检查分享限速
        let allowed = check_share_rate(
            &state.redis,
            runtime_config.share_max_create_per_minute,
            runtime_config.share_max_create_per_hour,
            runtime_config.share_max_create_per_day,
        )
        .await
        .unwrap_or(true);

        if !allowed {
            warn!("任务 #{} 创建分享被限速，延迟后重试", task_id);
            let wait = runtime_config.share_min_interval_secs * 1000
                + rand::random::<u64>() % (runtime_config.share_random_jitter_secs * 1000 + 1);
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

                sqlx::query(
                    r#"
                    INSERT INTO shares
                        (resource_id, share_url, pick_code, share_code, share_title,
                         share_type, file_count, total_size, status)
                    VALUES ($1, $2, $3, $4, $5, 'folder',
                            $6, $7, 'active')
                    "#,
                )
                .bind(resource_id)
                .bind(&share_result.share_url)
                .bind(&share_result.pick_code)
                .bind(&share_result.share_id)
                .bind(&share_title)
                .bind(share_file_ids.len() as i32)
                .bind(share_info.total_size)
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
    sqlx::query(
        "UPDATE import_tasks SET status = 'completed', current_step = '完成' WHERE id = $1",
    )
    .bind(task_id)
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
