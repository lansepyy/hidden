use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::Config;

const TMDB_BASE: &str = "https://api.themoviedb.org/3";
const TMDB_IMAGE_BASE: &str = "https://image.tmdb.org/t/p/w500";

// ─────────────────────────────────────────────
// TMDB 返回结构体
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbMovie {
    pub id: i64,
    pub title: String,
    pub original_title: String,
    pub release_date: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(default)]
    pub vote_average: f64,
    #[serde(default)]
    pub vote_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbTv {
    pub id: i64,
    pub name: String,
    pub original_name: String,
    pub first_air_date: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(default)]
    pub vote_average: f64,
    #[serde(default)]
    pub vote_count: i64,
}

// ─────────────────────────────────────────────
// 统一结果枚举
// ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TmdbResult {
    Movie(TmdbMovie),
    Tv(TmdbTv),
}

impl TmdbResult {
    pub fn title(&self) -> &str {
        match self {
            TmdbResult::Movie(m) => &m.title,
            TmdbResult::Tv(t) => &t.name,
        }
    }

    pub fn original_title(&self) -> &str {
        match self {
            TmdbResult::Movie(m) => &m.original_title,
            TmdbResult::Tv(t) => &t.original_name,
        }
    }

    pub fn year(&self) -> Option<i32> {
        let date = match self {
            TmdbResult::Movie(m) => m.release_date.as_deref(),
            TmdbResult::Tv(t) => t.first_air_date.as_deref(),
        };
        date.and_then(|d| d.split('-').next())
            .and_then(|y| y.parse().ok())
    }

    pub fn overview(&self) -> Option<&str> {
        match self {
            TmdbResult::Movie(m) => m.overview.as_deref(),
            TmdbResult::Tv(t) => t.overview.as_deref(),
        }
    }

    pub fn poster_url(&self) -> Option<String> {
        let path = match self {
            TmdbResult::Movie(m) => m.poster_path.as_deref(),
            TmdbResult::Tv(t) => t.poster_path.as_deref(),
        };
        path.filter(|p| !p.is_empty())
            .map(|p| format!("{}{}", TMDB_IMAGE_BASE, p))
    }

    pub fn backdrop_url(&self) -> Option<String> {
        let path = match self {
            TmdbResult::Movie(m) => m.backdrop_path.as_deref(),
            TmdbResult::Tv(t) => t.backdrop_path.as_deref(),
        };
        path.filter(|p| !p.is_empty())
            .map(|p| format!("{}{}", TMDB_IMAGE_BASE, p))
    }

    pub fn tmdb_id(&self) -> i64 {
        match self {
            TmdbResult::Movie(m) => m.id,
            TmdbResult::Tv(t) => t.id,
        }
    }

    /// 返回资源类型字符串（对应 resources.resource_type）
    pub fn media_type(&self) -> &str {
        match self {
            TmdbResult::Movie(_) => "movie",
            TmdbResult::Tv(_) => "tv",
        }
    }

    /// 置信度分（越高越可靠，用于选择电影/剧集哪个更准确）
    fn confidence(&self) -> f64 {
        match self {
            TmdbResult::Movie(m) => m.vote_average * (m.vote_count as f64).ln().max(1.0),
            TmdbResult::Tv(t) => t.vote_average * (t.vote_count as f64).ln().max(1.0),
        }
    }
}

// ─────────────────────────────────────────────
// TMDB 客户端
// ─────────────────────────────────────────────

pub struct TmdbClient {
    client: Client,
    api_key: String,
    language: String,
}

