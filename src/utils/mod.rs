use once_cell::sync::Lazy;
use regex::Regex;

// ─────────────────────────────────────────────
// 常量
// ─────────────────────────────────────────────

/// 视频文件扩展名
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "wmv", "flv", "m2ts", "ts", "rmvb", "rm",
    "iso", "bdmv",
];

/// 字幕文件扩展名
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "sup", "vtt"];

/// 常见广告/垃圾文件关键词（用于过滤）
static AD_KEYWORDS: &[&str] = &[
    "www.", ".com", ".net", ".cn", "广告", "招募",
    "合集", "整合", "搬运", "字幕组", "公众号",
];

// ─────────────────────────────────────────────
// 正则表达式（懒加载）
// ─────────────────────────────────────────────

/// 季集号识别：S01E01、S1E1、第1集、EP01 等
static RE_SEASON_EPISODE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        (?:
            [Ss](?P<s1>\d{1,3})[Ee](?P<e1>\d{1,3})    # S01E01
        |
            第\s*(?P<s2>\d{1,3})\s*季.*?第\s*(?P<e2>\d{1,3})\s*[集话] # 第1季第2集
        |
            [Ee][Pp]?(?P<e3>\d{1,3})                   # EP01
        |
            第\s*(?P<e4>\d{1,3})\s*[集话]              # 第01集
        )",
    )
    .expect("season/episode regex 编译失败")
});

/// 年份：4位数字，1900-2099
static RE_YEAR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:^|[\s.(【\[])(?P<y>(?:19|20)\d{2})(?:$|[\s.)】\]])").expect("year regex failed")
});

/// 画质标签：1080p、4K、2160p、HDR、HEVC、x265 等
static RE_QUALITY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)(?P<q>4[Kk]|2160[pP]|1080[pPiI]|720[pPiI]|480[pP]|HDR(?:10)?|SDR|HEVC|AVC|x265|x264|H\.?265|H\.?264|BluRay|WEB-?DL|HDTV|DVDRip)",
    )
    .expect("quality regex failed")
});

/// 技术标签截断：遇到这些词就认为标题已结束（片源/编码/分辨率/音频等）
static RE_TECH_CUT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        (?:^|[\s.\-_\[\(])
        (?:
            # 分辨率
            4[Kk]|2160[pP]|1080[pPiI]|720[pPiI]|480[pP]|UHD|FULL[\s._-]?HD
            # 片源
            |WEB[\s._-]?DL|WEBRip|BluRay|Blu[\s._-]?Ray|BDRip|BRRip|BDRemux|REMUX|HDTV|PDTV|HDCam|DVDRip|DVDScr|HDRip
            # 视频编码
            |[xX]265|[xX]264|H[\s._]?265|H[\s._]?264|HEVC|AVC|AV1|VP9
            # 音频编码（含声道数）
            |DTS[\s._-]?(?:HD|MA|X|ES)?|TrueHD|EAC3|AC3|AAC|FLAC|LPCM|PCM|MP3
            |DDP?[\s._]?\d[\s._]\d|DD\+[\s._]?\d
            # HDR 类型
            |HDR10\+?|HLG|Dolby[\s._]?Vision|DV(?=[\s.\-_\[\)]|$)
            # 流媒体来源
            |NF|AMZN|DSNP|HMAX|ATVP|PCOK|iT\b|STAN|FUNI|CRKL|PMTP|MA
            # 质量标签
            |IMAX|HQ|HQ[\s._-]?HDR|PROPER|REPACK|RETAIL|REMASTERED
        )
        (?:[\s.\-_\]\)]|$)
        "
    )
    .expect("tech cut regex failed")
});

// ─────────────────────────────────────────────
// 文件名解析
// ─────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ParsedFileName {
    pub title: String,
    pub year: Option<i32>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub quality: Option<String>,
    pub ext: String,
}

