use anyhow::Result;
use tracing::{info, warn};

use crate::{
    adapters::{Adapter115, FileEntry},
    config::Config,
    utils::{is_ad_file, is_video_file, parse_file_name, ParsedFileName},
};

use super::tmdb::TmdbResult;

// ─────────────────────────────────────────────
// 整理器
// ─────────────────────────────────────────────

/// 文件整理器：对转存进来的文件执行清理、重命名、移动操作
pub struct Organizer<'a> {
    adapter: &'a Adapter115,
    config: &'a Config,
}

impl<'a> Organizer<'a> {
    pub fn new(adapter: &'a Adapter115, config: &'a Config) -> Self {
        Self { adapter, config }
    }

    /// 对一批文件执行完整整理流程
    ///
    /// 返回整理后仍保留的文件 ID 列表，用于后续生成分享链接
    pub async fn organize(
        &self,
        files: &[FileEntry],
        tmdb: Option<&TmdbResult>,
        inferred: Option<&ParsedFileName>,
    ) -> Result<Vec<OrganizerResult>> {
        let min_video_bytes = self.config.clean_min_video_size_mb as i64 * 1024 * 1024;
        let root_folder = &self.config.account_115_root_folder_id;

        // 若有 TMDB 结果，在目标目录下为本次资源创建子目录
        let target_folder = self
            .ensure_target_folder(tmdb, inferred, root_folder)
            .await
            .unwrap_or_else(|_| root_folder.clone());

        let mut results: Vec<OrganizerResult> = Vec::new();

        for file in files.iter().filter(|f| !f.is_dir) {
            let file_id = match &file.file_id {
                Some(id) => id.clone(),
                None => continue,
            };

            // ── 1. 广告/垃圾文件过滤 ──────────────────────
            if self.should_delete(file, min_video_bytes) {
                info!("🗑️  删除广告/垃圾文件：{}", file.name);
                if let Err(e) = self.adapter.delete_files(&[&file_id]).await {
                    warn!("删除失败 {}：{:?}", file.name, e);
                }
                continue;
            }

            // ── 2. 生成标准化文件名 ────────────────────────
            let final_name = if is_video_file(&file.name) {
                build_standard_name(&file.name, tmdb)
                    .unwrap_or_else(|| file.name.clone())
            } else {
                file.name.clone()
            };

            // ── 3. 重命名（若名称有变化）──────────────────
            if final_name != file.name {
                info!("✏️  重命名：{} → {}", file.name, final_name);
                if let Err(e) = self.adapter.rename_file(&file_id, &final_name).await {
                    warn!("重命名失败（保留原名）：{:?}", e);
                }
            }

            // ── 4. 移动到目标目录 ──────────────────────────
            if !target_folder.is_empty() && target_folder != "0" {
                if let Err(e) = self.adapter.move_files(&[&file_id], &target_folder).await {
                    warn!("移动文件 {} 失败：{:?}", final_name, e);
                }
            }

            results.push(OrganizerResult {
                file_id,
                final_name,
                folder_id: target_folder.clone(),
            });
        }

        info!(
            "✅ 整理完成：保留 {} 个文件（原始 {} 个）",
            results.len(),
            files.iter().filter(|f| !f.is_dir).count()
        );

        Ok(results)
    }

    // ─────────────────────────────────────────────
    // 内部方法
    // ─────────────────────────────────────────────

