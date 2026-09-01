//! finweb 关键 API 集成测试
//!
//! 用临时目录建真实账套 + tokio + tower oneshot 直接打路由，
//! 覆盖：初始化状态、首次登录即管理员、会话鉴权、权限拒绝、健康检查。
//! 这是「首登即管理员 + 一人一机 + 导出管控」三大部署级特性的回归防线。

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use fincore::user::PasswordPolicy;
use fincore::BookOptions;
use tower::ServiceExt;

use finweb::handlers;
use finweb::state::{BookRegistry, WebState};

/// 建一个临时账套 + 完整 WebState，返回 (state, book_path, dir_guard)
fn test_state() -> (Arc<WebState>, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let book_path = dir.path().join("test.fbk");
    let opts = BookOptions {
        start_period: fincore::Period::new(2026, 1).unwrap(),
        ..Default::default()
    };
    // create_no_admin：模拟「首次登录即管理员」初始化流程
    // company 留空 = 未建账（触发 needs_setup 建账向导）
    findb::Db::create_no_admin(&book_path, &opts).expect("建账失败");

    let books = BookRegistry::new();
    books.register(&book_path, 4);
    let state = WebState::new(
        books,
        PasswordPolicy::default(),
        book_path.clone(),
        "".to_string(),
        "test".to_string(),
        opts.start_period.ymm(),
    );
    (state, book_path, dir)
}

/// 把 JSON 包成 POST 请求
fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// 从登录响应的 set-cookie 里取出 `finbook_sid=xxx` 段
fn sid_from(resp: &axum::http::Response<Body>) -> String {
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("登录应返回 set-cookie")
        .to_str()
        .unwrap();
    cookie.split(';').next().unwrap().to_string()
}

async fn login(state: &Arc<WebState>, username: &str, password: &str) -> (StatusCode, String) {
    let resp = handlers::router(state.clone())
        .oneshot(post_json(
            "/api/login",
            serde_json::json!({
                "username": username,
                "password": password,
                "device_id": "dev-test-0001",
                "device_name": "测试机",
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let sid = if status.is_success() {
        sid_from(&resp)
    } else {
        String::new()
    };
    (status, sid)
}

/// 带 sid 的请求
fn authed_get(uri: &str, sid: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, sid)
        .body(Body::empty())
        .unwrap()
}

/// 带 sid 的 POST JSON 请求
fn authed_post(uri: &str, sid: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, sid)
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn body_string(resp: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("读取响应体失败");
    String::from_utf8_lossy(&bytes).to_string()
}

#[tokio::test]
async fn setup_status_reports_no_admin() {
    let (state, _bp, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/setup/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"admin_set\":false"), "新账套应无管理员：{s}");
    assert!(s.contains("\"needs_setup\":true"), "未建账账套应标记 needs_setup：{s}");
}

#[tokio::test]
async fn setup_wizard_completes_book() {
    let (state, _bp, _dir) = test_state();
    // 首登创建管理员
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    assert!(!sid.is_empty());

    // 建账向导：读 options → 设置公司名与启用期间 → 保存
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur = opts.as_object().unwrap().clone();
    let mut merged = cur.clone();
    merged.insert("company".into(), serde_json::json!("某某贸易有限公司"));
    merged.insert("start_period".into(), serde_json::json!(202601));
    merged.insert("base_currency".into(), serde_json::json!("CNY"));
    // options 走 PUT
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/options")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &sid)
                .body(Body::from(serde_json::Value::Object(merged).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建账保存应成功");

    // 保存后 setup/status → needs_setup=false，公司名已写入
    let resp = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/setup/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"needs_setup\":false"), "建账后 needs_setup 应为 false：{s}");
    assert!(s.contains("某某贸易有限公司"), "公司名应已写入：{s}");

    // 仪表盘显示新公司名与默认期间
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("某某贸易有限公司"), "仪表盘应显示新公司名：{s}");
}

#[tokio::test]
async fn health_endpoint() {
    let (state, _bp, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "ok");
}

#[tokio::test]
async fn first_login_becomes_admin() {
    let (state, _bp, _dir) = test_state();
    let (status, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(status, StatusCode::OK, "首次登录应成功创建管理员");
    assert!(!sid.is_empty());

    // 用会话访问 /api/me，应看到 admin 角色
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"is_admin\":true"), "首个账号应为管理员：{s}");
}