/// 解析媒体文件名，提取标题/年份/季/集/画质等信息
pub fn parse_file_name(file_name: &str) -> ParsedFileName {
    let ext = std::path::Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // 去掉扩展名
    let stem = std::path::Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name);

    let mut result = ParsedFileName {
        ext,
        ..Default::default()
    };

    // 标准化分隔符：点、下划线→空格
    let normalized = stem.replace(['.', '_'], " ");

    // 解析画质（从原始文件名中提取，避免分隔符替换后找不到）
    if let Some(caps) = RE_QUALITY.captures(&normalized) {
        result.quality = caps.name("q").map(|m| m.as_str().to_string());
    }

    // 解析季集号
    if let Some(caps) = RE_SEASON_EPISODE.captures(&normalized) {
        result.season = caps
            .name("s1")
            .or_else(|| caps.name("s2"))
            .and_then(|m| m.as_str().parse().ok());
        result.episode = caps
            .name("e1")
            .or_else(|| caps.name("e2"))
            .or_else(|| caps.name("e3"))
            .or_else(|| caps.name("e4"))
            .and_then(|m| m.as_str().parse().ok());
    }

    // 解析年份
    if let Some(caps) = RE_YEAR.captures(&normalized) {
        if let Some(y) = caps.name("y") {
            result.year = y.as_str().parse().ok();
        }
    }

    // ── 提取标题：取最左侧截断点之前的内容 ──────────────────
    // 截断点优先级：季集号 < 年份 < 技术标签，取最小 start 位置
    let mut cutoff = normalized.len();

    if let Some(m) = RE_SEASON_EPISODE.find(&normalized) {
        cutoff = cutoff.min(m.start());
    }
    if let Some(m) = RE_YEAR.find(&normalized) {
        cutoff = cutoff.min(m.start());
    }
    if let Some(m) = RE_TECH_CUT.find(&normalized) {
        // RE_TECH_CUT 允许前缀空格，实际内容从 +1 开始；直接用 match start 即可
        cutoff = cutoff.min(m.start());
    }

    let title_raw = &normalized[..cutoff];
    result.title = clean_title(title_raw);

    result
}

/// 清理标题中的噪声字符
fn clean_title(raw: &str) -> String {
    raw.replace(['.', '_', '-', '[', ']', '(', ')', '【', '】', '《', '》'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

// ─────────────────────────────────────────────
// 文件过滤
// ─────────────────────────────────────────────

/// 判断是否为视频文件
pub fn is_video_file(file_name: &str) -> bool {
    let ext = std::path::Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    VIDEO_EXTENSIONS.contains(&ext.as_str())
}

/// 判断是否为字幕文件
pub fn is_subtitle_file(file_name: &str) -> bool {
    let ext = std::path::Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    SUBTITLE_EXTENSIONS.contains(&ext.as_str())
}

/// 判断文件名中是否含有广告/垃圾关键词
pub fn is_ad_file(file_name: &str) -> bool {
    let lower = file_name.to_lowercase();
    AD_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

// ─────────────────────────────────────────────
// 大小格式化
// ─────────────────────────────────────────────

/// 将字节数格式化为可读字符串
pub fn format_size(bytes: i64) -> String {
    const GB: i64 = 1024 * 1024 * 1024;
    const MB: i64 = 1024 * 1024;
    const KB: i64 = 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_season_episode() {
        let parsed = parse_file_name("Breaking.Bad.S03E07.1080p.mkv");
        assert_eq!(parsed.season, Some(3));
        assert_eq!(parsed.episode, Some(7));
        assert_eq!(parsed.quality.as_deref(), Some("1080p"));
        assert_eq!(parsed.ext, "mkv");
    }

    #[test]
    fn test_parse_year() {
        let parsed = parse_file_name("Inception.2010.BluRay.1080p.mkv");
        assert_eq!(parsed.year, Some(2010));
    }

    #[test]
    fn test_is_video() {
        assert!(is_video_file("movie.mkv"));
        assert!(is_video_file("clip.mp4"));
        assert!(!is_video_file("cover.jpg"));
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(500), "500 B");
    }
}