impl TmdbClient {
    pub fn new(config: &Config) -> Result<Self> {
        if config.tmdb_api_key.is_empty() {
            bail!("TMDB API Key 未配置");
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            client,
            api_key: config.tmdb_api_key.clone(),
            language: config.tmdb_language.clone(),
        })
    }

    // ─────────────────────────────────────────────
    // 内部请求
    // ─────────────────────────────────────────────

    async fn get(&self, path: &str, extra: &[(&str, &str)]) -> Result<serde_json::Value> {
        let url = format!("{}{}", TMDB_BASE, path);
        let mut params = vec![
            ("api_key", self.api_key.as_str()),
            ("language", self.language.as_str()),
        ];
        params.extend_from_slice(extra);

        let resp = self
            .client
            .get(&url)
            .query(&params)
            .send()
            .await
            .context("TMDB 请求失败")?;

        if !resp.status().is_success() {
            bail!("TMDB HTTP 错误：{}", resp.status());
        }

        resp.json::<serde_json::Value>()
            .await
            .context("解析 TMDB 响应失败")
    }

    // ─────────────────────────────────────────────
    // 搜索接口
    // ─────────────────────────────────────────────

    /// 搜索电影，返回按热度排序的结果列表
    pub async fn search_movie(&self, title: &str, year: Option<i32>) -> Result<Vec<TmdbMovie>> {
        let year_str = year.map(|y| y.to_string());
        let mut extra: Vec<(&str, &str)> = vec![("query", title)];
        if let Some(ref y) = year_str {
            extra.push(("year", y.as_str()));
        }

        let resp = self.get("/search/movie", &extra).await?;
        let movies: Vec<TmdbMovie> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();

        Ok(movies)
    }

    /// 搜索电视剧，返回按热度排序的结果列表
    pub async fn search_tv(&self, title: &str, year: Option<i32>) -> Result<Vec<TmdbTv>> {
        let year_str = year.map(|y| y.to_string());
        let mut extra: Vec<(&str, &str)> = vec![("query", title)];
        if let Some(ref y) = year_str {
            extra.push(("first_air_date_year", y.as_str()));
        }

        let resp = self.get("/search/tv", &extra).await?;
        let shows: Vec<TmdbTv> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();

        Ok(shows)
    }

    /// TMDB 热门电影，用于资源库首页推荐入口。
    pub async fn popular_movies(&self, limit: usize) -> Result<Vec<TmdbMovie>> {
        let resp = self.get("/movie/popular", &[]).await?;
        let mut movies: Vec<TmdbMovie> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        movies.truncate(limit);
        Ok(movies)
    }

    /// TMDB 正在热映电影
    pub async fn now_playing_movies(&self, limit: usize) -> Result<Vec<TmdbMovie>> {
        let resp = self.get("/movie/now_playing", &[]).await?;
        let mut movies: Vec<TmdbMovie> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        movies.truncate(limit);
        Ok(movies)
    }

    /// TMDB 即将上映电影
    pub async fn upcoming_movies(&self, limit: usize) -> Result<Vec<TmdbMovie>> {
        let resp = self.get("/movie/upcoming", &[]).await?;
        let mut movies: Vec<TmdbMovie> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        movies.truncate(limit);
        Ok(movies)
    }

    /// TMDB 高分电影 (top_rated)
    pub async fn top_rated_movies(&self, limit: usize) -> Result<Vec<TmdbMovie>> {
        let resp = self.get("/movie/top_rated", &[]).await?;
        let mut movies: Vec<TmdbMovie> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        movies.truncate(limit);
        Ok(movies)
    }

    /// TMDB 热门电视剧，用于资源库首页推荐入口。
    pub async fn popular_tv(&self, limit: usize) -> Result<Vec<TmdbTv>> {
        let resp = self.get("/tv/popular", &[]).await?;
        let mut shows: Vec<TmdbTv> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        shows.truncate(limit);
        Ok(shows)
    }

    /// TMDB 今日播出剧集
    pub async fn airing_today_tv(&self, limit: usize) -> Result<Vec<TmdbTv>> {
        let resp = self.get("/tv/airing_today", &[]).await?;
        let mut shows: Vec<TmdbTv> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        shows.truncate(limit);
        Ok(shows)
    }

    /// TMDB 高分剧集 (top_rated)
    pub async fn top_rated_tv(&self, limit: usize) -> Result<Vec<TmdbTv>> {
        let resp = self.get("/tv/top_rated", &[]).await?;
        let mut shows: Vec<TmdbTv> = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        shows.truncate(limit);
        Ok(shows)
    }

    // ─────────────────────────────────────────────
    // 详情接口
    // ─────────────────────────────────────────────

    /// 获取电影详情
    pub async fn movie_detail(&self, tmdb_id: i64) -> Result<TmdbMovie> {
        let path = format!("/movie/{}", tmdb_id);
        let resp = self.get(&path, &[]).await?;
        serde_json::from_value(resp).context("解析电影详情失败")
    }

    /// 获取剧集详情
    pub async fn tv_detail(&self, tmdb_id: i64) -> Result<TmdbTv> {
        let path = format!("/tv/{}", tmdb_id);
        let resp = self.get(&path, &[]).await?;
        serde_json::from_value(resp).context("解析剧集详情失败")
    }

    // ─────────────────────────────────────────────
    // 智能匹配
    // ─────────────────────────────────────────────

    /// 同时搜索电影和剧集，选择置信度更高的结果
    ///
    /// 策略：
    /// 1. 有标题的文件先用文件名解析拿到干净的 title + year
    /// 2. 并发请求电影/剧集，各取第一条
    /// 3. 若只有一个有结果，直接用那个
    /// 4. 若都有结果，比较 vote_average × ln(vote_count) 取高者
    pub async fn smart_search(&self, title: &str, year: Option<i32>) -> Option<TmdbResult> {
        if title.is_empty() {
            return None;
        }

        let result = self.smart_search_with_year(title, year).await;
        // 如果带年份搜索无结果，去掉年份再搜一次（避免因年份偏差导致匹配失败）
        if result.is_none() && year.is_some() {
            warn!("TMDB 带年份搜索无结果，尝试去掉年份重搜：'{}'", title);
            return self.smart_search_with_year(title, None).await;
        }
        result
    }

    async fn smart_search_with_year(&self, title: &str, year: Option<i32>) -> Option<TmdbResult> {
        let (movies, shows) = tokio::join!(
            self.search_movie(title, year),
            self.search_tv(title, year),
        );

        let best_movie = movies.ok().and_then(|mut v| {
            if v.is_empty() {
                None
            } else {
                v.sort_by(|a, b| {
                    let sa = a.vote_average * (a.vote_count as f64).ln().max(1.0);
                    let sb = b.vote_average * (b.vote_count as f64).ln().max(1.0);
                    sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
                });
                Some(v.remove(0))
            }
        });

        let best_show = shows.ok().and_then(|mut v| {
            if v.is_empty() {
                None
            } else {
                v.sort_by(|a, b| {
                    let sa = a.vote_average * (a.vote_count as f64).ln().max(1.0);
                    let sb = b.vote_average * (b.vote_count as f64).ln().max(1.0);
                    sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
                });
                Some(v.remove(0))
            }
        });

        match (best_movie, best_show) {
            (Some(m), None) => {
                debug!("TMDB 匹配到电影：{} ({})", m.title, m.id);
                Some(TmdbResult::Movie(m))
            }
            (None, Some(t)) => {
                debug!("TMDB 匹配到剧集：{} ({})", t.name, t.id);
                Some(TmdbResult::Tv(t))
            }
            (Some(m), Some(t)) => {
                let movie_result = TmdbResult::Movie(m.clone());
                let tv_result = TmdbResult::Tv(t.clone());
                if movie_result.confidence() >= tv_result.confidence() {
                    debug!("TMDB 倾向电影（置信度更高）：{}", m.title);
                    Some(TmdbResult::Movie(m))
                } else {
                    debug!("TMDB 倾向剧集（置信度更高）：{}", t.name);
                    Some(TmdbResult::Tv(t))
                }
            }
            (None, None) => {
                warn!("TMDB 未找到匹配：'{}' year={:?}", title, year);
                None
            }
        }
    }
}