#[tokio::test]
async fn unauthenticated_me_is_401() {
    let (state, _bp, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_password_rejected() {
    let (state, _bp, _dir) = test_state();
    // 先建出管理员：首次登录用 boss
    let (_, _) = login(&state, "boss", "Admin!2026").await;
    // 错误口令 → 与用户名不存在返回相同状态码，防止枚举
    let (status, _) = login(&state, "boss", "WrongPass123!").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn non_admin_cannot_manage_users() {
    let (state, _bp, _dir) = test_state();
    // 管理员开通一个普通会计（must_change_pwd=false，避免测试中被拦截）
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &admin_sid,
            serde_json::json!({
                "username": "acc1",
                "display_name": "会计一",
                "password": "Acc@123456",
                "role": "accountant",
                "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员建号应成功");

    // 普通账号登录后访问用户管理 → 403
    let (status, sid2) = login(&state, "acc1", "Acc@123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/users", &sid2))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通账号不应能管用户");
}

#[tokio::test]
async fn device_binding_blocks_second_device() {
    let (state, _bp, _dir) = test_state();
    // 管理员建一个普通账号
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &admin_sid,
            serde_json::json!({
                "username": "emp1",
                "display_name": "员工一",
                "password": "Emp@123456",
                "role": "viewer",
            }),
        ))
        .await;
    drop(admin_sid);

    // 第一次登录：自动绑定 dev-A
    let (status, _) = login(&state, "emp1", "Emp@123456").await;
    assert_eq!(status, StatusCode::OK, "首次登录应绑定并成功");

    // 换设备 dev-B 登录：应被拒
    let resp = handlers::router(state.clone())
        .oneshot(post_json(
            "/api/login",
            serde_json::json!({
                "username": "emp1",
                "password": "Emp@123456",
                "device_id": "dev-B-0002",
                "device_name": "另一台电脑",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "换设备应被拒绝");
}
#[tokio::test]
async fn books_listed_and_login_with_book_key() {
    // 双账套：第一个空账套（可建管理员），第二个也空账套
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let b1 = dir.path().join("company-a.fbk");
    let b2 = dir.path().join("company-b.fbk");
    let opts = BookOptions {
        start_period: fincore::Period::new(2026, 1).unwrap(),
        ..Default::default()
    };
    findb::Db::create_no_admin(&b1, &opts).unwrap();
    findb::Db::create_no_admin(&b2, &opts).unwrap();

    let books = BookRegistry::new();
    books.register(&b1, 4);
    books.register(&b2, 4);
    let state = WebState::new(
        books,
        PasswordPolicy::default(),
        b1.clone(),
        "".to_string(),
        "test".to_string(),
        opts.start_period.ymm(),
    );

    // 账套列表应返回两个
    let resp = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/books").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("company-a"), "应包含账套 company-a：{s}");
    assert!(s.contains("company-b"), "应包含账套 company-b：{s}");

    // 登录到 company-b（带 book_key）
    let resp = handlers::router(state.clone())
        .oneshot(post_json(
            "/api/login",
            serde_json::json!({
                "username": "boss",
                "password": "Admin!2026",
                "device_id": "dev-multi-1",
                "device_name": "多账套测试",
                "book_key": "company-b",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "登录 company-b 应成功");
    let sid = sid_from(&resp);

    // 会话绑定 company-b：dashboard 显示默认期间即可（各账套独立）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn import_run_kingdee_template_csv() {
    let (state, _bp, _dir) = test_state();
    let (status, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(status, StatusCode::OK);
    // 金蝶模板期初：科目编码,科目名称,方向,期初余额,累计借方,累计贷方
    let csv = "\u{feff}科目编码,科目名称,方向,期初余额,累计借方,累计贷方\n\
               1001,库存现金,借,10000,50000,30000\n\
               100201,银行存款-工行,贷,2000,0,2000\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin",
                "template": "kingdee",
                "text": csv,
                "mapping": {},
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "金蝶模板导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 条：{s}");
}

#[tokio::test]
async fn import_run_excel_file_upload() {
    let (state, _bp, _dir) = test_state();
    let (status, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(status, StatusCode::OK);
    // 用 findb 的 fixture：用友模板期初表（Excel）
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/../findb/tests/fixtures/yonyou_begin.xlsx");
    let bytes = std::fs::read(fixture).expect("读取 fixture xlsx 失败");
    let b64 = base64_encode(&bytes);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin",
                "template": "yonyou",
                "text": "",
                "file": b64,
                "mapping": {},
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "Excel 上传导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 条：{s}");
}

/// 无依赖 base64 编码（测试辅助）
fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}
