# syntax=docker/dockerfile:1
# FinBook Web 端容器镜像（多阶段构建）
# 构建: docker build -t finbook:latest .
# 运行: docker run -d --name finbook -p 8080:8080 \
#         -v finbook_data:/data finbook:latest
# 说明: 账套文件默认放在 /data/finbook.fbk（工作目录 /app），
#       容器内务必备份到卷挂载目录；正式使用建议配合反向代理提供 HTTPS。

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
# 账套数据卷挂载点
VOLUME ["/data"]
ENV FINBOOK_DB=/data/finbook.fbk \
    FINBOOK_LISTEN=0.0.0.0:8080 \
    FINWEB_STATIC_DIR=/app/static
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/8080 || exit 1'
CMD ["finweb"]