# 洞天福地 (Hidden)

> 115 云盘分享池全自动管理系统 — Rust 版 | v0.1.0

**洞天福地** 是一套基于 Rust 构建的高性能媒体资源自动化流水线，支持 115 云盘分享链接的解析、转存、整理（TMDB 匹配 + 重命名）、重新分享，并提供完整的 REST API 和内置 WebUI。

---

## 特性

- **全自动导入流水线**：提交 115 分享链接 → 自动解析 → 配额检查 → 转存 → 整理 → 重新分享
- **TMDB 元数据匹配**：自动识别电影/剧集，创建标准目录结构 `Movies/{Title (Year)}/`
- **智能文件名解析**：正则识别季/集/年份/画质，支持中英文混合命名
- **安全限速机制**：请求间隔 + 随机抖动，避免触发 115 风控
- **健康检查调度**：每小时巡检所有分享链接是否存活，自动标记失效
- **内置 WebUI**：仪表盘、资源库、分享管理、任务管理、设置、日志，无需额外部署
- **REST API**：Axum 0.7 框架，全 JSON 接口，支持分页查询
- **热配置更新**：通过 WebUI 修改 Cookie、限速等参数，无需重启服务
- **优雅关闭**：监听 SIGTERM/Ctrl-C，安全释放连接池

---

## 技术栈

| 类型 | 技术 |
|------|------|
| Web 框架 | Axum 0.7 |
| 异步运行时 | Tokio 1 |
| 数据库 ORM | SQLx 0.7 + PostgreSQL 15 |
| 任务队列 | Redis 7 (LPUSH / RPOP) |
| HTTP 客户端 | reqwest 0.11 |
| 定时任务 | tokio-cron-scheduler 0.9 |
| 日志追踪 | tracing + tracing-subscriber |
| 容器化 | Docker + docker-compose |
| 前端 | Bootstrap 5.3.3（无构建工具） |

---

## 快速开始

### 前置要求

- Docker & docker-compose（推荐部署方式）
- 或：Rust 1.76+、PostgreSQL 15+、Redis 7+（本地开发）

### 方式一：Docker Compose（推荐）

```bash
# 克隆项目
git clone <repo-url>
cd hidden

# 复制并编辑配置
cp .env.example .env
# 编辑 .env，至少填写：
#   ACCOUNT_115_COOKIE       — 115 账号 Cookie
#   ACCOUNT_115_ROOT_FOLDER_ID — 资源目标文件夹 ID
#   ACCOUNT_115_TEMP_FOLDER_ID — 临时转存文件夹 ID
#   TMDB_API_KEY             — TMDB API Key
#   JWT_SECRET               — 修改为随机高强度字符串

# 启动所有服务（自动构建、数据库迁移、启动）
docker-compose up -d

# 查看日志
docker-compose logs -f hidden

# WebUI 访问地址
open http://localhost:8080
```

### 方式二：本地开发

```bash
# 安装 sqlx-cli
cargo install sqlx-cli --no-default-features --features postgres

# 复制并编辑配置
cp .env.example .env

# 创建数据库并运行迁移
sqlx database create
sqlx migrate run

# 启动服务（建议配合 cargo-watch）
cargo install cargo-watch
cargo watch -x run
```

---

## 配置说明

配置通过环境变量注入，支持 `.env` 文件。运行后也可通过 WebUI → 设置页 热更新（无需重启）。

### 必填项

