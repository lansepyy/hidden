use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use rand::Rng;
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::config::Config;

// ─────────────────────────────────────────────
// 数据结构
// ─────────────────────────────────────────────

/// 分享信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareInfo {
    pub share_id: String,
    pub total_size: i64,
    pub file_count: usize,
    pub tree: Vec<FileEntry>,
}

/// 文件条目（用于解包结果）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub size: i64,
    pub path: String,
    pub is_dir: bool,
    pub file_id: Option<String>,
    pub pick_code: Option<String>,
}

/// 账号配额信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaInfo {
    pub total: i64,
    pub used: i64,
    pub free: i64,
}

/// 创建分享结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareResult {
    pub share_url: String,
    pub pick_code: String,
    pub share_id: String,
}

// ─────────────────────────────────────────────
// 115 账号适配器
// ─────────────────────────────────────────────

/// 115 云盘 API 适配器
///
/// 封装所有与 115 交互的逻辑：
/// - Cookie/会话管理
/// - 请求限速（防风控）
/// - 分享解析与转存
/// - 文件管理
/// - 分享链接创建
pub struct Adapter115 {
    client: Client,
    cookie: String,
    config: Arc<Config>,
    /// 上次请求时间（用于限速）
    last_request: Arc<Mutex<Instant>>,
}

impl Adapter115 {
    const BASE_URL: &'static str = "https://115.com";

