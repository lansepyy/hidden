# ─── 第一阶段：构建 ──────────────────────────────────────────────
FROM rust:1.82-slim AS builder

# 构建信息（由 CI 通过 --build-arg 注入）
ARG BUILD_VERSION=dev
ARG BUILD_SHA=unknown
ARG BUILD_TIME=unknown

# 安装 OpenSSL 和 pkg-config（reqwest/sqlx 需要）
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# 先复制 Cargo.toml / Cargo.lock 以利用 Docker 缓存层
# Cargo.lock* 用 glob，有则复制，无则跳过（CI 首次构建时可能不存在）
COPY Cargo.toml Cargo.lock* ./
# 创建空 main.rs 用于预编译依赖（不使用 sqlx 宏，只拉依赖层）
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN SQLX_OFFLINE=true cargo build --release 2>/dev/null || true
RUN rm -f target/release/deps/hidden*

# 复制完整源码（含 CI 生成的 .sqlx/ 离线查询缓存）并正式编译
COPY . .
RUN BUILD_SHA="${BUILD_SHA}" \
    BUILD_TIME="${BUILD_TIME}" \
    BUILD_VERSION="${BUILD_VERSION}" \
    SQLX_OFFLINE=true \
    cargo build --release

# ─── 第二阶段：运行镜像 ─────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# 安装运行时依赖（OpenSSL、CA 证书、时区数据）
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    tzdata \
    && rm -rf /var/lib/apt/lists/*

# 创建非 root 用户运行应用
RUN groupadd -r hidden && useradd -r -g hidden hidden

WORKDIR /app

# 从构建阶段复制可执行文件和迁移文件
COPY --from=builder /build/target/release/hidden /app/hidden
COPY --from=builder /build/migrations /app/migrations

# 设置权限
RUN chown -R hidden:hidden /app
USER hidden

ENV TZ=Asia/Shanghai

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=10s --start-period=30s --retries=3 \
    CMD wget -qO- http://localhost:8080/api/health || exit 1

ENTRYPOINT ["/app/hidden"]
