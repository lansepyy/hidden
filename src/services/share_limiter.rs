use anyhow::Result;
use chrono::Utc;
use tracing::warn;

/// 检查当前分钟/小时/天是否已达到分享创建上限
///
/// 返回 `true` 表示允许创建，`false` 表示已超限
pub async fn check_share_rate(
    redis: &redis::Client,
    max_per_minute: u32,
    max_per_hour: u32,
    max_per_day: u32,
) -> Result<bool> {
    let mut conn = redis.get_async_connection().await?;
    let now = Utc::now();

    let min_key = format!("hidden:share:min:{}", now.format("%Y%m%d%H%M"));
    let hour_key = format!("hidden:share:hour:{}", now.format("%Y%m%d%H"));
    let day_key = format!("hidden:share:day:{}", now.format("%Y%m%d"));

    let min_count: i64 = redis::cmd("GET")
        .arg(&min_key)
        .query_async(&mut conn)
        .await
        .unwrap_or(0i64);

    let hour_count: i64 = redis::cmd("GET")
        .arg(&hour_key)
        .query_async(&mut conn)
        .await
        .unwrap_or(0i64);

    let day_count: i64 = redis::cmd("GET")
        .arg(&day_key)
        .query_async(&mut conn)
        .await
        .unwrap_or(0i64);

    if min_count >= max_per_minute as i64 {
        warn!(
            "⚠️  分享限速触发：本分钟已创建 {} 次（上限 {}）",
            min_count, max_per_minute
        );
        return Ok(false);
    }

    if hour_count >= max_per_hour as i64 {
        warn!(
            "⚠️  分享限速触发：本小时已创建 {} 次（上限 {}）",
            hour_count, max_per_hour
        );
        return Ok(false);
    }

    if day_count >= max_per_day as i64 {
        warn!(
            "⚠️  分享限速触发：今日已创建 {} 次（上限 {}）",
            day_count, max_per_day
        );
        return Ok(false);
    }

    Ok(true)
}

/// 记录一次分享创建，递增 Redis 计数器
pub async fn record_share_created(redis: &redis::Client) -> Result<()> {
    let mut conn = redis.get_async_connection().await?;
    let now = Utc::now();

    let min_key = format!("hidden:share:min:{}", now.format("%Y%m%d%H%M"));
    let hour_key = format!("hidden:share:hour:{}", now.format("%Y%m%d%H"));
    let day_key = format!("hidden:share:day:{}", now.format("%Y%m%d"));

    // 每个 key 分别 INCR + EXPIRE（让 key 自动到期，无需手动清理）
    for (key, ttl) in [
        (&min_key, 90i64),      // 分钟计数保留 90 秒
        (&hour_key, 3660i64),   // 小时计数保留 61 分钟
        (&day_key, 86460i64),   // 天计数保留 24 小时零 1 分钟
    ] {
        let _: i64 = redis::cmd("INCR")
            .arg(key)
            .query_async(&mut conn)
            .await?;
        let _: i64 = redis::cmd("EXPIRE")
            .arg(key)
            .arg(ttl)
            .query_async(&mut conn)
            .await?;
    }

    Ok(())
}

/// 查询当前分钟/小时/天的分享创建次数（用于监控展示）
pub async fn get_share_counts(
    redis: &redis::Client,
) -> Result<(i64, i64, i64)> {
    let mut conn = redis.get_async_connection().await?;
    let now = Utc::now();

    let min_key = format!("hidden:share:min:{}", now.format("%Y%m%d%H%M"));
    let hour_key = format!("hidden:share:hour:{}", now.format("%Y%m%d%H"));
    let day_key = format!("hidden:share:day:{}", now.format("%Y%m%d"));

    let min: i64 = redis::cmd("GET")
        .arg(&min_key)
        .query_async(&mut conn)
        .await
        .unwrap_or(0);
    let hour: i64 = redis::cmd("GET")
        .arg(&hour_key)
        .query_async(&mut conn)
        .await
        .unwrap_or(0);
    let day: i64 = redis::cmd("GET")
        .arg(&day_key)
        .query_async(&mut conn)
        .await
        .unwrap_or(0);

    Ok((min, hour, day))
}
