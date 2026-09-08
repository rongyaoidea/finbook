# syntax=docker/dockerfile:1
# FinBook Web 端容器镜像（多阶段构建，多租户模式）
# 构建: docker build -t finbook:latest .
# 运行: docker run -d --name finbook -p 8080:8080 \
#         -v finbook_data:/data finbook:latest
# 说明: 平台身份库 realm.db 与用户自建账套 books/ 都放在 /data（数据卷），
#       容器销毁重建不会丢数据；正式使用建议配合反向代理提供 HTTPS。
# 平台管理员首次启动自动引导，账号口令见启动日志（可用 FINBOOK_ADMIN_USER/PASS 预置）。

# ── 构建阶段 ──────────────────────────────────────────────
FROM rust:1-bookworm AS builder
WORKDIR /build
# 先拷贝清单缓存依赖层（只有这两个文件变化时才重下依赖）
COPY Cargo.toml Cargo.lock ./
COPY crates/fincore/Cargo.toml crates/fincore/
COPY crates/findb/Cargo.toml crates/findb/
COPY crates/finweb/Cargo.toml crates/finweb/
# 预取依赖（等价源码占位，让 cargo 缓存依赖编译产物）
RUN mkdir -p crates/fincore/src crates/findb/src crates/finweb/src \
 && echo 'fn main() {}' > crates/finweb/src/main.rs \
 && echo '' > crates/fincore/src/lib.rs \
 && echo '' > crates/findb/src/lib.rs \
 && cargo build --release -p finweb 2>/dev/null || true
# 拷贝真实源码并构建
COPY crates/ crates/
RUN cargo build --release -p finweb

# ── 运行阶段 ──────────────────────────────────────────────
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/finweb /usr/local/bin/finweb
# 静态资源（SPA）
COPY crates/finweb/static ./static
# 平台身份库 + 账套目录统一挂到 /data 数据卷（重建容器不丢数据）
VOLUME ["/data"]
ENV FINBOOK_REALM=/data/realm.db \
    FINBOOK_BOOKS_DIR=/data/books \
    FINBOOK_LISTEN=0.0.0.0:8080 \
    FINWEB_STATIC_DIR=/app/static
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/8080 || exit 1'
CMD ["finweb"]
