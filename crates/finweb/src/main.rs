//! FinBook Web 服务端入口
//!
//! 在 Linux 服务器上运行，多个用户通过浏览器访问同一套账（服务端 SQLite，WAL 模式）。
//! 首次访问且账套为空时，「首次登录即管理员」：第一个成功登录的账号会成为系统管理员。
//!
//! 业务代码在 lib 目标（finweb::handlers / finweb::state），本文件只做启动装配。

use std::path::PathBuf;

use axum::Router;
use tower_http::services::ServeDir;

use fincore::user::PasswordPolicy;
use fincore::BookOptions;
use findb::{users, Db};

use finweb::handlers;
use finweb::state::{BookRegistry, WebState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let book = std::env::var("FINBOOK_DB").unwrap_or_else(|_| "./finbook.fbk".to_string());
    let listen = std::env::var("FINBOOK_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let path = PathBuf::from(&book);
    // 多账套目录（可选）：扫描其中所有 *.fbk 并注册；未设置则仅注册 FINBOOK_DB 单账套
    let book_dir = std::env::var("FINBOOK_DIR").ok().map(PathBuf::from);

    // 账套不存在则自动建账（不内置管理员 → 首次登录即管理员）
    let existed = path.exists();
    let db = if existed {
        Db::open(&path)?
    } else {
        let opts = BookOptions::default();
        Db::create_no_admin(&path, &opts)?
    };

    let company = db.options().company.clone();
    let default_period = db.options().start_period.ymm();
    let admin_set = users::admin_exists(&db)?;
    let admin_user = users::first_admin_username(&db)?;
    drop(db); // 释放这一连接，连接池会按需重新打开

    // 账套注册表：先注册 FINBOOK_DB（单账套兼容），再扫描 FINBOOK_DIR
    let books = BookRegistry::new();
    books.register(&path, 16);
    if let Some(dir) = &book_dir {
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
        PasswordPolicy::default(),
        path.clone(),
        company,
        env!("CARGO_PKG_VERSION").to_string(),
        default_period,
        static_dir(),
        asset_version(),
    );

    let app = build_app(state.clone(), state.static_dir.clone());

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!(
        "\n  FinBook Web 已启动\n  访问地址 : http://{listen}\n  账套文件 : {}\n  公司名称 : {}\n   管理员账号: {}\n",
        path.display(),
        state.company_name(),
        if admin_set {
            format!("已设定（{}）", admin_user.unwrap_or_default())
        } else {
            "未设定 —— 首次登录的账号将自动成为管理员".to_string()
        }
    );
    if !existed {
        println!("  （检测到账套文件不存在，已自动创建空账套，请使用浏览器首次登录以初始化管理员）\n");
    }
    if book_dir.is_some() {
        let books = state.books.list();
        let summary = books
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("  （多账套模式：已注册 {} 个账套：{}）\n", books.len(), summary);
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// 组装应用：API 路由 + 静态资源（前端 SPA）
fn build_app(state: std::sync::Arc<WebState>, static_dir: PathBuf) -> Router {
    // handlers::router 内部已带 /api 前缀，这里用 merge 而不是 nest，避免变成 /api/api/...
    let api = handlers::router(state);
    api.fallback_service(ServeDir::new(static_dir))
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
