use std::time::Duration;

use anyhow::Result;
use tracing::{error, info, warn};

use crate::{
    adapters::{Adapter115, FileEntry},
    services::{check_share_rate, record_share_created, Organizer, TmdbClient},
    utils::{is_subtitle_file, is_video_file, parse_file_name},
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
    // 配额 API 失败时返回全 0，跳过空间检查避免误抦任务
    if quota.total > 0 {
        let min_free_bytes = runtime_config.transfer_min_free_space_gb as i64 * 1024 * 1024 * 1024;
        if quota.free < share_info.total_size + min_free_bytes {
            return Err(anyhow::anyhow!(
                "存储空间不足：剩余 {} bytes，需要转存 {} bytes 并保留 {} bytes 余量",
                quota.free,
                share_info.total_size,
                min_free_bytes
            ));
        }
    } else {
        tracing::warn!("配额 API 返回全 0，跳过空间检查继续执行");
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

    // 列出转存后任务专属临时目录中的真实条目（用于兜底分享文件夹）
    let temp_files = adapter
        .list_files(&task_temp_folder)
        .await
        .map_err(|e| anyhow::anyhow!("列举任务临时目录失败：{}", e))?;

    // 递归展开转存后的目录。115 分享经常是“外层文件夹 + 内部媒体文件”，
    // 只列临时目录第一层会导致后续整理/入库看不到真正的视频文件。
    let mut media_files = Vec::new();
    for entry in &temp_files {
        collect_files_recursive(&adapter, entry, "", &mut media_files).await?;
    }

    if media_files.is_empty() {
        warn!(
            "任务 #{} 转存完成但递归未发现文件，后续将仅保留顶层条目用于分享兜底",
            task_id
        );
    }

    // 从文件名推断标题和年份（取第一个视频文件）
    let inferred = media_files
        .iter()
        .filter(|f| !f.is_dir && is_video_file(&f.name))
        .map(|f| parse_file_name(&f.name))
        .find(|p| !p.title.is_empty());

    // TMDB 匹配
    // 有季/集信息时强制按电视剧搜索，避免同名电影热度更高导致剧集被整理到电影目录。
    // 无季/集信息时继续使用电影/电视剧智能匹配。
    let tmdb_result = if let Some(ref parsed) = inferred {
        match TmdbClient::new(&runtime_config) {
            Ok(client) => {
                let r = if parsed.season.is_some() || parsed.episode.is_some() {
                    match client.search_tv(&parsed.title, parsed.year).await {
                        Ok(mut shows) => shows
                            .drain(..)
                            .next()
                            .map(crate::services::tmdb::TmdbResult::Tv),
                        Err(e) => {
                            warn!("TMDB 剧集搜索失败（跳过匹配）：{:?}", e);
                            None
                        }
                    }
                } else {
                    client.smart_search(&parsed.title, parsed.year).await
                };

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
        .organize(&media_files, tmdb_result.as_ref(), inferred.as_ref())
        .await
        .unwrap_or_else(|e| {
            warn!("整理步骤出错（非致命，继续执行）：{:?}", e);
            vec![]
        });

    // 无论 TMDB 是否匹配成功，都写入 resources / resource_files。
    // 否则只要 TMDB 未配置、匹配失败，任务就会“转存完成但资源库无展示”。
    let resource_id = upsert_resource_and_files(
        &state.db,
        tmdb_result.as_ref(),
        inferred.as_ref(),
        &media_files,
        &organize_results,
    )
    .await;

    // ── Step 5: 创建新分享 ───────────────────────
    update_task_step(&state.db, task_id, "sharing", "创建分享链接").await;

    let share_file_ids = build_share_file_ids(&organize_results, &temp_files);

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
        let share_title = build_display_title(tmdb_result.as_ref(), inferred.as_ref())
            .unwrap_or_else(|| format!("Hidden 导入任务 #{}", task_id));

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

                if organize_results.is_empty() {
                    warn!(
                        "任务 #{} 整理结果为空，分享目标仍可能位于临时目录，跳过源文件清理",
                        task_id
                    );
                } else {
                    cleanup_task_temp_folder_after_share(
                        &adapter,
                        &runtime_config.account_115_temp_folder_id,
                        &task_temp_folder,
                        &task_temp_folder_name,
                        task_id,
                    )
                    .await;
                }
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

/// 展开 115 目录，把所有文件加入 `out`。
///
/// `Adapter115::list_files` 只返回单层；转存分享目录时，真正的视频经常在下一层或更深层。
/// 这里使用显式栈迭代，避免递归 async fn 产生无限大小 Future。
async fn collect_files_recursive(
    adapter: &Adapter115,
    entry: &FileEntry,
    parent_path: &str,
    out: &mut Vec<FileEntry>,
) -> Result<()> {
    let mut stack: Vec<(FileEntry, String)> = vec![(entry.clone(), parent_path.to_string())];

    while let Some((current, parent)) = stack.pop() {
        let current_path = if parent.is_empty() {
            current.name.clone()
        } else {
            format!("{}/{}", parent, current.name)
        };

        if !current.is_dir {
            let mut file = current;
            file.path = current_path;
            out.push(file);
            continue;
        }

        let Some(folder_id) = current.file_id.as_deref() else {
            warn!("目录 {} 缺少 cid，无法展开", current_path);
            continue;
        };

        let children = adapter
            .list_files(folder_id)
            .await
            .map_err(|e| anyhow::anyhow!("列举目录 {} 失败：{}", current_path, e))?;

        // 反向入栈以尽量保持与 115 返回一致的遍历顺序。
        for child in children.into_iter().rev() {
            stack.push((child, current_path.clone()));
        }
    }

    Ok(())
}

/// 构建用于创建分享的条目 ID。
///
/// 优先分享整理后的“资源目录 + 文件”组合：
/// - 资源目录用于让分享接收方看到完整文件夹结构；
/// - 文件 ID 作为兜底，避免某些 115 分享接口只分享目录时漏文件。
/// 若整理失败，则回退到任务临时目录第一层的文件/目录 ID。
fn build_share_file_ids(
    organize_results: &[crate::services::organizer::OrganizerResult],
    temp_files: &[FileEntry],
) -> Vec<String> {
    let mut ids = Vec::<String>::new();

    if organize_results.is_empty() {
        for entry in temp_files {
            if let Some(id) = entry.file_id.as_deref() {
                push_unique_id(&mut ids, id);
            }
        }
        return ids;
    }

    for result in organize_results {
        if !result.folder_id.is_empty() && result.folder_id != "0" {
            push_unique_id(&mut ids, &result.folder_id);
        }
    }

    for result in organize_results {
        push_unique_id(&mut ids, &result.file_id);
    }

    ids
}

fn build_display_title(
    tmdb: Option<&crate::services::tmdb::TmdbResult>,
    inferred: Option<&crate::utils::ParsedFileName>,
) -> Option<String> {
    let title = preferred_resource_title(tmdb, inferred)?;
    let year = tmdb.and_then(|t| t.year()).or_else(|| inferred.and_then(|p| p.year));
    let year_str = year.map(|y| format!(" ({})", y)).unwrap_or_default();
    Some(format!("{}{}", title, year_str))
}

fn preferred_resource_title(
    tmdb: Option<&crate::services::tmdb::TmdbResult>,
    inferred: Option<&crate::utils::ParsedFileName>,
) -> Option<String> {
    let inferred_title = inferred
        .map(|p| p.title.trim())
        .filter(|title| !title.is_empty());

    let tmdb_title = tmdb.map(|t| t.title().trim()).filter(|title| !title.is_empty());

    match (tmdb_title, inferred_title) {
        (Some(t), Some(local)) if contains_cjk(local) && !contains_cjk(t) => Some(local.to_string()),
        (Some(t), _) => Some(t.to_string()),
        (None, Some(local)) => Some(local.to_string()),
        (None, None) => None,
    }
}

fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{4e00}'..='\u{9fff}').contains(&c)
            || ('\u{3400}'..='\u{4dbf}').contains(&c)
            || ('\u{f900}'..='\u{faff}').contains(&c)
    })
}

fn push_unique_id(ids: &mut Vec<String>, id: &str) {
    if id.trim().is_empty() {
        return;
    }
    if !ids.iter().any(|existing| existing == id) {
        ids.push(id.to_string());
    }
}

/// 分享创建成功后清理任务专属临时目录。
///
/// 安全措施：
/// 1. 只删除 `ACCOUNT_115_TEMP_FOLDER_ID` 下本任务创建的 `hidden-task-{id}` 目录；
/// 2. 临时根目录为空/根目录时拒绝删除；
/// 3. 删除前重新列出临时根目录确认目录 ID 和目录名均匹配；
/// 4. 只删除任务目录本身，不直接按媒体文件 ID 删除，避免误删成品目录内容。
async fn cleanup_task_temp_folder_after_share(
    adapter: &Adapter115,
    configured_temp_root: &str,
    task_temp_folder: &str,
    task_temp_folder_name: &str,
    task_id: i64,
) {
    if configured_temp_root.trim().is_empty() || configured_temp_root == "0" {
        warn!(
            "任务 #{} 跳过源文件清理：未配置安全的临时目录 ACCOUNT_115_TEMP_FOLDER_ID",
            task_id
        );
        return;
    }

    if task_temp_folder.trim().is_empty()
        || task_temp_folder == "0"
        || task_temp_folder == configured_temp_root
        || !task_temp_folder_name.starts_with("hidden-task-")
    {
        warn!(
            "任务 #{} 跳过源文件清理：临时目录参数异常 root={} folder={} name={}",
            task_id, configured_temp_root, task_temp_folder, task_temp_folder_name
        );
        return;
    }

    let entries = match adapter.list_files(configured_temp_root).await {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "任务 #{} 跳过源文件清理：无法列出配置临时目录 {}：{:?}",
                task_id, configured_temp_root, e
            );
            return;
        }
    };

    let confirmed = entries.iter().any(|entry| {
        entry.is_dir
            && entry.name == task_temp_folder_name
            && entry.file_id.as_deref() == Some(task_temp_folder)
    });

    if !confirmed {
        warn!(
            "任务 #{} 跳过源文件清理：{} 不在配置临时目录 {} 下，避免误删",
            task_id, task_temp_folder_name, configured_temp_root
        );
        return;
    }

    match adapter.delete_files(&[task_temp_folder]).await {
        Ok(_) => info!(
            "🧹 任务 #{} 分享后已安全删除源临时目录：{} ({})",
            task_id, task_temp_folder_name, task_temp_folder
        ),
        Err(e) => warn!(
            "任务 #{} 分享后删除源临时目录失败（分享已创建）：{:?}",
            task_id, e
        ),
    }
}

