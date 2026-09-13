//! FinBook Web 服务端入口
//!
//! 多租户模式：全局平台身份库（realm.db）+ 多账套目录。
//! - 平台管理员：管理普通用户账号、查看全部账套（本身不需要建账套）。
//! - 普通用户：登录后自建账套（每个账套独立 `.fbk` 文件，彼此隔离）。
//!
//! 业务代码在 lib 目标（finweb::handlers / finweb::state），本文件只做启动装配。

use std::path::PathBuf;

use axum::Router;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use fincore::user::PasswordPolicy;

use finweb::handlers;
use finweb::realm::RealmDb;
use finweb::state::{BookRegistry, SessionStore, WebState};

/// 初始化日志：默认 info 级（含 HTTP 访问日志），可用 RUST_LOG 调级
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let listen = std::env::var("FINBOOK_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    // 平台身份库（全局账号 + 账套目录）
    let realm_path =
        std::env::var("FINBOOK_REALM").unwrap_or_else(|_| "./data/realm.db".to_string());
    // 用户自建账套的存放目录
    let books_dir = std::env::var("FINBOOK_BOOKS_DIR").unwrap_or_else(|_| "./data/books".to_string());
    let books_dir = PathBuf::from(&books_dir);
    std::fs::create_dir_all(&books_dir)?;

    // 兼容旧版单账套环境变量：若设置且目录里有账套文件，则注册为可选账套
    let legacy_dir = std::env::var("FINBOOK_DIR").ok().map(PathBuf::from);

    // 平台身份库：打开（首次自动建表）并引导管理员
    let realm = RealmDb::open(&realm_path)?;
    let bootstrap = realm.ensure_bootstrap(
        &std::env::var("FINBOOK_ADMIN_USER").unwrap_or_else(|_| "admin".to_string()),
        &std::env::var("FINBOOK_ADMIN_PASS").unwrap_or_default(),
        std::env::var("FINBOOK_ADMIN_MUST_CHANGE")
            .map(|v| v != "0" && v != "false")
            .unwrap_or(false),
        &PasswordPolicy::default(),
    )?;

    // 账套注册表：启动时把平台账套目录全量载入（key → path）
    let books = BookRegistry::new();
    for p in realm.load_all_book_paths()? {
        if p.exists() {
            books.register(&p, 16);
        }
    }
    // 旧版单账套目录里的文件也一并注册（便于迁移；归属仍以 realm_book 为准）
    if let Some(dir) = &legacy_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "fbk").unwrap_or(false) {
                    books.register(&p, 16);
                }
            }
        }
    }

    let state = WebState::new(
        books,
        SessionStore::new(),
        PasswordPolicy::default(),
        realm,
        books_dir.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
        default_period(),
        static_dir(),
        asset_version(),
    );

    let app = build_app(state.clone(), state.static_dir.clone());

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let book_count = state.books.list().len();
    println!(
        "\n  FinBook Web 已启动（多租户模式）\n  访问地址   : http://{listen}\n  平台身份库 : {}\n  账套目录   : {}\n  已注册账套 : {book_count} 个\n",
        state.realm.path().display(),
        books_dir.display(),
    );
    if let Some((user, pass)) = bootstrap {
        println!("  ┌─────────────────────────────────────────────┐");
        println!("  │ 已初始化平台管理员（请立即保存，仅显示一次）│");
        println!("  │   账号：{user:<40}│");
        println!("  │   口令：{pass:<40}│");
        println!("  └─────────────────────────────────────────────┘\n");
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// 默认工作期间：当前月份（ymm）
fn default_period() -> i32 {
    use chrono::Datelike;
    let now = chrono::Local::now();
    (now.year() as i32) * 100 + now.month() as i32
}

/// 组装应用：API 路由 + 访问日志 + 静态资源（前端 SPA）
fn build_app(state: std::sync::Arc<WebState>, static_dir: PathBuf) -> Router {
    // handlers::router 内部已带 /api 前缀，这里用 merge 而不是 nest，避免变成 /api/api/...
    // 访问日志：先挂 TraceLayer 再补 fallback，API 与静态资源请求均留痕
    let api = handlers::router(state);
    api.layer(TraceLayer::new_for_http())
        // 最外层兜 panic：即使某条请求路径上的 unwrap 触发，也只返回 500，不拖垮进程
        .layer(CatchPanicLayer::new())
        .fallback_service(ServeDir::new(static_dir))
}

/// 定位静态资源目录：优先环境变量，其次可执行文件同级的 static/，最后当前目录的 static/
fn static_dir() -> PathBuf {
    if let Ok(d) = std::env::var("FINWEB_STATIC_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("static");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("static")
}

/// 由前端静态文件的最新修改时间生成资源版本号。
/// 任何 JS/CSS 变更都会使版本号变化 → 首页 `?v=` 随之变化 → 浏览器缓存自动失效，
/// 避免用户长期拿到旧版前端（曾因固定 `?v=20260101` 导致行为与样式不同步）。
fn asset_version() -> String {
    let dir = static_dir();
    let mut latest: u128 = 0;
    for name in ["app.js", "style.css", "util.js"] {
        if let Ok(meta) = std::fs::metadata(dir.join(name)) {
            if let Ok(m) = meta.modified() {
                if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                    latest = latest.max(d.as_nanos());
                }
            }
        }
    }
    if latest == 0 {
        "dev".to_string()
    } else {
        latest.to_string()
    }
}
