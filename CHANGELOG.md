# Changelog

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.0.0/) 规范。  
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

---

## [0.1.0] — 2026-05-04

### 首个正式版本

#### 核心功能

- **全自动导入流水线**：提交 115 分享链接 → 自动解析 → 配额检查 → 转存 → 整理 → 重新分享
  - 状态机流转：`pending → parsing → waiting_space → transferring → organizing → sharing → completed`
  - 每步均持久化状态，重启后可继续
- **分享解析**：支持 `https://115.com/s/{id}` 和 `https://115cdn.com/s/{id}` 格式，自动提取 `?password=` 提取码
- **存储配额检查**：转存前检查剩余空间，空间不足时任务进入 `waiting_space` 等待
- **文件整理**：
  - 广告/垃圾文件过滤（关键词匹配 + 扩展名黑名单 + 视频最小体积检测）
  - 正则解析文件名提取：季集号、年份、画质、编码
  - 自动创建 `Movies/{Title (Year)}/` / `TV/{Title (Year)} {tmdb_id=xxx}/` 目录结构
- **TMDB 元数据匹配**：按解析出的标题+年份搜索，自动识别电影/剧集，写入资源库
- **创建新分享**：整理完成后对最终文件创建新的 115 分享链接，限速避免风控
- **分享链接健康检查**：每小时定时巡检所有 active 分享，自动标记失效链接

#### API 接口

- `GET /api/health` — 服务健康检查（含版本/构建信息）
- `GET /api/stats` — 仪表盘统计（资源数、分享数、任务数、存储配额）
- `GET/PUT /api/settings` — 运行时配置读写（热更新，无需重启）
- `GET/POST /api/tasks` — 任务列表/创建
- `GET /api/tasks/{id}` — 任务详情
- `POST /api/tasks/{id}/retry` — 重试失败/跳过任务
- `POST /api/tasks/{id}/cancel` — 取消待处理任务（`pending`/`waiting_space`）
- `DELETE /api/tasks/{id}` — 删除任务记录
- `GET /api/resources` — 资源库列表（分页 + 关键词搜索）
- `GET /api/resources/{id}` — 资源详情
- `GET /api/resources/{id}/shares` — 资源关联分享（含所有状态）
- `GET /api/resources/{id}/files` — 资源关联文件
- `GET /api/shares` — 分享管理列表（分页 + 状态过滤）
- `POST /api/shares/{id}/check` — 手动验证分享链接
- `POST /api/shares/{id}/rebuild` — 重建分享链接
- `DELETE /api/shares/{id}` — 删除分享（同时取消云端）
- `GET /api/folders` — 浏览 115 目录树
- `GET /api/folder-name` — 查询文件夹名称
- `GET /api/logs` — 任务日志流水（最新 200 条）
- `GET/PUT /api/logs/level` — 动态调整日志级别

#### WebUI

- 单页应用，Bootstrap 5.3.3 + Bootstrap Icons，无需构建工具
- **仪表盘**：资源/分享/任务/存储配额统计卡，颜色编码配额进度条
- **资源库**：分页展示，含 TMDB 海报/评分/简介，支持关键词搜索，详情模态框展示分享与文件
- **分享管理**：状态徽章（active/inactive/failed/deleted），支持验证/重建/删除
- **任务管理**：按状态过滤，显示进度步骤与错误信息，支持取消/重试/删除
- **设置页**：运行时配置热更新，文件夹选择器浏览 115 目录树
- **日志页**：任务流水展示，11 种状态分类过滤，5 秒自动刷新，动态调整日志级别

#### 安全性

- 分享 URL 格式校验，只允许 `115.com`/`115cdn.com` 域名
- 日志页链接 XSS 防护（`safeHref` 协议校验 + `rel="noopener noreferrer"`）
- 存储密码以 URL query 参数传输时自动剥离后存储

#### 定时任务

- 每小时：分享链接存活检查
- 每 30 分钟：存储配额检查（低于 90% 使用率发出警告）
- 每天凌晨 3 点：清理任务产生的 `hidden-task-*` 临时目录

#### 兼容性修复

- 115 API `state` 字段兼容整数 `1` 和布尔 `true` 两种形式
- 115 API `size` 字段兼容字符串 `"2199023255552"` 和数字两种形式
- `get_quota()` 正确读取 `data.all_remain.size` 字段（而非不存在的 `rt_space_info`）

#### 技术栈

| 组件 | 版本 |
|------|------|
| Rust | 1.76+ |
| Axum | 0.7 |
| SQLx | 0.7 |
| PostgreSQL | 15 |
| Redis | 7 |
| Docker | 20.10+ |

---

[0.1.0]: https://github.com/your-repo/hidden/releases/tag/v0.1.0