    /// 判断文件是否应被删除（广告/垃圾/过小视频）
    fn should_delete(&self, file: &FileEntry, min_video_bytes: i64) -> bool {
        // 广告关键词
        if is_ad_file(&file.name) {
            return true;
        }

        // 扩展名黑名单
        let ext = std::path::Path::new(&file.name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if matches!(ext.as_str(), "url" | "lnk" | "nfo") {
            return true;
        }

        // 视频文件体积过小（广告片段，阈值由配置决定）
        if is_video_file(&file.name) && file.size > 0 && file.size < min_video_bytes {
            return true;
        }

        false
    }

    /// 在成品目录下为当前资源创建/定位子目录
    /// - 电影：`{root}/Movies/{Title} ({Year})/`
    /// - 剧集：`{root}/TV/{Title} ({Year})/`
    async fn ensure_target_folder(
        &self,
        tmdb: Option<&TmdbResult>,
        inferred: Option<&ParsedFileName>,
        root_folder: &str,
    ) -> Result<String> {
        if root_folder.is_empty() || root_folder == "0" {
            // 没有配置成品目录，直接用根目录
            return Ok(root_folder.to_string());
        }

        let tmdb = match tmdb {
            Some(t) => t,
            None => return Ok(root_folder.to_string()),
        };

        let category = match tmdb {
            TmdbResult::Movie(_) => "电影",
            TmdbResult::Tv(_) => "电视剧",
        };

        let year_suffix = tmdb
            .year()
            .map(|y| format!(" ({})", y))
            .unwrap_or_default();
        // 去掉文件名中不能出现的字符。若本地解析出中文标题且 TMDB 返回英文标题，
        // 优先使用本地中文标题，避免成品目录被重命名成英文名。
        let safe_title = sanitize_name(&preferred_title(tmdb, inferred.map(|p| p.title.as_str())));

        // 剧集文件夹名带 tmdb_id，电影文件夹名不带。
        // 资源目录名始终使用 TMDB 中文标题 + 年份；电影/剧集通过上层“电影/电视剧”目录区分。
        let resource_folder_name = match tmdb {
            TmdbResult::Tv(_) => format!("{}{} {{tmdb_id={}}}", safe_title, year_suffix, tmdb.tmdb_id()),
            TmdbResult::Movie(_) => format!("{}{}", safe_title, year_suffix),
        };

        // 先建分类目录（电影 / 电视剧），再建资源目录
        // 失败时直接用 root，不阻塞整体流程
        let category_folder = self
            .adapter
            .create_folder(root_folder, category)
            .await
            .unwrap_or_else(|_| root_folder.to_string());

        let resource_folder = self
            .adapter
            .create_folder(&category_folder, &resource_folder_name)
            .await?;

        Ok(resource_folder)
    }
}

// ─────────────────────────────────────────────
// 整理结果
// ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct OrganizerResult {
    /// 115 文件 ID
    pub file_id: String,
    /// 整理后的文件名
    pub final_name: String,
    /// 最终所在目录 ID
    pub folder_id: String,
}

// ─────────────────────────────────────────────
// 文件名标准化
// ─────────────────────────────────────────────

/// 根据 TMDB 元数据生成标准化文件名
///
/// - 电影：`Title (Year) - Quality - {tmdb_id=xxxxx}.ext`
/// - 剧集：`Title (Year) - S01E01 - Quality.ext`
pub fn build_standard_name(original: &str, tmdb: Option<&TmdbResult>) -> Option<String> {
    let parsed = parse_file_name(original);
    if parsed.ext.is_empty() {
        return None;
    }

    let title = preferred_title(tmdb, Some(parsed.title.as_str()));
    if title.is_empty() {
        return None;
    }
    let title = sanitize_name(&title);

    let year = tmdb.and_then(|t| t.year()).or(parsed.year);
    let year_str = year
        .map(|y| format!(" ({})", y))
        .unwrap_or_default();

    let se_str = match (parsed.season, parsed.episode) {
        (Some(s), Some(e)) => format!(" - S{:02}E{:02}", s, e),
        (None, Some(e)) => format!(" - E{:02}", e),
        _ => String::new(),
    };

    let quality_str = parsed
        .quality
        .as_deref()
        .map(|q| format!(" - {}", q))
        .unwrap_or_default();

    // 电影文件名末尾带 tmdb_id，剧集不带
    let tmdb_suffix = match tmdb {
        Some(TmdbResult::Movie(m)) => format!(" - {{tmdb_id={}}}", m.id),
        _ => String::new(),
    };

    Some(format!(
        "{}{}{}{}{}.{}",
        title, year_str, se_str, quality_str, tmdb_suffix, parsed.ext
    ))
}

/// 优先标题策略：
/// - TMDB 有中文标题时使用 TMDB 标题；
/// - 本地文件名解析出中文标题且 TMDB 标题不是中文时，使用本地中文标题；
/// - 否则使用 TMDB 标题；
/// - 没有 TMDB 时使用本地解析标题。
fn preferred_title(tmdb: Option<&TmdbResult>, local_title: Option<&str>) -> String {
    let local_title = local_title.map(str::trim).filter(|s| !s.is_empty());
    let tmdb_title = tmdb.map(|t| t.title().trim()).filter(|s| !s.is_empty());

    match (tmdb_title, local_title) {
        (Some(t), Some(local)) if contains_cjk(local) && !contains_cjk(t) => local.to_string(),
        (Some(t), _) => t.to_string(),
        (None, Some(local)) => local.to_string(),
        (None, None) => String::new(),
    }
}

fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{4e00}'..='\u{9fff}').contains(&c)
            || ('\u{3400}'..='\u{4dbf}').contains(&c)
            || ('\u{f900}'..='\u{faff}').contains(&c)
    })
}

/// 移除文件名中不合法的字符（Windows/115 均适用）
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}
