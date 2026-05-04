use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

/// 将 115 API 返回的字符串/数字 ID 统一转换为字符串。
///
/// 115 的部分接口会把 `fid`、`cid`、`file_id`、`pick_code` 等字段以数字返回，
/// 如果只调用 `as_str()` 会导致 ID 丢失，后续转存、移动、分享都会失败。
fn value_to_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| value.as_i64().map(|n| n.to_string()))
        .or_else(|| value.as_u64().map(|n| n.to_string()))
}

// ─────────────────────────────────────────────
// Adapter115
// ─────────────────────────────────────────────

pub struct Adapter115 {
    client: Client,
    cookie: String,
    config: Arc<Config>,
    last_request: Arc<Mutex<Instant>>,
}

impl Adapter115 {
    const WEBAPI: &'static str = "https://webapi.115.com";
    const PROAPI: &'static str = "https://proapi.115.com";

    /// 构造一个新的适配器实例（同步构造，调用方在需要时会重新构建以获取最新 Cookie）
    pub fn new(cookie: &str, config: Arc<Config>) -> anyhow::Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static("Mozilla/5.0 (compatible; hidden/1.0)"),
        );
        if !cookie.is_empty() {
            headers.insert(
                header::COOKIE,
                header::HeaderValue::from_str(cookie).unwrap_or_else(|_| header::HeaderValue::from_static("")),
            );
        }

        let client = Client::builder().default_headers(headers).build().context("创建 HTTP 客户端失败")?;

        let initial = Instant::now() - Duration::from_millis(config.account_115_request_interval_ms);

        Ok(Self {
            client,
            cookie: cookie.to_string(),
            config,
            last_request: Arc::new(Mutex::new(initial)),
        })
    }

    /// 简单的请求速率限制，保证两次请求之间至少间隔配置的毫秒数
    async fn rate_limit(&self) {
        let min_interval = Duration::from_millis(self.config.account_115_request_interval_ms);
        let mut guard = self.last_request.lock().await;
        let elapsed = guard.elapsed();
        if elapsed < min_interval {
            let wait = min_interval - elapsed;
            debug!("115 rate limit: sleeping {}ms", wait.as_millis());
            sleep(wait).await;
        }
        *guard = Instant::now();
    }

    /// 带重试的 GET 请求，返回解析后的 JSON 值
    async fn get_with_retry(&self, url: &str, params: &[(&str, &str)]) -> anyhow::Result<Value> {
        let max_retries = self.config.account_115_retry_times;
        let mut attempt: u32 = 0;

        loop {
            self.rate_limit().await;

            let req = self.client.get(url).query(params);
            let resp = req.send().await;

            match resp {
                Ok(r) => {
                    let j = r.json::<Value>().await.context("解析 GET 响应为 JSON 失败");
                    match j {
                        Ok(v) => return Ok(v),
                        Err(e) if attempt < max_retries => {
                            attempt += 1;
                            let backoff = Duration::from_secs(u64::from(1u32 << attempt.min(4)));
                            warn!("GET JSON 解析失败，重试中（{}/{}）等待 {:?}: {:?}", attempt, max_retries, backoff, e);
                            sleep(backoff).await;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) if attempt < max_retries => {
                    attempt += 1;
                    let backoff = Duration::from_secs(u64::from(1u32 << attempt.min(4)));
                    warn!("GET 请求失败，重试中（{}/{}）等待 {:?}: {:?}", attempt, max_retries, backoff, e);
                    sleep(backoff).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// 带重试的 POST 表单请求，返回解析后的 JSON
    async fn post_with_retry(&self, url: &str, form: &[(&str, &str)]) -> anyhow::Result<Value> {
        let max_retries = self.config.account_115_retry_times;
        let mut attempt: u32 = 0;

        loop {
            self.rate_limit().await;

            let resp = self.client.post(url).form(form).send().await;
            match resp {
                Ok(r) => {
                    let j = r.json::<Value>().await.context("解析 POST 响应 JSON 失败");
                    match j {
                        Ok(v) => return Ok(v),
                        Err(e) if attempt < max_retries => {
                            attempt += 1;
                            let backoff = Duration::from_secs(u64::from(1u32 << attempt.min(4)));
                            warn!("POST JSON 解析失败，重试中（{}/{}）等待 {:?}: {:?}", attempt, max_retries, backoff, e);
                            sleep(backoff).await;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) if attempt < max_retries => {
                    attempt += 1;
                    let backoff = Duration::from_secs(u64::from(1u32 << attempt.min(4)));
                    warn!("POST 请求失败，重试中（{}/{}）等待 {:?}: {:?}", attempt, max_retries, backoff, e);
                    sleep(backoff).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    // ─────────────────────────────────────────────
    // 会话与账号信息
    // ─────────────────────────────────────────────

    /// 检查 Cookie/会话是否有效
    pub async fn check_session(&self) -> anyhow::Result<bool> {
        let url = format!("{}/user/space_summury", Self::WEBAPI);
        match self.post_with_retry(&url, &[]).await {
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

    /// 获取账号存储配额
    pub async fn get_quota(&self) -> anyhow::Result<QuotaInfo> {
        let url = format!("{}/android/user/space_info", Self::PROAPI);
        let resp = self.get_with_retry(&url, &[]).await?;

        // 115 部分接口返回整数 1 作为 state，需同时兼容 bool 和 int
        let state_ok = resp["state"].as_bool()
            .unwrap_or_else(|| resp["state"].as_i64().map_or(false, |v| v != 0));
        let data = if state_ok { resp["data"].clone() } else { resp.clone() };

        let total = data["rt_space_info"]["all_total"]["size"].as_i64()
            .or_else(|| data["all_total"]["size"].as_i64())
            .unwrap_or(0);
        let used = data["rt_space_info"]["all_use"]["size"].as_i64()
            .or_else(|| data["all_use"]["size"].as_i64())
            .unwrap_or(0);
        let free = data["rt_space_info"]["all_remain"]["size"].as_i64()
            .or_else(|| data["all_remain"]["size"].as_i64())
            .unwrap_or_else(|| total.saturating_sub(used));

        info!("📊 存储配额 - 总计:{} 已用:{} 剩余:{} (state_ok={})",
            total, used, free, state_ok);
        if free == 0 && total == 0 {
            warn!("配额 API 返回值全为 0，原始响应: {}", resp);
        }

        Ok(QuotaInfo { total, used, free })
    }

    // ─────────────────────────────────────────────
    // 分享解析与管理
    // ─────────────────────────────────────────────

    /// 解析 115 分享链接，返回完整的文件树
    /// GET https://webapi.115.com/share/snap
    pub async fn parse_share(&self, share_code: &str, receive_code: Option<&str>) -> anyhow::Result<ShareInfo> {
        let url = format!("{}/share/snap", Self::WEBAPI);
        let mut all_files: Vec<FileEntry> = Vec::new();
        let mut offset: usize = 0;
        let limit: usize = 1000;

        loop {
            let limit_str = limit.to_string();
            let offset_str = offset.to_string();
            let params = [
                ("share_code", share_code),
                ("receive_code", receive_code.unwrap_or("")),
                ("cid", "0"),
                ("limit", limit_str.as_str()),
                ("offset", offset_str.as_str()),
            ];

            let resp = self.get_with_retry(&url, &params).await?;

            if !resp["state"].as_bool().unwrap_or(false) {
                let errno = resp["errno"].as_i64().unwrap_or(0);
                let msg = resp["msg"].as_str().unwrap_or("unknown");
                bail!("解析分享失败 [errno={}]：{}", errno, msg);
            }

            let data = &resp["data"];
            let files = data["list"].as_array().cloned().unwrap_or_default();
            let count = data["count"].as_u64().unwrap_or(0) as usize;

            for f in &files {
                // p115client: is_dir = "fid" not in info
                // 目录无 fid 字段，其自身 ID 在 cid；文件有 fid 且 cid 为父目录 ID
                let is_dir = f["fid"].is_null()
                    || f["ico"].as_str() == Some("folder")
                    || f["is_dir"].as_i64().unwrap_or(0) == 1;
                let file_id = if is_dir {
                    value_to_string(&f["cid"])
                        .or_else(|| value_to_string(&f["fid"]))
                } else {
                    value_to_string(&f["fid"])
                        .or_else(|| value_to_string(&f["file_id"]))
                };

                all_files.push(FileEntry {
                    name: f["n"].as_str().or_else(|| f["file_name"].as_str()).unwrap_or("").to_string(),
                    size: f["s"].as_i64().or_else(|| f["file_size"].as_i64()).unwrap_or(0),
                    path: f["n"].as_str().or_else(|| f["file_name"].as_str()).unwrap_or("").to_string(),
                    is_dir,
                    file_id,
                    pick_code: value_to_string(&f["pc"]).or_else(|| value_to_string(&f["pick_code"])),
                });
            }

            offset += files.len();
            if offset >= count || files.is_empty() { break; }
        }

        let total_size: i64 = all_files.iter().map(|f| f.size).sum();
        let file_count = all_files.iter().filter(|f| !f.is_dir).count();

        info!("📦 解析分享 {} 完成：{} 个文件，共 {} bytes", share_code, file_count, total_size);

        Ok(ShareInfo { share_id: share_code.to_string(), total_size, file_count, tree: all_files })
    }

    /// 将分享文件转存到指定目录
    pub async fn transfer_share(&self, share_code: &str, receive_code: Option<&str>, file_ids: &[&str], target_folder_id: &str) -> anyhow::Result<bool> {
        let url = format!("{}/share/receive", Self::WEBAPI);
        let ids = file_ids.join(",");
        let form = [
            ("share_code", share_code),
            ("receive_code", receive_code.unwrap_or("")),
            ("file_id", ids.as_str()),
            ("cid", target_folder_id),
        ];

        let owned: Vec<(String, String)> = form.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let borrowed: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;

        if resp["state"].as_bool().unwrap_or(false) {
            info!("✅ 转存完成：{} 个文件 → cid={}", file_ids.len(), target_folder_id);
            Ok(true)
        } else {
            let errno = resp["errno"].as_i64().unwrap_or(0);
            let msg = resp["msg"].as_str().unwrap_or("unknown").to_string();
            error!("转存失败 [errno={}]：{}", errno, msg);
            bail!("转存失败 [errno={}]：{}", errno, msg);
        }
    }

    /// 新建文件夹，返回新文件夹 ID
    pub async fn create_folder(&self, parent_id: &str, name: &str) -> anyhow::Result<String> {
        let url = format!("{}/open/folder/add", Self::PROAPI);
        let form = [("file_name", name), ("pid", parent_id)];
        let borrowed: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, *v)).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        if !resp["state"].as_bool().unwrap_or(false) {
            bail!("创建文件夹失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
        }

        let folder_id = value_to_string(&resp["data"]["file_id"])
            .or_else(|| value_to_string(&resp["data"]["cid"]))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("创建文件夹成功但响应缺少文件夹 ID：{}", resp))?;
        info!("📁 创建文件夹 '{}' → cid={}", name, folder_id);
        Ok(folder_id)
    }

    /// 移动文件
    pub async fn move_files(&self, file_ids: &[&str], target_folder_id: &str) -> anyhow::Result<bool> {
        let url = format!("{}/open/ufile/move", Self::PROAPI);
        let ids = file_ids.join(",");
        let form = [("file_ids", ids.as_str()), ("to_cid", target_folder_id)];
        let borrowed: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, *v)).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        Ok(resp["state"].as_bool().unwrap_or(false))
    }

    /// 重命名文件/目录
    pub async fn rename_file(&self, file_id: &str, new_name: &str) -> anyhow::Result<bool> {
        let url = format!("{}/open/ufile/update", Self::PROAPI);
        let form = [("file_id", file_id), ("file_name", new_name)];
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
        let url = format!("{}/rb/delete", Self::WEBAPI);

        let owned: Vec<(String, String)> = if file_ids.len() == 1 {
            vec![("fid".to_string(), file_ids[0].to_string())]
        } else {
            file_ids.iter().enumerate().map(|(i, id)| (format!("fid[{}]", i), id.to_string())).collect()
        };
        let borrowed: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        Ok(resp["state"].as_bool().unwrap_or(false))
    }

    /// 列出指定目录下的所有文件（不深递归，webapi 115 兼容性最佳）
    pub async fn list_files(&self, cid: &str) -> anyhow::Result<Vec<FileEntry>> {
        let url = format!("{}/files", Self::WEBAPI);
        let params = [
            ("cid", cid),
            ("show_dir", "1"),
            ("limit", "1150"),
            ("offset", "0"),
            ("aid", "1"),
            ("count_folders", "1"),
            ("record_open_time", "1"),
        ];

        let resp = self.get_with_retry(&url, &params).await?;
        let data_arr = resp["data"].as_array().cloned().unwrap_or_default();

        let entries: Vec<FileEntry> = data_arr
            .iter()
            .map(|f| {
                // p115client: is_dir = "fid" not in info
                // 目录无 fid 字段，其自身 ID 在 cid；文件有 fid 且 cid 为父目录 ID
                let is_dir = f["fid"].is_null()
                    || f["ico"].as_str() == Some("folder")
                    || f["is_dir"].as_i64().unwrap_or(0) == 1;
                let file_id = if is_dir {
                    value_to_string(&f["cid"])
                        .or_else(|| value_to_string(&f["fid"]))
                } else {
                    value_to_string(&f["fid"])
                        .or_else(|| value_to_string(&f["file_id"]))
                };

                FileEntry {
                    name: f["n"]
                        .as_str()
                        .or_else(|| f["file_name"].as_str())
                        .unwrap_or("")
                        .to_string(),
                    size: f["s"].as_i64().or_else(|| f["file_size"].as_i64()).unwrap_or(0),
                    path: f["n"]
                        .as_str()
                        .or_else(|| f["file_name"].as_str())
                        .unwrap_or("")
                        .to_string(),
                    is_dir,
                    file_id,
                    pick_code: value_to_string(&f["pc"])
                        .or_else(|| value_to_string(&f["pick_code"])),
                }
            })
            .collect();

        info!("📂 列举目录 {} 完成：{} 个条目", cid, entries.len());
        Ok(entries)
    }

    /// 通过 CID 获取文件夹名称
    /// 利用 /files?cid=xxx 响应中的 `path` 数组（面包屑），取最后一项即为当前目录名
    pub async fn get_folder_name(&self, cid: &str) -> anyhow::Result<String> {
        if cid == "0" || cid.is_empty() {
            return Ok("根目录".to_string());
        }
        let url = format!("{}/files", Self::WEBAPI);
        let params = [
            ("cid", cid),
            ("show_dir", "1"),
            ("limit", "1"),
            ("offset", "0"),
            ("aid", "1"),
        ];
        let resp = self.get_with_retry(&url, &params).await?;
        // path 是面包屑数组，最后一项是当前目录
        if let Some(path_arr) = resp["path"].as_array() {
            if let Some(last) = path_arr.last() {
                let name = last["name"].as_str()
                    .or_else(|| last["n"].as_str())
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    return Ok(name);
                }
            }
        }
        // 兜底：返回 CID 本身
        Ok(cid.to_string())
    }

    /// 为指定文件/目录创建分享链接
    pub async fn create_share(&self, file_ids: &[&str], _title: Option<&str>, _duration_days: u32) -> anyhow::Result<ShareResult> {
        let url = format!("{}/share/send", Self::WEBAPI);
        let ids = file_ids.join(",");
        let form = [
            ("file_ids", ids.as_str()),
            ("ignore_warn", "1"),
            ("is_asc", "1"),
            ("order", "file_name"),
        ];
        let owned: Vec<(String, String)> = form.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let borrowed: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        if !resp["state"].as_bool().unwrap_or(false) {
            bail!("创建分享失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
        }

        let data = &resp["data"];
        let share_code = value_to_string(&data["share_code"])
            .filter(|code| !code.is_empty())
            .ok_or_else(|| anyhow::anyhow!("创建分享成功但响应缺少 share_code：{}", resp))?;
        let receive_code = value_to_string(&data["receive_code"]).unwrap_or_default();

        info!("🔗 创建分享链接 → 115.com/s/{}", share_code);

        Ok(ShareResult { share_url: format!("https://115.com/s/{}", share_code), pick_code: receive_code, share_id: share_code })
    }

    /// 验证分享链接是否可访问
    pub async fn verify_share(&self, share_code: &str, receive_code: Option<&str>) -> anyhow::Result<bool> {
        match self.parse_share(share_code, receive_code).await {
            Ok(_) => Ok(true),
            Err(e) => {
                warn!("分享链接验证失败 {}：{:?}", share_code, e);
                Ok(false)
            }
        }
    }

    /// 取消 115 云端分享链接
    /// POST https://webapi.115.com/share/updateshare  { share_code, action: "cancel" }
    pub async fn cancel_share(&self, share_code: &str) -> anyhow::Result<()> {
        let url = format!("{}/share/updateshare", Self::WEBAPI);
        let form = [
            ("share_code", share_code),
            ("action", "cancel"),
        ];
        let owned: Vec<(String, String)> = form.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let borrowed: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        let ok = resp["state"].as_bool()
            .unwrap_or_else(|| resp["state"].as_i64().map_or(false, |v| v != 0));
        if !ok {
            bail!("取消分享失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
        }
        info!("🗑️ 已取消 115 分享 {}", share_code);
        Ok(())
    }
}
