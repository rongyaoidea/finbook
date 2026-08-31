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
use finweb::state::{DbPool, WebState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let book = std::env::var("FINBOOK_DB").unwrap_or_else(|_| "./finbook.fbk".to_string());
    let listen = std::env::var("FINBOOK_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let path = PathBuf::from(&book);

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

    let pool = DbPool::new(&path, 16);
    let state = WebState::new(
        pool,
        PasswordPolicy::default(),
        path.clone(),
        company,
        env!("CARGO_PKG_VERSION").to_string(),
        default_period,
    );

    let app = build_app(state.clone());

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!(
        "\n  FinBook Web 已启动\n  访问地址 : http://{listen}\n  账套文件 : {}\n  公司名称 : {}\n   管理员账号: {}\n",
        path.display(),
        state.company,
        if admin_set {
            format!("已设定（{}）", admin_user.unwrap_or_default())
        } else {
            "未设定 —— 首次登录的账号将自动成为管理员".to_string()
        }
    );
    if !existed {
        println!("  （检测到账套文件不存在，已自动创建空账套，请使用浏览器首次登录以初始化管理员）\n");
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// 组装应用：API 路由 + 静态资源（前端 SPA）
fn build_app(state: std::sync::Arc<WebState>) -> Router {
    // handlers::router 内部已带 /api 前缀，这里用 merge 而不是 nest，避免变成 /api/api/...
    let api = handlers::router(state);
    let static_dir = static_dir();
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
