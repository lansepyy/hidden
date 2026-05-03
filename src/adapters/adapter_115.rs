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
    /// 分享/文件基础操作（webapi）
    const WEBAPI: &'static str = "https://webapi.115.com";
    /// 文件管理 open 接口（proapi）
    const PROAPI: &'static str = "https://proapi.115.com";
    /// 解析 115 分享链接，通过分页拉取完整文件树（参数完全对齐 p115client）
    ///
    /// GET https://webapi.115.com/share/snap
    /// params: share_code, receive_code, cid=0, limit=1000, offset=0
    pub async fn parse_share(
        &self,
        share_code: &str,
        receive_code: Option<&str>,
    ) -> anyhow::Result<ShareInfo> {
        let url = format!("{}/share/snap", Self::WEBAPI);

        let mut all_files: Vec<FileEntry> = Vec::new();
        let mut offset = 0usize;
        let limit = 1000usize;

        loop {
            let offset_str = offset.to_string();
            let limit_str = limit.to_string();
            let params = [
                ("share_code", share_code),
                ("receive_code", receive_code.unwrap_or("")),
                ("cid", "0"),
                ("limit", &limit_str),
                ("offset", &offset_str),
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
                all_files.push(FileEntry {
                    name: f["n"].as_str()
                        .or_else(|| f["file_name"].as_str())
                        .unwrap_or("").to_string(),
                    size: f["s"].as_i64()
                        .or_else(|| f["file_size"].as_i64())
                        .unwrap_or(0),
                    path: f["n"].as_str()
                        .or_else(|| f["file_name"].as_str())
                        .unwrap_or("").to_string(),
                    is_dir: f["ico"].as_str() == Some("folder")
                        || f["is_dir"].as_i64().unwrap_or(0) == 1,
                    file_id: f["fid"].as_str()
                        .or_else(|| f["file_id"].as_str())
                        .map(|s| s.to_string()),
                    pick_code: f["pc"].as_str()
                        .or_else(|| f["pick_code"].as_str())
                        .map(|s| s.to_string()),
                });
            }

            offset += files.len();
            if offset >= count || files.is_empty() {
                break;
            }
        }

        let total_size: i64 = all_files.iter().map(|f| f.size).sum();
        let file_count = all_files.iter().filter(|f| !f.is_dir).count();

        info!("📦 解析分享 {} 完成：{} 个文件，共 {} bytes", share_code, file_count, total_size);

        Ok(ShareInfo {
            share_id: share_code.to_string(),
            total_size,
            file_count,
            tree: all_files,
        })
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
    /// POST https://webapi.115.com/user/space_summury
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

    // ─────────────────────────────────────────────
    // 账号信息
    // ─────────────────────────────────────────────

    /// 获取账号存储配额
    /// GET https://proapi.115.com/android/user/space_info
    pub async fn get_quota(&self) -> anyhow::Result<QuotaInfo> {
        let url = format!("{}/android/user/space_info", Self::PROAPI);
        let resp = self.get_with_retry(&url, &[]).await?;

        // 响应可能是 {state:true, data:{rt_space_info:{...}}} 或直接包含数据
        let data = if resp["state"].as_bool().unwrap_or(false) {
            resp["data"].clone()
        } else {
            resp.clone()
        };

        // 字段路径：rt_space_info.all_total.size / rt_space_info.all_use.size
        let total = data["rt_space_info"]["all_total"]["size"].as_i64()
            .or_else(|| data["all_total"]["size"].as_i64())
            .unwrap_or(0);
        let used = data["rt_space_info"]["all_use"]["size"].as_i64()
            .or_else(|| data["all_use"]["size"].as_i64())
            .unwrap_or(0);

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
    ///
    /// GET https://webapi.115.com/share/snap
    /// params: share_code, receive_code, cid=0, limit=1000, offset
    pub async fn parse_share(
        &self,
        share_id: &str,
        pick_code: Option<&str>,
    ) -> anyhow::Result<ShareInfo> {
        let url = format!("{}/share/snap", Self::WEBAPI);
        let mut all_files: Vec<FileEntry> = Vec::new();
        let mut offset = 0usize;
        let limit = 1000usize;

        loop {
            let offset_str = offset.to_string();
            let limit_str = limit.to_string();
            let receive_code_val = pick_code.unwrap_or("");

            let params = [
                ("share_code", share_id),
                ("receive_code", receive_code_val),
                ("cid", "0"),
                ("limit", &limit_str),
                ("offset", &offset_str),
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
                all_files.push(FileEntry {
                    name: f["n"].as_str()
                        .or_else(|| f["file_name"].as_str())
                        .unwrap_or("").to_string(),
                    size: f["s"].as_i64()
                        .or_else(|| f["file_size"].as_i64())
                        .unwrap_or(0),
                    path: f["n"].as_str()
                        .or_else(|| f["file_name"].as_str())
                        .unwrap_or("").to_string(),
                    // ico = "folder" 表示目录
                    is_dir: f["ico"].as_str() == Some("folder")
                        || f["is_dir"].as_i64().unwrap_or(0) == 1,
                    file_id: f["fid"].as_str()
                        .or_else(|| f["file_id"].as_str())
                        .map(|s| s.to_string()),
                    pick_code: f["pc"].as_str()
                        .or_else(|| f["pick_code"].as_str())
                        .map(|s| s.to_string()),
                });
            }

            offset += files.len();
            if offset >= count || files.is_empty() {
                break;
            }
        }

        let total_size: i64 = all_files.iter().map(|f| f.size).sum();
        let file_count = all_files.iter().filter(|f| !f.is_dir).count();

        info!("📦 解析分享 {} 完成：{} 个文件，共 {} bytes", share_id, file_count, total_size);

        Ok(ShareInfo {
            share_id: share_id.to_string(),
            total_size,
            file_count,
            tree: all_files,
        })
    }

    // ─────────────────────────────────────────────
    // 文件操作
    // ─────────────────────────────────────────────

    /// 将分享文件转存到指定目录
    ///
    /// POST https://webapi.115.com/share/receive
    /// data: share_code, receive_code, file_id（逗号分隔）, cid（目标目录）
    pub async fn transfer_share(
        &self,
        share_id: &str,
        pick_code: Option<&str>,
        file_ids: &[&str],
        target_folder_id: &str,
    ) -> anyhow::Result<bool> {
        let url = format!("{}/share/receive", Self::WEBAPI);
        let ids = file_ids.join(",");
        let receive_code_val = pick_code.unwrap_or("");

        let form = [
            ("share_code", share_id),
            ("receive_code", receive_code_val),
            ("file_id", ids.as_str()),
            ("cid", target_folder_id),
        ];

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
    ///
    /// POST https://proapi.115.com/open/folder/add
    /// data: file_name, pid
    pub async fn create_folder(
        &self,
        parent_id: &str,
        name: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{}/open/folder/add", Self::PROAPI);
        let form = [("file_name", name), ("pid", parent_id)];
        let borrowed: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, *v)).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;

        if !resp["state"].as_bool().unwrap_or(false) {
            bail!("创建文件夹失败：{}", resp["msg"].as_str().unwrap_or("unknown"));
        }

        // 新建目录的 ID 在 data.file_id 或 data.cid
        let folder_id = resp["data"]["file_id"].as_str()
            .or_else(|| resp["data"]["cid"].as_str())
            .unwrap_or("")
            .to_string();

        info!("📁 创建文件夹 '{}' → cid={}", name, folder_id);
        Ok(folder_id)
    }

    /// 移动文件
    ///
    /// POST https://proapi.115.com/open/ufile/move
    /// data: file_ids（逗号分隔）, to_cid
    pub async fn move_files(
        &self,
        file_ids: &[&str],
        target_folder_id: &str,
    ) -> anyhow::Result<bool> {
        let url = format!("{}/open/ufile/move", Self::PROAPI);
        let ids = file_ids.join(",");
        let form = [("file_ids", ids.as_str()), ("to_cid", target_folder_id)];
        let borrowed: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, *v)).collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        Ok(resp["state"].as_bool().unwrap_or(false))
    }

    /// 重命名文件/目录
    ///
    /// POST https://proapi.115.com/open/ufile/update
    /// data: file_id, file_name
    pub async fn rename_file(
        &self,
        file_id: &str,
        new_name: &str,
    ) -> anyhow::Result<bool> {
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
    ///
    /// POST https://webapi.115.com/rb/delete
    /// data: fid[0]=x&fid[1]=y...（多文件用索引键）或 fid=x（单文件）
    pub async fn delete_files(&self, file_ids: &[&str]) -> anyhow::Result<bool> {
        let url = format!("{}/rb/delete", Self::WEBAPI);

        let owned: Vec<(String, String)> = if file_ids.len() == 1 {
            vec![("fid".to_string(), file_ids[0].to_string())]
        } else {
            file_ids.iter().enumerate()
                .map(|(i, id)| (format!("fid[{}]", i), id.to_string()))
                .collect()
        };
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let resp = self.post_with_retry(&url, &borrowed).await?;
        Ok(resp["state"].as_bool().unwrap_or(false))
    }

    /// 列出指定目录下的所有文件（不深递归，webapi 115 兼容性最佳）
    ///
    /// GET https://webapi.115.com/files
    /// params: cid, show_dir=1, limit=1150, offset=0, aid=1, count_folders=1, record_open_time=1
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
            .map(|f| FileEntry {
                name: f["n"].as_str()
                    .or_else(|| f["file_name"].as_str())
                    .unwrap_or("")
                    .to_string(),
                size: f["s"].as_i64()
                    .or_else(|| f["file_size"].as_i64())
                    .unwrap_or(0),
                path: f["n"].as_str()
                    .or_else(|| f["file_name"].as_str())
                    .unwrap_or("")
                    .to_string(),
                is_dir: f["ico"].as_str() == Some("folder")
                    || f["is_dir"].as_i64().unwrap_or(0) == 1,
                file_id: f["fid"].as_str()
                    .or_else(|| f["file_id"].as_str())
                    .map(|s| s.to_string()),
                pick_code: f["pc"].as_str()
                    .or_else(|| f["pick_code"].as_str())
                    .map(|s| s.to_string()),
            })
            .collect();

        info!("📂 列举目录 {} 完成：{} 个条目", cid, entries.len());
        Ok(entries)
    }

    // ─────────────────────────────────────────────
    // 分享链接管理
    // ─────────────────────────────────────────────

    /// 为指定文件/目录创建分享链接
    ///
    /// POST https://webapi.115.com/share/send
    /// data: file_ids（逗号分隔）, ignore_warn=1, is_asc=1, order=file_name
    pub async fn create_share(
        &self,
        file_ids: &[&str],
        _title: Option<&str>,
        _duration_days: u32,
    ) -> anyhow::Result<ShareResult> {
        let url = format!("{}/share/send", Self::WEBAPI);
        let ids = file_ids.join(",");
        let form = [
            ("file_ids", ids.as_str()),
            ("ignore_warn", "1"),
            ("is_asc", "1"),
            ("order", "file_name"),
        ];
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
        // share_code 是分享码（URL 路径部分）
        let share_code = data["share_code"].as_str().unwrap_or("").to_string();
        // receive_code 是提取码
        let receive_code = data["receive_code"].as_str().unwrap_or("").to_string();

        info!("🔗 创建分享链接 → 115.com/s/{}", share_code);

        Ok(ShareResult {
            share_url: format!("https://115.com/s/{}", share_code),
            pick_code: receive_code,
            share_id: share_code,
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