/// 写入资源与文件索引，保证前端资源库能展示导入结果。
async fn upsert_resource_and_files(
    db: &sqlx::PgPool,
    tmdb: Option<&crate::services::tmdb::TmdbResult>,
    inferred: Option<&crate::utils::ParsedFileName>,
    media_files: &[FileEntry],
    organize_results: &[crate::services::organizer::OrganizerResult],
) -> Option<i64> {
    let title = preferred_resource_title(tmdb, inferred).unwrap_or_else(|| "未识别资源".to_string());
    let original_title = tmdb.map(|t| t.original_title());
    let year = tmdb.and_then(|t| t.year()).or_else(|| inferred.and_then(|p| p.year));
    let resource_type = tmdb
        .map(|t| t.media_type())
        .or_else(|| {
            media_files
                .iter()
                .any(|f| parse_file_name(&f.name).episode.is_some())
                .then_some("tv")
        })
        .unwrap_or("other");
    let tmdb_id = tmdb.map(|t| t.tmdb_id());
    let overview = tmdb.and_then(|t| t.overview());
    let poster_url = tmdb.and_then(|t| t.poster_url());
    let backdrop_url = tmdb.and_then(|t| t.backdrop_url());

    let resource_id = match sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO resources
            (title, original_title, year, resource_type,
             tmdb_id, overview, poster_url, backdrop_url, status)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active')
        RETURNING id
        "#,
    )
    .bind(&title)
    .bind(original_title)
    .bind(year)
    .bind(resource_type)
    .bind(tmdb_id)
    .bind(overview)
    .bind(poster_url)
    .bind(backdrop_url)
    .fetch_one(db)
    .await
    {
        Ok(rid) => rid,
        Err(e) => {
            warn!("资源入库失败（非致命）：{:?}", e);
            return None;
        }
    };

    info!("📝 资源 #{} 已写入数据库：{}", resource_id, title);

    if organize_results.is_empty() {
        for file in media_files.iter().filter(|f| !f.is_dir) {
            insert_resource_file(db, resource_id, file, None).await;
        }
    } else {
        for result in organize_results {
            let fallback_file;
            let file = match media_files
                .iter()
                .find(|f| f.file_id.as_deref() == Some(result.file_id.as_str()))
            {
                Some(source) => source,
                None => {
                    fallback_file = FileEntry {
                        name: result.final_name.clone(),
                        size: 0,
                        path: result.final_name.clone(),
                        is_dir: false,
                        file_id: Some(result.file_id.clone()),
                        pick_code: None,
                    };
                    &fallback_file
                }
            };

            insert_resource_file(db, resource_id, file, Some(result)).await;
        }
    }

    Some(resource_id)
}

