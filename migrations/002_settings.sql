-- 系统运行时配置表（优先级高于环境变量，可通过 WebUI 修改）
CREATE TABLE IF NOT EXISTS settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL DEFAULT '',
    description TEXT,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 初始配置项（已存在则跳过）
INSERT INTO settings (key, value, description) VALUES
    ('account_115_cookie',            '',      '115 账号 Cookie（优先级高于环境变量）'),
    ('account_115_root_folder_id',    '',      '115 成品目录 CID'),
    ('account_115_temp_folder_id',    '',      '115 临时转存目录 CID'),
    ('account_115_request_interval_ms','1500', 'API 请求间隔（毫秒）'),
    ('account_115_retry_times',       '3',     'API 失败重试次数'),
    ('tmdb_api_key',                  '',      'TMDB API Key（用于媒体信息匹配）'),
    ('tmdb_language',                 'zh-CN', 'TMDB 语言代码（如 zh-CN, en-US）'),
    ('share_max_create_per_minute',   '2',     '每分钟最多创建分享数'),
    ('share_max_create_per_hour',     '20',    '每小时最多创建分享数'),
    ('share_max_create_per_day',      '100',   '每天最多创建分享数'),
    ('share_min_interval_secs',       '30',    '两次创建分享最小间隔（秒）'),
    ('share_random_jitter_secs',      '15',    '创建分享随机抖动幅度（秒）'),
    ('clean_min_video_size_mb',       '100',   '整理时保留视频最小大小（MB，小于此值视为广告）'),
    ('transfer_min_free_space_gb',    '50',    '转存前检查最小剩余空间（GB）')
ON CONFLICT (key) DO NOTHING;
