-- 补齐转存限制运行时配置，并修正早期默认值与 .env.example / Config 不一致的问题
INSERT INTO settings (key, value, description) VALUES
    ('transfer_max_size_gb',       '80',  '单次转存最大文件总大小（GB，0 表示不限制）'),
    ('transfer_max_file_count',    '200', '单次转存最多文件数量（0 表示不限制）')
ON CONFLICT (key) DO NOTHING;

UPDATE settings
SET value = CASE key
    WHEN 'share_max_create_per_hour' THEN '60'
    WHEN 'share_max_create_per_day' THEN '300'
    WHEN 'share_random_jitter_secs' THEN '10'
    WHEN 'transfer_min_free_space_gb' THEN '20'
    ELSE value
END,
updated_at = NOW()
WHERE (key = 'share_max_create_per_hour' AND value = '20')
   OR (key = 'share_max_create_per_day' AND value = '100')
   OR (key = 'share_random_jitter_secs' AND value = '15')
   OR (key = 'transfer_min_free_space_gb' AND value = '50');
