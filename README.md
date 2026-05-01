# 洞天福地 (Hidden)

> 115 云盘分享池自动化管理系统 — Rust 版

**洞天福地** 是一套基于 Rust 构建的高性能媒体资源自动化流水线，支持 115 云盘分享链接的解析、转存、整理、重新分享，并提供完整的 REST API 供前端或其他系统调用。

---

## 特性

- **全自动导入流水线**：提交 115 分享链接 → 自动解析 → 配额检查 → 转存 → 整理 → 重新分享
- **智能文件名解析**：正则识别季/集/年份/画质，支持中英文混合命名
- **安全限速机制**：请求间隔 + 随机抖动，避免触发 115 风控
- **健康检查调度**：定时巡检所有分享链接是否存活，自动标记失效
- **REST API**：Axum 0.7 框架，全 JSON 接口，支持分页查询
- **优雅关闭**：监听 SIGTERM/Ctrl-C，安全释放连接池

---

## 技术栈

| 类型 | 技术 |
|------|------|
| Web 框架 | [Axum](https://github.com/tokio-rs/axum) 0.7 |
| 异步运行时 | [Tokio](https://tokio.rs) 1 |
| 数据库 ORM | [SQLx](https://github.com/launchbadge/sqlx) 0.7 + PostgreSQL 15 |
| 任务队列 | Redis (LPUSH / RPOP) |
| HTTP 客户端 | reqwest 0.11 |
| 定时任务 | tokio-cron-scheduler 0.9 |
| 日志追踪 | tracing + tracing-subscriber |
| 容器化 | Docker + docker-compose |

---

## 快速开始

### 前置要求

- Rust 1.76+（推荐通过 [rustup](https://rustup.rs) 安装）
- PostgreSQL 15+
- Redis 7+
- Docker & docker-compose（可选）

### 方式一：Docker Compose（推荐）

```bash
# 克隆项目
git clone <repo-url>
cd hidden

# 复制并编辑配置
cp .env.example .env
# 必填：ACCOUNT_115_COOKIE / ACCOUNT_115_ROOT_FOLDER_ID / TMDB_API_KEY

# 启动所有服务（自动构建、迁移、启动）
docker-compose up -d

# 查看日志
docker-compose logs -f hidden
```

### 方式二：本地开发

```bash
# 安装 sqlx-cli（用于数据库迁移）
cargo install sqlx-cli --no-default-features --features postgres

# 复制并编辑配置
cp .env.example .env

# 运行数据库迁移
sqlx migrate run

# 启动开发服务器（热重载建议配合 cargo-watch）
cargo run

# 或使用 cargo-watch 自动重启
cargo install cargo-watch
cargo watch -x run
```

---

## 配置说明

配置通过环境变量注入，参考 `.env.example` 文件。

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `DATABASE_URL` | PostgreSQL 连接字符串 | **必填** |
| `ACCOUNT_115_COOKIE` | 115 账号 Cookie | **必填** |
| `ACCOUNT_115_ROOT_FOLDER_ID` | 资源目标文件夹 ID | `0` |
| `ACCOUNT_115_TEMP_FOLDER_ID` | 临时转存文件夹 ID | `0` |
| `TMDB_API_KEY` | TMDB API Key | **必填** |
| `JWT_SECRET` | JWT 签名密钥 | 默认值（生产必改）|
| `TRANSFER_MAX_SIZE_GB` | 单次最大转存大小 | `80` GB |
| `SHARE_MIN_INTERVAL_SECS` | 创建分享最小间隔 | `30` 秒 |

完整变量列表见 [`.env.example`](.env.example)。

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