    /// 构建适配器实例
    pub fn new(cookie: &str, config: Arc<Config>) -> anyhow::Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            ),
        );
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json, text/javascript, */*; q=0.01"),
        );
        headers.insert(
            header::HeaderName::from_static("cookie"),
            header::HeaderValue::from_str(cookie)
                .context("Cookie 格式无效")?,
        );

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .gzip(true)
            .build()?;

        Ok(Self {
            client,
            cookie: cookie.to_string(),
            config,
            last_request: Arc::new(Mutex::new(
                Instant::now() - Duration::from_secs(100),
            )),
        })
    }

    // ─────────────────────────────────────────────
    // 内部工具方法
    // ─────────────────────────────────────────────

    /// 请求限速：确保两次请求间隔不低于配置值，并添加随机抖动
    async fn rate_limit(&self) {
        let interval_ms = self.config.account_115_request_interval_ms;
        let jitter_ms = rand::thread_rng().gen_range(0..500u64);
        let target_interval = Duration::from_millis(interval_ms + jitter_ms);

        let mut last = self.last_request.lock().await;
        let elapsed = last.elapsed();
        if elapsed < target_interval {
            let wait = target_interval - elapsed;
            debug!("限速等待 {}ms", wait.as_millis());
            sleep(wait).await;
        }
        *last = Instant::now();
    }

    /// 带重试的 GET 请求
    async fn get_with_retry(
        &self,
        url: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<serde_json::Value> {
        let max_retries = self.config.account_115_retry_times;
        let mut attempt = 0u32;

        loop {
            self.rate_limit().await;

            let result = self
                .client
                .get(url)
                .query(params)
                .send()
                .await
                .context("HTTP 请求失败")?
                .json::<serde_json::Value>()
                .await
                .context("解析响应 JSON 失败");

            match result {
                Ok(v) => return Ok(v),
                Err(e) if attempt < max_retries => {
                    attempt += 1;
                    let backoff = Duration::from_secs(2u64.pow(attempt));
                    warn!("请求失败（第 {}/{} 次重试）：{:?}，等待 {}s", attempt, max_retries, e, backoff.as_secs());
                    sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// 带重试的 POST 请求
    async fn post_with_retry(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> anyhow::Result<serde_json::Value> {
        let max_retries = self.config.account_115_retry_times;
        let mut attempt = 0u32;

        loop {
            self.rate_limit().await;

            let result = self
                .client
                .post(url)
                .form(form)
                .send()
                .await
                .context("HTTP POST 请求失败")?
                .json::<serde_json::Value>()
                .await
                .context("解析 POST 响应 JSON 失败");

            match result {
                Ok(v) => return Ok(v),
                Err(_e) if attempt < max_retries => {
                    attempt += 1;
                    let backoff = Duration::from_secs(2u64.pow(attempt));
                    warn!("POST 失败（第 {}/{} 次重试）：等待 {}s", attempt, max_retries, backoff.as_secs());
                    sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    // ─────────────────────────────────────────────
    // 会话管理
    // ─────────────────────────────────────────────

    /// 检查 Cookie/会话是否有效
    pub async fn check_session(&self) -> anyhow::Result<bool> {
        let url = format!("{}/api/user/space", Self::BASE_URL);
        match self.get_with_retry(&url, &[]).await {
            Ok(v) if v["state"].as_bool().unwrap_or(false) => {
                info!("✅ 115 会话有效");
                Ok(true)
            }
            Ok(v) => {
                warn!("115 会话无效：{}", v["msg"].as_str().unwrap_or("unknown"));
                Ok(false)
            }
            Err(e) => {
                error!("会话检查失败：{:?}", e);
                Ok(false)
            }
        }
    }

    // ─────────────────────────────────────────────
    // 账号信息
    // ─────────────────────────────────────────────

    /// 获取账号存储配额
    pub async fn get_quota(&self) -> anyhow::Result<QuotaInfo> {
        let url = format!("{}/api/user/space", Self::BASE_URL);
        let resp = self.get_with_retry(&url, &[]).await?;

        if !resp["state"].as_bool().unwrap_or(false) {
            bail!("获取配额失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
        }

        let data = &resp["data"];
        let total: i64 = data["all_total"]["size"].as_i64().unwrap_or(0);
        let used: i64 = data["all_use"]["size"].as_i64().unwrap_or(0);

        Ok(QuotaInfo {
            total,
            used,
            free: total - used,
        })
    }

    // ─────────────────────────────────────────────
    // 分享链接解析
    // ─────────────────────────────────────────────

    /// 解析 115 分享链接，返回完整的文件树
    pub async fn parse_share(
        &self,
        share_id: &str,
        pick_code: Option<&str>,
    ) -> anyhow::Result<ShareInfo> {
        let url = format!("{}/api/share/get", Self::BASE_URL);
        let mut params = vec![("share_id", share_id)];
        let code;
        if let Some(pc) = pick_code {
            code = pc.to_string();
            params.push(("receive_code", &code));
        }

        let resp = self.get_with_retry(&url, &params).await?;

        if !resp["state"].as_bool().unwrap_or(false) {
            bail!("解析分享失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
        }

        let files = resp["data"].as_array().cloned().unwrap_or_default();
        let tree: Vec<FileEntry> = files
            .iter()
            .map(|f| FileEntry {
                name: f["file_name"].as_str().unwrap_or("").to_string(),
                size: f["file_size"].as_i64().unwrap_or(0),
                path: f["file_name"].as_str().unwrap_or("").to_string(),
                is_dir: f["is_dir"].as_bool().unwrap_or(false),
                file_id: f["file_id"].as_str().map(|s| s.to_string()),
                pick_code: f["pick_code"].as_str().map(|s| s.to_string()),
            })
            .collect();

        let total_size: i64 = tree.iter().map(|f| f.size).sum();
        let file_count = tree.iter().filter(|f| !f.is_dir).count();

        info!("📦 解析分享 {} 完成：{} 个文件，共 {} bytes", share_id, file_count, total_size);

        Ok(ShareInfo {
            share_id: share_id.to_string(),
            total_size,
            file_count,
            tree,
        })
    }

    // ─────────────────────────────────────────────
    // 文件操作
    // ─────────────────────────────────────────────

    /// 将分享文件转存到指定目录
    pub async fn transfer_share(
        &self,
        share_id: &str,
        pick_code: Option<&str>,
        file_ids: &[&str],
        target_folder_id: &str,
    ) -> anyhow::Result<bool> {
        let url = format!("{}/api/share/transfer", Self::BASE_URL);
        let ids = file_ids.join(",");
        let mut form = vec![
            ("share_id", share_id),
            ("file_ids", &ids),
            ("target_id", target_folder_id),
        ];
        let code;
        if let Some(pc) = pick_code {
            code = pc.to_string();
            form.push(("receive_code", &code));
        }

        // 将 form 中的 &str 引用转为 owned String 以延长生命周期
        let owned: Vec<(String, String)> = form
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;

        if resp["state"].as_bool().unwrap_or(false) {
            info!("✅ 转存完成：{} 个文件 → {}", file_ids.len(), target_folder_id);
            Ok(true)
        } else {
            let msg = resp["msg"].as_str().unwrap_or("unknown").to_string();
            error!("转存失败：{}", msg);
            Ok(false)
        }
    }

    /// 新建文件夹，返回新文件夹 ID
    pub async fn create_folder(
        &self,
        parent_id: &str,
        name: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{}/api/directory/create", Self::BASE_URL);
        let form = [("parent_id", parent_id), ("name", name)];
        let borrowed: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, *v)).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;

        if !resp["state"].as_bool().unwrap_or(false) {
            bail!("创建文件夹失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
        }

        let folder_id = resp["data"]["id"]
            .as_str()
            .unwrap_or("")
            .to_string();

        info!("📁 创建文件夹 '{}' → ID: {}", name, folder_id);
        Ok(folder_id)
    }

    /// 移动文件
    pub async fn move_files(
        &self,
        file_ids: &[&str],
        target_folder_id: &str,
    ) -> anyhow::Result<bool> {
        let url = format!("{}/api/move", Self::BASE_URL);
        let ids = file_ids.join(",");
        let form = [("file_ids", ids.as_str()), ("folder_id", target_folder_id)];
        let borrowed: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, *v)).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        Ok(resp["state"].as_bool().unwrap_or(false))
    }

    /// 重命名文件/目录
    pub async fn rename_file(
        &self,
        file_id: &str,
        new_name: &str,
    ) -> anyhow::Result<bool> {
        let url = format!("{}/api/file/rename", Self::BASE_URL);
        let form = [("fid", file_id), ("file_name", new_name)];
        let borrowed: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, *v)).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;

        if resp["state"].as_bool().unwrap_or(false) {
            info!("✏️  重命名 {} → {}", file_id, new_name);
            Ok(true)
        } else {
            warn!("重命名失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
            Ok(false)
        }
    }

    /// 删除文件/目录（移入回收站）
    pub async fn delete_files(&self, file_ids: &[&str]) -> anyhow::Result<bool> {
        let url = format!("{}/api/delete", Self::BASE_URL);
        let ids = file_ids.join(",");
        let form = [("file_ids", ids.as_str())];
        let borrowed: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, *v)).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        Ok(resp["state"].as_bool().unwrap_or(false))
    }

    /// 列出指定目录下的所有文件（递归一层，不深递归）
    pub async fn list_files(&self, folder_id: &str) -> anyhow::Result<Vec<FileEntry>> {
        let url = format!("{}/api/files", Self::BASE_URL);
        // cid=目录ID, show_dir=1 包含子目录, limit=1150 最大单页
        let params = [
            ("cid", folder_id),
            ("show_dir", "1"),
            ("limit", "1150"),
            ("offset", "0"),
        ];

        let resp = self.get_with_retry(&url, &params).await?;

        // 115 文件列表接口有时通过 state 字段，有时不带
        let data_arr = resp["data"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| resp["list"].as_array().cloned().unwrap_or_default());

        let entries: Vec<FileEntry> = data_arr
            .iter()
            .map(|f| FileEntry {
                name: f["file_name"]
                    .as_str()
                    .or_else(|| f["name"].as_str())
                    .unwrap_or("")
                    .to_string(),
                size: f["file_size"].as_i64().unwrap_or(0),
                path: f["file_name"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                // is_dir: 1 表示目录
                is_dir: f["is_dir"].as_i64().unwrap_or(0) == 1
                    || f["is_dir"].as_bool().unwrap_or(false),
                file_id: f["fid"]
                    .as_str()
                    .or_else(|| f["file_id"].as_str())
                    .map(|s| s.to_string()),
                pick_code: f["pc"]
                    .as_str()
                    .or_else(|| f["pick_code"].as_str())
                    .map(|s| s.to_string()),
            })
            .collect();

        info!(
            "📂 列举目录 {} 完成：{} 个条目",
            folder_id,
            entries.len()
        );
        Ok(entries)
    }

    // ─────────────────────────────────────────────
    // 分享链接管理
    // ─────────────────────────────────────────────

    /// 为指定文件/目录创建分享链接
    pub async fn create_share(
        &self,
        file_ids: &[&str],
        title: Option<&str>,
        duration_days: u32,
    ) -> anyhow::Result<ShareResult> {
        let url = format!("{}/api/share/send", Self::BASE_URL);
        let ids = file_ids.join(",");
        let duration_str = duration_days.to_string();
        let mut form: Vec<(&str, &str)> = vec![
            ("file_ids", &ids),
            ("expire", &duration_str),
        ];
        let t;
        if let Some(title_str) = title {
            t = title_str.to_string();
            form.push(("title", &t));
        }

        let owned: Vec<(String, String)> = form
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;

        if !resp["state"].as_bool().unwrap_or(false) {
            bail!("创建分享失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
        }

        let data = &resp["data"];
        let link_id = data["link_id"].as_str().unwrap_or("").to_string();
        let pick = data["code"].as_str().unwrap_or("").to_string();

        info!("🔗 创建分享链接 → 115.com/s/{}", link_id);

        Ok(ShareResult {
            share_url: format!("https://115.com/s/{}", link_id),
            pick_code: pick,
            share_id: link_id,
        })
    }

    /// 验证分享链接是否可访问
    pub async fn verify_share(
        &self,
        share_id: &str,
        pick_code: Option<&str>,
    ) -> anyhow::Result<bool> {
        match self.parse_share(share_id, pick_code).await {
            Ok(_) => Ok(true),
            Err(e) => {
                warn!("分享链接验证失败 {}：{:?}", share_id, e);
                Ok(false)
            }
        }
    }
}
