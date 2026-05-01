-- 洞天福地 (Hidden) - 初始数据库迁移
-- Version: 001

-- ─────────────────────────────────────────
-- 枚举类型
-- ─────────────────────────────────────────

CREATE TYPE resource_type AS ENUM (
    'movie', 'tv', 'anime', 'variety', 'documentary', 'other'
);

CREATE TYPE task_status AS ENUM (
    'pending', 'parsing', 'waiting_space', 'transferring',
    'transfer_failed', 'organizing', 'sharing', 'verifying',
    'completed', 'failed', 'skipped'
);

CREATE TYPE share_status AS ENUM (
    'active', 'inactive', 'failed', 'deleted'
);

CREATE TYPE account_status AS ENUM (
    'active', 'inactive', 'banned', 'cookie_expired'
);

-- ─────────────────────────────────────────
-- 资源元数据表
-- ─────────────────────────────────────────

CREATE TABLE resources (
    id              BIGSERIAL PRIMARY KEY,
    title           VARCHAR(255) NOT NULL,
    original_title  VARCHAR(255),
    year            INTEGER,
    resource_type   VARCHAR(50) NOT NULL DEFAULT 'other',
    tmdb_id         BIGINT,
    imdb_id         VARCHAR(50),
    overview        TEXT,
    poster_url      VARCHAR(500),
    backdrop_url    VARCHAR(500),
    status          VARCHAR(50) NOT NULL DEFAULT 'active',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_resources_title ON resources(title);
CREATE INDEX idx_resources_tmdb_id ON resources(tmdb_id);
CREATE INDEX idx_resources_title_year ON resources(title, year);
CREATE INDEX idx_resources_status ON resources(status);

-- ─────────────────────────────────────────
-- 资源文件表
-- ─────────────────────────────────────────

CREATE TABLE resource_files (
    id              BIGSERIAL PRIMARY KEY,
    resource_id     BIGINT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    file_name       VARCHAR(500) NOT NULL,
    file_path       TEXT,
    file_size       BIGINT,
    file_ext        VARCHAR(50),
    media_type      VARCHAR(50),
    season          INTEGER,
    episode         INTEGER,
    quality         VARCHAR(50),
    source          VARCHAR(50),
    codec           VARCHAR(50),
    audio           VARCHAR(100),
    subtitle_info   TEXT,
    cloud_file_id   VARCHAR(100),
    pick_code       VARCHAR(100),
    strm_path       TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_resource_files_resource_id ON resource_files(resource_id);
CREATE INDEX idx_resource_files_cloud_file_id ON resource_files(cloud_file_id);

-- ─────────────────────────────────────────
-- 分享链接表
-- ─────────────────────────────────────────

CREATE TABLE shares (
    id              BIGSERIAL PRIMARY KEY,
    resource_id     BIGINT REFERENCES resources(id) ON DELETE SET NULL,
    share_url       TEXT NOT NULL,
    pick_code       VARCHAR(50),
    share_code      VARCHAR(100),
    share_title     VARCHAR(255),
    share_type      VARCHAR(50),
    file_count      INTEGER,
    total_size      BIGINT,
    status          VARCHAR(50) NOT NULL DEFAULT 'active',
    last_checked_at TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_shares_resource_id ON shares(resource_id);
CREATE INDEX idx_shares_status ON shares(status);

-- ─────────────────────────────────────────
-- 导入任务表
-- ─────────────────────────────────────────

CREATE TABLE import_tasks (
    id                  BIGSERIAL PRIMARY KEY,
    source_share_url    TEXT NOT NULL,
    source_pick_code    VARCHAR(50),
    status              VARCHAR(50) NOT NULL DEFAULT 'pending',
    total_size          BIGINT,
    total_files         INTEGER,
    current_step        VARCHAR(100),
    error_message       TEXT,
    priority            INTEGER NOT NULL DEFAULT 5,
    category            VARCHAR(50),
    remark              TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_import_tasks_status ON import_tasks(status);
CREATE INDEX idx_import_tasks_created_at ON import_tasks(created_at DESC);

-- ─────────────────────────────────────────
-- 导入批次表
-- ─────────────────────────────────────────

CREATE TABLE import_batches (
    id              BIGSERIAL PRIMARY KEY,
    task_id         BIGINT NOT NULL REFERENCES import_tasks(id) ON DELETE CASCADE,
    batch_index     INTEGER NOT NULL,
    status          VARCHAR(50) NOT NULL DEFAULT 'pending',
    file_count      INTEGER,
    total_size      BIGINT,
    temp_folder_id  VARCHAR(100),
    target_folder_id VARCHAR(100),
    share_id        BIGINT REFERENCES shares(id),
    error_message   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_import_batches_task_id ON import_batches(task_id);
CREATE INDEX idx_import_batches_status ON import_batches(status);

-- ─────────────────────────────────────────
-- 115 账号表
-- ─────────────────────────────────────────

CREATE TABLE accounts (
    id                  BIGSERIAL PRIMARY KEY,
    name                VARCHAR(255) NOT NULL UNIQUE,
    cookie_encrypted    TEXT NOT NULL,
    root_folder_id      VARCHAR(100),
    temp_folder_id      VARCHAR(100),
    total_size          BIGINT,
    used_size           BIGINT,
    free_size           BIGINT,
    status              VARCHAR(50) NOT NULL DEFAULT 'active',
    last_checked_at     TIMESTAMPTZ,
    last_failed_at      TIMESTAMPTZ,
    failure_count       INTEGER NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ─────────────────────────────────────────
-- 审计日志表
-- ─────────────────────────────────────────

CREATE TABLE audit_logs (
    id              BIGSERIAL PRIMARY KEY,
    user_id         BIGINT,
    action          VARCHAR(100) NOT NULL,
    resource_type   VARCHAR(50),
    resource_id     BIGINT,
    old_value       TEXT,
    new_value       TEXT,
    ip_address      VARCHAR(50),
    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_audit_logs_timestamp ON audit_logs(timestamp DESC);
CREATE INDEX idx_audit_logs_user_id ON audit_logs(user_id);

-- ─────────────────────────────────────────
-- 自动更新 updated_at 触发器
-- ─────────────────────────────────────────

CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_resources_updated_at
    BEFORE UPDATE ON resources
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TRIGGER trg_import_tasks_updated_at
    BEFORE UPDATE ON import_tasks
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TRIGGER trg_import_batches_updated_at
    BEFORE UPDATE ON import_batches
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TRIGGER trg_accounts_updated_at
    BEFORE UPDATE ON accounts
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();