| 变量 | 说明 |
|------|------|
| `DATABASE_URL` | PostgreSQL 连接字符串 |
| `ACCOUNT_115_COOKIE` | 115 账号 Cookie（从浏览器开发者工具获取） |
| `TMDB_API_KEY` | TMDB API Key（[申请地址](https://www.themoviedb.org/settings/api)） |
| `JWT_SECRET` | JWT 签名密钥（生产环境务必修改） |

### 选填项

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `ACCOUNT_115_ROOT_FOLDER_ID` | `""` | 整理后资源存放目录 CID，`0` 或空 = 根目录 |
| `ACCOUNT_115_TEMP_FOLDER_ID` | `""` | 转存中间临时目录 CID，`0` 或空 = 根目录 |
| `ACCOUNT_115_REQUEST_INTERVAL_MS` | `1500` | API 请求间隔（毫秒） |
| `ACCOUNT_115_RETRY_TIMES` | `3` | 请求失败重试次数 |
| `TMDB_LANGUAGE` | `zh-CN` | TMDB 语言代码 |
| `TRANSFER_MAX_SIZE_GB` | `80` | 单次最大转存大小（GB），`0` = 不限 |
| `TRANSFER_MAX_FILE_COUNT` | `200` | 单次最多转存文件数，`0` = 不限 |
| `TRANSFER_MIN_FREE_SPACE_GB` | `20` | 转存前要求最小剩余空间（GB） |
| `SHARE_MAX_CREATE_PER_MINUTE` | `2` | 每分钟最多创建分享数 |
| `SHARE_MAX_CREATE_PER_HOUR` | `20` | 每小时最多创建分享数 |
| `SHARE_MAX_CREATE_PER_DAY` | `100` | 每天最多创建分享数 |
| `SHARE_MIN_INTERVAL_SECS` | `30` | 两次创建分享最小间隔（秒） |
| `SHARE_RANDOM_JITTER_SECS` | `15` | 创建分享随机抖动幅度（秒） |
| `CLEAN_MIN_VIDEO_SIZE_MB` | `100` | 整理时保留视频最小大小（MB） |
| `PORT` | `8080` | 监听端口 |
| `RUST_LOG` | `hidden=debug,tower_http=warn,sqlx=warn` | 日志级别 |

---

## 导入任务流程

```
提交分享 URL
     │
     ▼
[parsing]      解析分享文件树，获取文件列表和总大小
     │
     ▼
[waiting_space] 检查配额：剩余空间 ≥ 总大小 + 最小余量
     │
     ▼
[transferring]  转存文件到临时目录
     │
     ▼
[organizing]   整理：过滤广告文件 → TMDB 匹配 → 重命名 → 移动到成品目录
     │
     ▼
[sharing]      创建新分享链接（含限速保护）
     │
     ▼
[completed]    写入资源库，任务完成
```

失败时可在 WebUI 任务页点击 **重试**；`waiting_space` 状态的任务可**取消**后重新提交。

---

## API 速览

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/health` | 健康检查 |
| GET | `/api/stats` | 仪表盘统计 |
| GET/PUT | `/api/settings` | 运行时配置 |
| POST | `/api/tasks` | 提交导入任务 |
| GET | `/api/tasks` | 任务列表（分页/过滤） |
| POST | `/api/tasks/{id}/retry` | 重试任务 |
| POST | `/api/tasks/{id}/cancel` | 取消任务 |
| DELETE | `/api/tasks/{id}` | 删除任务记录 |
| GET | `/api/resources` | 资源库列表 |
| GET | `/api/shares` | 分享列表 |
| POST | `/api/shares/{id}/rebuild` | 重建失效分享 |
| GET | `/api/logs` | 任务日志流水 |
| GET/PUT | `/api/logs/level` | 动态调整日志级别 |

---

## 目录结构

```
hidden/
├── src/
│   ├── main.rs              # 入口：初始化、路由、优雅关闭
│   ├── config.rs            # 环境变量配置
│   ├── error.rs             # 统一错误类型
│   ├── adapters/
│   │   └── adapter_115.rs   # 115 API 适配层（配额/解析/转存/分享）
│   ├── api/
│   │   ├── health.rs        # 健康检查、统计、日志
│   │   ├── tasks.rs         # 任务 CRUD
│   │   ├── resources.rs     # 资源库
│   │   ├── shares.rs        # 分享管理
│   │   └── settings.rs      # 运行时配置
│   ├── services/
│   │   ├── organizer.rs     # 文件整理器
│   │   ├── tmdb.rs          # TMDB 元数据搜索
│   │   └── share_limiter.rs # 分享创建限速
│   ├── workers/
│   │   ├── import_worker.rs # 导入任务状态机
│   │   └── mod.rs           # 调度器 + 定时任务
│   └── utils/               # 工具函数（文件名解析、视频判断等）
├── migrations/
│   ├── 001_initial.sql      # 基础表结构
│   └── 002_settings.sql     # 运行时配置表
├── static/
│   └── index.html           # 内置 WebUI（单文件，无构建工具）
├── Dockerfile
├── docker-compose.yml
├── .env.example
└── CHANGELOG.md
```

---

## 开发说明

### 获取 115 文件夹 ID

1. 打开 115 云盘，进入目标文件夹
2. 查看浏览器地址栏，`cid=` 后面的数字即为文件夹 ID
3. 或在 WebUI → 设置页使用文件夹选择器可视化浏览并选择

### 获取 TMDB API Key

1. 注册 [TMDB 账号](https://www.themoviedb.org)
2. 进入 设置 → API → 申请开发者 Key
3. 将 `v3 Auth` 下的 API Key 填入配置

### 动态调整日志级别

在 WebUI → 日志页可直接切换 DEBUG/INFO/WARN/ERROR，无需重启，立即生效。

---

## License

MIT


---

## API 接口

服务默认监听 `http://0.0.0.0:8080`，所有接口前缀为 `/api`。

### 健康检查

```
GET /api/health
```

### 任务管理

```
POST /api/tasks/import-share    提交导入任务
GET  /api/tasks                 任务列表（分页）
GET  /api/tasks/:id             任务详情
POST /api/tasks/:id/retry       重试失败任务
POST /api/tasks/:id/cancel      取消待处理任务
```

**提交任务示例：**

```json
POST /api/tasks/import-share
{
  "share_url": "https://115.com/s/xxxxxxxx",
  "pick_code": "abcd",
  "category": "movie",
  "priority": 5,
  "remark": "可选备注"
}
```

### 资源查询

```
GET /api/resources              搜索资源（keyword/type/year 过滤）
GET /api/resources/:id          资源详情
GET /api/resources/:id/shares   资源关联分享链接
GET /api/resources/:id/files    资源文件列表
```

### 分享管理

```
GET  /api/shares/:id            分享详情
POST /api/shares/:id/check      手动检查分享链接
POST /api/shares/:id/rebuild    重建已失效分享
```

---

## 项目结构

```
hidden/
├── src/
│   ├── main.rs              # 入口：初始化、路由、优雅关闭
│   ├── config.rs            # 配置结构体（from_env）
│   ├── error.rs             # 统一错误类型（AppError → HTTP 响应）
│   ├── api/
│   │   ├── mod.rs           # 路由注册
│   │   ├── health.rs        # GET /health
│   │   ├── tasks.rs         # 任务 CRUD
│   │   ├── resources.rs     # 资源查询
│   │   └── shares.rs        # 分享管理
│   ├── db/
│   │   ├── mod.rs           # 连接池初始化
│   │   └── models.rs        # SQLx FromRow 实体模型
│   ├── adapters/
│   │   ├── mod.rs
│   │   └── adapter_115.rs   # 115 云盘 API 封装
│   ├── workers/
│   │   ├── mod.rs           # 调度器 + enqueue_task
│   │   └── import_worker.rs # 任务处理状态机
│   ├── services/
│   │   └── mod.rs           # 服务层（可扩展 TMDB 匹配等）
│   └── utils/
│       └── mod.rs           # 文件名解析、大小格式化等工具
├── migrations/
│   └── 001_initial.sql      # 数据库 schema
├── Dockerfile               # 多阶段构建
├── docker-compose.yml       # 完整服务编排
├── .env.example             # 配置模板
└── Cargo.toml
```

---

## 任务状态流转

```
pending
  └─→ parsing          (解析分享链接)
        └─→ waiting_space     (检查配额)
              └─→ transferring      (转存文件)
                    └─→ organizing        (整理重命名)
                          └─→ sharing           (创建分享链接)
                                └─→ completed

  ※ 任意步骤出错 → failed（可手动 retry）
  ※ 手动取消（pending 状态）→ skipped
```

---

## 开发计划

- [ ] TMDB 自动匹配与元数据写入
- [ ] 文件重命名（Emby/Jellyfin 标准命名规范）
- [ ] 多账号负载均衡与故障转移
- [ ] WebSocket 实时任务进度推送
- [ ] Web 管理界面

---

## License

MIT