/// 写入单个资源文件，包含解析出的季集、画质、扩展名等字段。
async fn insert_resource_file(
    db: &sqlx::PgPool,
    resource_id: i64,
    file: &FileEntry,
    organized: Option<&crate::services::organizer::OrganizerResult>,
) {
    let final_name = organized
        .map(|r| r.final_name.as_str())
        .unwrap_or(file.name.as_str());
    let parsed = parse_file_name(final_name);
    let media_type = if is_video_file(final_name) {
        Some("video")
    } else if is_subtitle_file(final_name) {
        Some("subtitle")
    } else {
        Some("other")
    };
    let cloud_file_id = organized
        .map(|r| r.file_id.as_str())
        .or(file.file_id.as_deref());
    let file_path = organized
        .map(|r| format!("{}/{}", r.folder_id, final_name))
        .unwrap_or_else(|| file.path.clone());

    let _ = sqlx::query(
        r#"
        INSERT INTO resource_files
            (resource_id, file_name, file_path, file_size, file_ext,
             media_type, season, episode, quality, cloud_file_id, pick_code)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        "#,
    )
    .bind(resource_id)
    .bind(final_name)
    .bind(file_path)
    .bind(file.size)
    .bind(parsed.ext)
    .bind(media_type)
    .bind(parsed.season)
    .bind(parsed.episode)
    .bind(parsed.quality)
    .bind(cloud_file_id)
    .bind(file.pick_code.as_deref())
    .execute(db)
    .await;
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
