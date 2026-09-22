//! finweb 关键 API 集成测试（多租户模型）
//!
//! 覆盖：平台身份库引导、平台登录、普通用户自建账套、归属隔离、
//! 成员协作（邀请账套内成员）、管理员跨账套查看（不留痕迹）、
//! 平台账号管理权限边界、「一人一机」设备绑定与重置、导入等核心业务链路。
//! 这是「多租户 + 归属隔离 + 设备绑定」部署级特性的回归防线。

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use fincore::user::PasswordPolicy;
use fincore::BookOptions;
use tower::ServiceExt;

use finweb::handlers;
use finweb::realm::RealmDb;
use finweb::state::{BookRegistry, SessionStore, WebState};

/// 建临时平台身份库 + 预置账套 + 完整 WebState，返回 (state, books_dir, dir_guard)
fn test_state() -> (Arc<WebState>, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    // 平台身份库：引导管理员 boss（不强制改密，便于测试）
    let realm = RealmDb::open(dir.path().join("realm.db")).expect("建 realm 失败");
    realm
        .ensure_bootstrap("boss", "Admin!2026", false, &PasswordPolicy::default())
        .expect("引导管理员失败");

    // 预置一个归属 boss 的账套 b1
    let books_dir = dir.path().join("books");
    std::fs::create_dir_all(&books_dir).expect("建账套目录失败");
    let opts = BookOptions {
        start_period: fincore::Period::new(2026, 1).unwrap(),
        ..Default::default()
    };
    let book_path = books_dir.join("b1.fbk");
    let db = findb::Db::create_no_admin(&book_path, &opts).expect("建账失败");
    // 身份对账：把 boss 种子为账套内管理员（复用生产逻辑）
    let ru = realm.get_user("boss").unwrap().expect("boss 应存在");
    finweb::realm::ensure_book_admin(&db, &ru).expect("种子账套管理员失败");
    drop(db);
    realm
        .register_book("b1", &book_path.to_string_lossy(), "boss", "预置公司")
        .unwrap();

    let reg = BookRegistry::new();
    reg.register(&book_path, 4);
    let state = WebState::new(
        reg,
        SessionStore::new(),
        PasswordPolicy::default(),
        realm,
        books_dir.clone(),
        "test".to_string(),
        opts.start_period.ymm(),
        std::path::PathBuf::new(),
        "test".to_string(),
    );
    (state, books_dir, dir)
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
    login_with_device(state, username, password, "dev-test-0001").await
}

async fn login_with_device(
    state: &Arc<WebState>,
    username: &str,
    password: &str,
    device_id: &str,
) -> (StatusCode, String) {
    let resp = handlers::router(state.clone())
        .oneshot(post_json(
            "/api/login",
            serde_json::json!({
                "username": username,
                "password": password,
                "device_id": device_id,
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

/// 选择当前账套（进入某套账）
async fn select_book(state: &Arc<WebState>, sid: &str, key: &str) -> StatusCode {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/books/{}/select", key),
            sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    resp.status()
}

/// 普通用户自助建账套（返回 (status, body)）
async fn create_book(state: &Arc<WebState>, sid: &str, company: &str) -> (StatusCode, String) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/books",
            sid,
            serde_json::json!({ "key": "", "company": company, "start_period": 202601 }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let s = body_string(resp).await;
    (status, s)
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

/// 带 sid 的 PUT JSON 请求
fn authed_put(uri: &str, sid: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
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

// ---------------------------------------------------------------------------
// 基础
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_endpoint() {
    let (state, _bd, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "ok");
}

#[tokio::test]
async fn unauthenticated_me_is_401() {
    let (state, _bd, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_password_rejected() {
    let (state, _bd, _dir) = test_state();
    let (status, _) = login(&state, "boss", "WrongPass123!").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn setup_status_after_bootstrap() {
    let (state, _bd, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/setup/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"admin_set\":true"), "引导后应有管理员：{s}");
}

// ---------------------------------------------------------------------------
// 平台登录 + 归属可见性
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_login_returns_platform_identity() {
    let (state, _bd, _dir) = test_state();
    let (status, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(status, StatusCode::OK, "平台登录应成功");
    assert!(!sid.is_empty());
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"is_admin\":true"), "应返回平台身份：{s}");
    assert!(s.contains("b1"), "管理员应看到预置账套：{s}");
    // 未选账套前，进入账套级接口应被引导去选账套
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "未选账套时 /me 应 401");
}

#[tokio::test]
async fn owner_enters_own_book_and_sees_company() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let status = select_book(&state, &sid, "b1").await;
    assert_eq!(status, StatusCode::OK, "进入自己的账套应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"vouchers\""), "仪表盘应正常返回：{s}");
}

// ---------------------------------------------------------------------------
// 自建账套 + 隔离
// ---------------------------------------------------------------------------

#[tokio::test]
async fn normal_user_creates_and_enters_own_book() {
    let (state, _bd, _dir) = test_state();
    // 管理员开通普通用户
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({
                "username": "zhang",
                "display_name": "张会计",
                "password": "Zhang123456",
                "is_admin": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通平台用户应成功");

    // 普通用户登录（首登强制改密）→ 改密
    let (status, sid) = login(&state, "zhang", "Zhang123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "强制改密应成功");

    // 自建账套并进入
    let (status, body) = create_book(&state, &sid, "张记贸易").await;
    assert_eq!(status, StatusCode::OK, "自建账套应成功：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();

    let status = select_book(&state, &sid, &key).await;
    assert_eq!(status, StatusCode::OK, "进入自建账套应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"is_admin\":true"), "创建者应为账套内管理员：{s}");
}

#[tokio::test]
async fn normal_user_cannot_enter_others_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    // 开通 wang（无账套）
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "wang", "display_name": "王出纳", "password": "Wang123456" }),
        ))
        .await
        .unwrap();
    let (_, sid) = login(&state, "wang", "Wang123456").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Wang123456", "new": "Wang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 试图进入 boss 的账套 → 403
    let status = select_book(&state, &sid, "b1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "普通用户不应能进入他人账套");

    // 自己的账套列表应为空
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"books\":[]"), "无账套用户列表应为空：{s}");
}

#[tokio::test]
async fn normal_user_cannot_manage_platform_users() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "wang", "display_name": "王出纳", "password": "Wang123456" }),
        ))
        .await
        .unwrap();
    let (_, sid) = login(&state, "wang", "Wang123456").await;
    // 首登强制改密后再访问（强制改密期间会被 401 拦截）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Wang123456", "new": "Wang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/platform/users", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通用户不应能管平台账号");
}

// ---------------------------------------------------------------------------
// 管理员跨账套 + 平台账号管理
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_can_enter_any_book() {
    let (state, books_dir, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    // boss 既是平台管理员也是 b1 的归属者；再开一个普通用户建账套验证跨账套
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "li", "display_name": "李会计", "password": "Li123456" }),
        ))
        .await
        .unwrap();
    let (_, li_sid) = login(&state, "li", "Li123456").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &li_sid,
            serde_json::json!({ "old": "Li123456", "new": "Li654321" }),
        ))
        .await
        .unwrap();
    let (_, body) = create_book(&state, &li_sid, "李记商行").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();

    // 平台管理员进入 li 的账套：临时管理员身份，不写入账套 user 表
    let status = select_book(&state, &admin_sid, &key).await;
    assert_eq!(status, StatusCode::OK, "平台管理员应能进入任意账套");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &admin_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员进入后 /me 应可用");

    // 无痕迹验证：管理员进入他人账套后，账套内不应出现管理员的账号行
    let db = findb::Db::open(books_dir.join(format!("{key}.fbk"))).expect("打开账套文件失败");
    assert!(
        findb::users::get(&db, "boss").unwrap().is_none(),
        "平台管理员不应在他人账套留下账号记录"
    );
}

#[tokio::test]
async fn platform_user_lifecycle() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    // 开通 → 停用 → 重置口令
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "emp", "display_name": "员工", "password": "Emp123456" }),
        ))
        .await
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users/emp/reset-password",
            &admin_sid,
            serde_json::json!({ "new": "Emp999999" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重置口令应成功");
    // 重置后旧口令不可用、新口令可登录
    let (status, _) = login(&state, "emp", "Emp123456").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "旧口令应失效");
    // 重置口令清除了 must_change_pwd，可直接登录
    let (status, sid) = login(&state, "emp", "Emp999999").await;
    assert_eq!(status, StatusCode::OK, "新口令应可登录");
    // 删除自己应被拒绝
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/boss")
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "删除自己应被拒绝");
    drop(sid);
}

#[tokio::test]
async fn deleting_book_unblocks_user_deletion() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "zhang", "display_name": "张会计", "password": "Zhang123456" }),
        ))
        .await
        .unwrap();
    let (_, sid) = login(&state, "zhang", "Zhang123456").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    // 自建账套
    let (_, body) = create_book(&state, &sid, "张记贸易").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    // 有账套时不能删账号
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/zhang")
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "有账套时应拒绝删除账号");

    // 归属者删除自己的账套
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&format!("/api/books/{}", key))
                .header(header::COOKIE, &sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "归属者应能删除自己的账套");
    // 账套应已从列表消失
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"books\":[]"), "删除后列表应为空：{s}");
    // 此时账号可删
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/zhang")
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "账套清空后应能删除账号");
}

#[tokio::test]
async fn last_admin_cannot_be_removed() {
    let (state, _bd, _dir) = test_state();
    // 开通第二个管理员，用它来删 boss（boss 不是其当前会话账号）
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid,
            serde_json::json!({ "username": "m2", "display_name": "管理员二", "password": "M2aa123456", "is_admin": true }),
        ))
        .await
        .unwrap();
    let (_, m2_sid) = login(&state, "m2", "M2aa123456").await;
    // 首登强制改密
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &m2_sid,
            serde_json::json!({ "old": "M2aa123456", "new": "M2aa654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "m2 改密应成功");
    // 平台管理员可删除他人账套（boss 名下有预置账套 b1，先清掉才能删账号）
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/books/b1")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "平台管理员应能删除他人账套");
    // 两个管理员时可删 boss
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/boss")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存在两个管理员时可删其一");
    // 只剩 m2 一个管理员时，删除自己应被拒绝
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/m2")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "最后一个管理员不可删除");
}

#[tokio::test]
async fn platform_device_binding_and_reset() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "emp", "display_name": "员工", "password": "Emp123456" }),
        ))
        .await
        .unwrap();

    // 第一台设备登录：绑定该设备（首登强制改密，先改密再测权限边界）
    let (status, s1) = login_with_device(&state, "emp", "Emp123456", "dev-A").await;
    assert_eq!(status, StatusCode::OK, "首台设备登录应成功并完成绑定");
    assert!(!s1.is_empty());
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &s1,
            serde_json::json!({ "old": "Emp123456", "new": "Emp654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 平台层「一人一机」：第二台设备登录被拒绝（403），需管理员重置
    let (status, _) = login_with_device(&state, "emp", "Emp654321", "dev-B").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "其它设备登录应被拒绝");

    // 非管理员不能重置设备
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users/emp/reset-device",
            &s1,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通用户不能重置设备");

    // 管理员重置设备绑定（同时踢掉该用户全部会话）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users/emp/reset-device",
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员重置设备应成功");
    // 旧会话已失效
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &s1))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "重置设备后旧会话应失效");

    // 重置后 dev-B 可登录
    let (status, _) = login_with_device(&state, "emp", "Emp654321", "dev-B").await;
    assert_eq!(status, StatusCode::OK, "重置设备后新设备应可登录");
}

#[tokio::test]
async fn member_revoke_takes_effect_immediately() {
    let (state, _bd, _dir) = test_state();
    // 开通 zhang（owner）与 acc1（成员）
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    for (u, pwd) in [("zhang", "Zhang123456"), ("acc1", "Acc1123456")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &admin_sid,
                serde_json::json!({ "username": u, "display_name": u, "password": pwd }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通 {u} 应成功");
    }

    // zhang 首登改密 → 建账套 → 进入
    let (status, zhang_sid) = login(&state, "zhang", "Zhang123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &zhang_sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (_, body) = create_book(&state, &zhang_sid, "张记撤销").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    assert_eq!(select_book(&state, &zhang_sid, &key).await, StatusCode::OK);

    // 邀请 acc1（会计，不强制改密）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &zhang_sid,
            serde_json::json!({
                "username": "acc1", "display_name": "小会", "password": "Acc1123456",
                "role": "accountant", "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // acc1 登录改密进入账套
    let (status, acc_sid) = login(&state, "acc1", "Acc1123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &acc_sid,
            serde_json::json!({ "old": "Acc1123456", "new": "Acc1654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(select_book(&state, &acc_sid, &key).await, StatusCode::OK);

    // 归属者不可被停用/修改账套内身份（否则账套失去主人）
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/zhang",
            &zhang_sid,
            serde_json::json!({ "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "归属者不可被停用");

    // owner 停用成员 → 成员现有会话立即下线
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/acc1",
            &zhang_sid,
            serde_json::json!({ "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "停用成员应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &acc_sid))
        .await
        .unwrap();
    // 停用即移除其全部会话（remove_by_username），下一次请求在会话层就被拒绝；
    // 即使会话残留，身份对账后的 disabled 复核也会 403 兜底
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "被停用成员的会话应立即下线");

    // owner 移除成员 → 成员重新登录也进不了账套（平台账号本身不受影响）
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/users/acc1")
                .header(header::COOKIE, &zhang_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "移除成员应成功");
    let (status, acc_sid2) = login(&state, "acc1", "Acc1654321").await;
    assert_eq!(status, StatusCode::OK, "平台账号本身仍可登录");
    let status = select_book(&state, &acc_sid2, &key).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "被移除的成员不再能进入账套");
}

#[tokio::test]
async fn book_member_collaboration() {
    let (state, _bd, _dir) = test_state();
    // 1) 管理员开通平台账号：zhang（账套归属者）与 acc1（被邀请成员）
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    for (u, name, pwd) in [
        ("zhang", "张会计", "Zhang123456"),
        ("acc1", "小会", "Acc1123456"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &admin_sid,
                serde_json::json!({ "username": u, "display_name": name, "password": pwd }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {u} 应成功");
    }

    // 2) zhang 首登改密 → 自建账套 → 进入
    let (status, zhang_sid) = login(&state, "zhang", "Zhang123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &zhang_sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (_, body) = create_book(&state, &zhang_sid, "张记贸易").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    let status = select_book(&state, &zhang_sid, &key).await;
    assert_eq!(status, StatusCode::OK, "归属者应能进入自己的账套");

    // 3) 归属者邀请 acc1 进入账套（账套内建「会计」角色，不强制改密）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &zhang_sid,
            serde_json::json!({
                "username": "acc1",
                "display_name": "小会",
                "password": "Acc1123456",
                "role": "accountant",
                "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请成员应成功");
    // 未开通平台账号的子账号应被拒绝（避免产生无法登录的死账号）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &zhang_sid,
            serde_json::json!({
                "username": "ghost",
                "display_name": "幽灵",
                "password": "Ghost123456",
                "role": "accountant",
                "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "无平台账号的子账号应被拒绝"
    );

    // 4) acc1 平台登录 → 改密 → 进入 zhang 的账套
    let (status, acc_sid) = login(&state, "acc1", "Acc1123456").await;
    assert_eq!(status, StatusCode::OK, "成员平台登录应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &acc_sid,
            serde_json::json!({ "old": "Acc1123456", "new": "Acc1654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let status = select_book(&state, &acc_sid, &key).await;
    assert_eq!(status, StatusCode::OK, "被邀请成员应能进入账套");

    // 5) 成员身份：非管理员、会计角色、无用户管理权限
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &acc_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"is_admin\":false"), "成员应为非管理员：{s}");
    assert!(s.contains("\"role\":\"accountant\""), "成员角色应为会计：{s}");
    assert!(!s.contains("\"user_manage\""), "成员不应有用户管理权限：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/platform/users", &acc_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "成员不应能管平台账号");
}

// ---------------------------------------------------------------------------
// 业务回归（导入）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn import_run_kingdee_template_csv() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let status = select_book(&state, &sid, "b1").await;
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
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let _ = select_book(&state, &sid, "b1").await;
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

// ===========================================================================
// 功能对齐 finui：科目 / 期初 / 日志 / 备份 / 参数 / 模板 / 档案 / 工资 / 报销
// ===========================================================================

/// 带 sid 的 DELETE 请求
fn authed_delete(uri: &str, sid: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::COOKIE, sid)
        .body(Body::empty())
        .unwrap()
}

/// boss 登录并进入预置账套 b1
async fn boss_in_b1(state: &Arc<WebState>) -> String {
    let (_, sid) = login(state, "boss", "Admin!2026").await;
    assert_eq!(select_book(state, &sid, "b1").await, StatusCode::OK);
    sid
}

/// 录一张借贷 100 元的两行凭证（借 debit_acc / 贷 1001）
async fn post_simple_voucher(state: &Arc<WebState>, sid: &str, debit_acc: &str) -> StatusCode {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-31", "word": "记",
                "no": 99, "attachments": 0, "memo": "测试凭证",
                "entries": [
                    { "line": 1, "account_code": debit_acc, "summary": "购入", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "购入", "debit": "0", "credit": "100" }
                ],
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    if !status.is_success() {
        let body = body_string(resp).await;
        panic!("录凭证失败 {status}：{body}");
    }
    status
}

#[tokio::test]
async fn accounts_crud_usage_guard_and_begin() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新增科目（带辅助核算维度）
    let acc = |name: &str| {
        serde_json::json!({
            "account": {
                "code": "8888", "name": name, "category": "asset", "dir": "debit",
                "aux": 0, "unit": null, "currency": null, "has_qty": false,
                "is_cash": false, "is_bank": false, "cash_flow_item": null,
                "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
            },
            "aux_kinds": ["item", "supplier"]
        })
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, acc("原材料")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增科目应成功");

    // 重复编码 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, acc("重复")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复科目应拒绝");

    // 列表包含新科目，掩码 = item(32) | supplier(2) = 34
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/accounts", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("8888"), "科目列表应含 8888：{s}");
    let list: serde_json::Value = serde_json::from_str(&s).unwrap();
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["code"] == "8888")
        .expect("科目 8888 应存在");
    assert_eq!(row["aux"], 34, "辅助核算掩码应为 34");

    // 修改名称
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/accounts", &sid, acc("原材料改良")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 未使用 → 可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/accounts/8888", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "未使用科目应可删除");

    // 建一个无辅助核算的科目，被凭证引用后 → 删除被拦截
    let plain = serde_json::json!({
        "account": {
            "code": "1409", "name": "测试物料", "category": "asset", "dir": "debit",
            "aux": 0, "unit": null, "currency": null, "has_qty": false,
            "is_cash": false, "is_bank": false, "cash_flow_item": null,
            "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
        },
        "aux_kinds": []
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, plain))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(post_simple_voucher(&state, &sid, "1409").await, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/accounts/1409", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已用科目不可删除");

    // 期初录入 + 回读（贷方应为负数）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "1000", "ac": "0", "qty": null },
                { "account_code": "2001", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let rows: serde_json::Value = serde_json::from_str(&s).unwrap();
    let arr = rows.as_array().unwrap();
    let cash = arr.iter().find(|r| r["account_code"] == "1001").unwrap();
    let loan = arr.iter().find(|r| r["account_code"] == "2001").unwrap();
    assert_eq!(
        cash["year_begin"].as_str().unwrap().parse::<f64>().unwrap(),
        5000.0,
        "借方期初应为正"
    );
    assert_eq!(
        loan["year_begin"].as_str().unwrap().parse::<f64>().unwrap(),
        -3000.0,
        "贷方期初应为负"
    );

    // 操作日志可查且非空（含科目操作记录）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/logs?limit=50&q=%E7%A7%91%E7%9B%AE", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let logs: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(!logs.as_array().unwrap().is_empty(), "日志搜索应非空：{s}");

    // 备份 → 列表落地 → 恢复
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/backups", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "备份应成功");
    let s = body_string(resp).await;
    let name: String = serde_json::from_str::<serde_json::Value>(&s).unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains(&name), "备份列表应包含 {name}：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/restore", &sid, serde_json::json!({ "file": name })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "恢复应成功");
    // 路径穿越必须被拒绝（file_name 归一化后落入 backups 内且不存在 → 404）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/restore",
            &sid,
            serde_json::json!({ "file": "../../etc/passwd" }),
        ))
        .await
        .unwrap();
    assert!(
        !resp.status().is_success(),
        "路径穿越必须被拒绝：{}",
        resp.status()
    );

    // 账套参数读写
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let mut opts: serde_json::Value = serde_json::from_str(&s).unwrap();
    opts["company"] = serde_json::json!("改名公司");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("改名公司"), "参数应已更新：{s}");
}

#[tokio::test]
async fn templates_and_aux_lifecycle() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 模板 CRUD
    let tpl = serde_json::json!({
        "id": 0, "name": "月度计提房租", "memo": "每月房租",
        "entries": [
            { "summary": "计提房租", "account_code": "660201", "dir": "debit", "amount": "5000", "aux": {} },
            { "summary": "计提房租", "account_code": "221101", "dir": "credit", "amount": "5000", "aux": {} }
        ],
        "freq": "monthly", "start_period": 202601, "end_period": null,
        "last_period": null, "active": true
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/templates", &sid, tpl.clone()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建模板应成功");
    let id: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 到期列表应含月度模板
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/templates/due?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("月度计提房租"), "本期到期应包含模板：{s}");

    // 生成凭证 → 回写 last_period，本期到期列表不再包含
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/templates/{id}/generate"),
            &sid,
            serde_json::json!({ "period": 202601, "date": "2026-01-31" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "模板生成凭证应成功");
    let s = body_string(resp).await;
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&s).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0, "应返回凭证 id：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/templates/due?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(
        !s.contains("月度计提房租"),
        "生成后本期到期列表应移除该模板（last_period 已推进）：{s}"
    );

    // 修改
    let mut tpl2 = tpl.clone();
    tpl2["name"] = serde_json::json!("月度计提房租(改)");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(&format!("/api/templates/{id}"), &sid, tpl2))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/templates/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 辅助档案 CRUD
    let ent = serde_json::json!({
        "id": 0, "kind": "customer", "code": "C001", "name": "客户一",
        "parent_code": null, "disabled": false, "props": {}, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/aux", &sid, ent))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建档案应成功");
    let aid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/aux?kind=customer", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("C001"), "客户档案应存在：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/aux/{aid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn payroll_lifecycle_with_tax_and_vouchers() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 前置：建职员档案
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "employee", "code": "E001", "name": "张三",
                "parent_code": null, "disabled": false, "props": {}, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 录工资（后端算税）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E001", "dept": "销售部", "gross": "10000",
                "social": "800", "housing": "500", "deduction": "0",
                "additional": "1000", "social_co": "2000", "housing_co": "500", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录工资应成功");
    let s = body_string(resp).await;
    let row: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(row["tax_base"].as_str().unwrap().parse::<f64>().unwrap() > 0.0, "计税基数应为正：{s}");
    let net: f64 = row["net"].as_str().unwrap().parse().unwrap();
    assert!(net > 0.0 && net < 10000.0, "实发应在 0 与应发之间：{net}");

    // 列表回读
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll?period=202601", &sid))
        .await
        .unwrap();
    assert!(body_string(resp).await.contains("E001"), "工资表应含 E001");

    // 累计（202602 的 ytd 应含 202601 的 10000）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll/ytd?period=202602&employee=E001", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"income\":\"10000"), "累计收入应为 10000：{s}");

    // 计提凭证 → 工资行挂上凭证 → 删除被拦截
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/accrue?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "expense": "660201", "wage_payable": "221101",
                "social_payable": "221103", "housing_payable": "221104"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提凭证应成功");
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0, "应返回凭证 id");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains(&vid.to_string()), "工资行应挂上凭证 id：{s}");
    let pid: i64 = {
        let rows: serde_json::Value = serde_json::from_str(&s).unwrap();
        rows[0]["id"].as_i64().unwrap()
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/payroll/{pid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已挂凭证的工资行不可删除");

    // 发放凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222103", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发放凭证应成功");

    // 社保缴纳凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/social-pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "social_payable": "221103", "housing_payable": "221104",
                "personal_payable": "2241", "bank_account": "100201"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "社保缴纳凭证应成功");
}

#[tokio::test]
async fn claim_lifecycle_to_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新增草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claims",
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-15", "applicant": "张三",
                "dept": "销售部", "reason": "差旅费", "amount": "500",
                "items": [ { "expense_account": "660201", "amount": "500", "memo": "机票" } ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建报销单应成功");
    let s = body_string(resp).await;
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let cid = v["id"].as_i64().unwrap();
    assert_eq!(v["no"], "BX202601-001", "单据号应自增：{s}");

    // 草稿期可改
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/claims/{cid}"),
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-16", "applicant": "张三",
                "dept": "销售部", "reason": "差旅费(改)", "amount": "600",
                "items": [ { "expense_account": "660201", "amount": "600", "memo": "机票" } ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿应可修改");

    // 提交 → 审批 → 支付
    for st in ["submitted", "approved", "paid"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/claims/{cid}/transition"),
                &sid,
                serde_json::json!({ "status": st }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "流转到 {st} 应成功");
    }

    // 支付后不可再改内容
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/claims/{cid}"),
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-16", "applicant": "张三",
                "dept": "销售部", "reason": "再改", "amount": "600", "items": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已支付单据不可修改");

    // 生成凭证（明细合计与金额一致）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/claims/{cid}/voucher"),
            &sid,
            serde_json::json!({ "pay_account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "生成凭证应成功");
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0);

    // 重复生成 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/claims/{cid}/voucher"),
            &sid,
            serde_json::json!({ "pay_account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复生成凭证应被拦截");

    // 已生成凭证 → 删除被拦截
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/claims/{cid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已挂凭证的报销单不可删除");

    // 列表状态过滤
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=paid", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("BX202601-001"), "已付列表应含该单：{s}");
}

#[tokio::test]
async fn login_rate_limited_after_repeated_failures() {
    let (state, _bd, _dir) = test_state();
    // 连续失败 10 次（阈值内），每次都应是 401
    for i in 0..10 {
        let (status, _sid) = login(&state, "boss", "WrongPass!").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "第 {} 次失败应 401", i + 1);
    }
    // 第 11 次即使密码正确，也被限流 → 429 + Retry-After
    let resp = handlers::router(state.clone())
        .oneshot(post_json(
            "/api/login",
            serde_json::json!({
                "username": "boss",
                "password": "Admin!2026",
                "device_id": "dev-test-0001",
                "device_name": "测试机",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS, "超过阈值应 429");
    assert!(
        resp.headers().contains_key(header::RETRY_AFTER),
        "429 响应应带 Retry-After 头"
    );
    let s = body_string(resp).await;
    assert!(s.contains("retry_after"), "响应体应含剩余等待秒数：{s}");
}

/// 越权回归：只读（Viewer）与出纳不得写入这些端点。
///
/// 历史上预算版本 / 审批 / 报表附注 / 档案的写路由只用只读权限 Perm::Report 把关，
/// 而 Report 是每个角色（含 Viewer）都自带的最低权限，等于对只读账号开放了写入；
/// 模板 / 工资 / 报销的 DELETE 又误用了 Perm::VoucherNew，使无 VoucherDelete
/// 的出纳也能删。
#[tokio::test]
async fn readonly_roles_cannot_write() {
    let (state, _bd, _dir) = test_state();
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;

    // 平台账号（首登强制改密）
    for u in ["view1", "cash1"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss_sid,
                serde_json::json!({ "username": u, "display_name": u, "password": "Init123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {u}");
    }
    // boss 进入名下账套 b1，把两人拉成 Viewer / 出纳
    assert_eq!(
        select_book(&state, &boss_sid, "b1").await,
        StatusCode::OK,
        "boss 应能进入 b1"
    );
    for (u, role) in [("view1", "viewer"), ("cash1", "cashier")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss_sid,
                serde_json::json!({
                    "username": u, "display_name": u, "password": "Init123456",
                    "role": role, "must_change_pwd": false,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {u} 为 {role}");
    }

    let mut sids = Vec::new();
    for u in ["view1", "cash1"] {
        let (st, sid) = login(&state, u, "Init123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 平台登录");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Init123456", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 首登改密");
        let (st, sid) = login(&state, u, "Pass123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 改密后重登");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 应能进入 b1"
        );
        sids.push(sid);
    }
    let view_sid = sids[0].clone();
    let cash_sid = sids[1].clone();

    // Viewer 读权限仍在
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/budget/versions", &view_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "Viewer 应能读预算版本");

    // 但所有写入口一律 403
    for (uri, body) in [
        (
            "/api/budget/versions",
            serde_json::json!({ "key": "v1", "name": "回归" }),
        ),
        (
            "/api/approvals",
            serde_json::json!({ "biz_kind": "purchase", "biz_id": 1, "title": "t", "approvers": ["boss"] }),
        ),
        (
            "/api/reports/notes",
            serde_json::json!({ "report_key": "balance-sheet", "period": 202601, "content": "x" }),
        ),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(uri, &view_sid, body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "Viewer 写 {uri} 应被拒");
    }

    // 出纳无 VoucherDelete，不能删凭证模板
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/templates/1", &cash_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "出纳删凭证模板应被拒");
}

/// 通过 extra_perms 拿到 UserManage 的普通用户，不能改自己的角色 / 权限矩阵 /
/// 数据范围（那等于一步自我提权）；改他人的授权仍然放行。
#[tokio::test]
async fn user_manager_cannot_escalate_self() {
    let (state, _bd, _dir) = test_state();
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;

    for u in ["sup1", "acc9"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss_sid,
                serde_json::json!({ "username": u, "display_name": u, "password": "Init123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {u}");
    }
    assert_eq!(select_book(&state, &boss_sid, "b1").await, StatusCode::OK);
    for (u, role, extra) in [
        ("sup1", "accountant", vec!["user_manage"]),
        ("acc9", "accountant", vec![]),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss_sid,
                serde_json::json!({
                    "username": u, "display_name": u, "password": "Init123456",
                    "role": role, "must_change_pwd": false, "extra_perms": extra,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {u}");
    }

    let mut sid_sup = String::new();
    for u in ["sup1", "acc9"] {
        let (st, sid) = login(&state, u, "Init123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 登录");
        // 平台账号建号时强制首登改密，不改密会被拦在账套之外
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Init123456", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 首登改密");
        let (st, sid) = login(&state, u, "Pass123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 改密后重登");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 进账套"
        );
        if u == "sup1" {
            sid_sup = sid;
        }
    }

    // 给自己加授权：三种载体都要被拦
    for body in [
        serde_json::json!({ "role": "admin" }),
        serde_json::json!({ "extra_perms": ["user_manage", "backup"] }),
        serde_json::json!({ "deny_perms": [] }),
        serde_json::json!({ "data_scope": {} }),
        serde_json::json!({ "disabled": true }),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_put("/api/users/sup1", &sid_sup, body.clone()))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "自我提权应被拒，body={body}"
        );
    }

    // 改自己的显示名/备注不涉及授权，放行
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/sup1",
            &sid_sup,
            serde_json::json!({ "display_name": "我自己改的" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "自助改显示名不该被拦");

    // 改他人授权仍然可以：说明拦的是「改自己」而不是把 UserManage 削没了
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/acc9",
            &sid_sup,
            serde_json::json!({ "role": "viewer" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改他人角色应放行");
    let v: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).expect("响应应是 JSON");
    assert_eq!(v["ok"], serde_json::json!(true));
}

// ---------------------------------------------------------------------------
// 回归：租户边界（2026-09 审查修复）
// ---------------------------------------------------------------------------

/// 平台管理员开通一个非管理员账号，完成首登改密后返回可用的 sid（尚未选账套）
async fn provision_plain_user(
    state: &Arc<WebState>,
    admin_sid: &str,
    username: &str,
    init_pwd: &str,
) -> String {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            admin_sid,
            serde_json::json!({ "username": username, "display_name": username, "password": init_pwd }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {username} 应成功");
    let (st, sid) = login(state, username, init_pwd).await;
    assert_eq!(st, StatusCode::OK, "{username} 首次登录应成功");
    let new_pwd = format!("{init_pwd}x");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": init_pwd, "new": new_pwd }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{username} 首登改密应成功");
    sid
}

/// 账套管理员不能把平台管理员拉进自己的账套（否则可用账套内重置口令
/// 重置他人的平台口令，形成提权链）
#[tokio::test]
async fn cannot_invite_platform_admin_into_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let zhang = provision_plain_user(&state, &admin_sid, "zhangx", "Zx12345678").await;
    let (status, body) = create_book(&state, &zhang, "张记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &zhang, &key).await, StatusCode::OK);

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &zhang,
            serde_json::json!({
                "username": "boss", "display_name": "平台管理员", "password": "Boss123456",
                "role": "accountant", "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "不应允许邀请平台管理员进账套");
}

/// 备份目录全局共享，但 list/restore 必须按账套隔离，不能跨租户读取/覆盖
#[tokio::test]
async fn backups_isolated_per_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let zhang = provision_plain_user(&state, &admin_sid, "zhangy", "Zy12345678").await;
    let li = provision_plain_user(&state, &admin_sid, "liy", "Ly12345678").await;

    let (status, body) = create_book(&state, &zhang, "张记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let zk = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &zhang, &zk).await, StatusCode::OK);

    let (status, body) = create_book(&state, &li, "李记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lk = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &li, &lk).await, StatusCode::OK);

    // 张备份后拿到文件名
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/backups", &zhang, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "备份应成功");
    let v: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).expect("备份响应应是 JSON");
    let zname = v["name"].as_str().expect("应返回备份文件名").to_string();

    // 李的备份列表里不能出现张的备份
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &li))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_string(resp).await;
    assert!(!list.contains(&zname), "不应看到其他账套的备份：{list}");

    // 李恢复张的备份必须被拒（不是 404，而是明确属于别的账套）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/restore", &li, serde_json::json!({ "file": zname })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不应允许恢复其他账套的备份");

    // 自己的备份仍可见（确认过滤没有把范围清空）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &zhang))
        .await
        .unwrap();
    let own = body_string(resp).await;
    assert!(own.contains(&zname), "自己的备份应可见：{own}");
}

/// 跨租户口令接管回归：账套管理员不能重置「其他账套归属者」的平台口令。
///
/// 历史漏洞：任意自建账套的用户（天然拥有 UserManage）可把受害者平台账号
/// 拉进自己的账套，再调 `/api/users/:username/reset-password` 改写其全局口令，
/// 而该口令会被同步覆盖到受害者名下所有账套 → 跨租户锁定/接管。
#[tokio::test]
async fn book_admin_cannot_reset_global_password_of_other_book_owner() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let attacker = provision_plain_user(&state, &admin_sid, "att1", "At12345678").await;
    let victim = provision_plain_user(&state, &admin_sid, "vic1", "Vc12345678").await;

    // 双方各自建账套（此时双方都是自己账套的 Admin）
    let (st, body) = create_book(&state, &attacker, "攻击者账套").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let akey = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &attacker, &akey).await, StatusCode::OK);

    let (st, body) = create_book(&state, &victim, "受害者账套").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let vkey = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &victim, &vkey).await, StatusCode::OK);

    // 攻击者把受害者账号拉进自己的账套（合法成员邀请，但不应借此重置全局口令）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &attacker,
            serde_json::json!({
                "username": "vic1", "display_name": "受害者", "password": "Init123456",
                "role": "accountant", "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请成员本身应成功");

    // 重置全局口令必须被拒（受害者拥有自己的账套）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users/vic1/reset-password",
            &attacker,
            serde_json::json!({ "new": "Hacked123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "账套管理员不得重置其他账套归属者的平台口令"
    );

    // 受害者原口令仍可登录（未被改写）
    let (st, _) = login(&state, "vic1", "Vc12345678x").await;
    assert_eq!(st, StatusCode::OK, "受害者口令不应被跨租户重置");
}

// ---------------------------------------------------------------------------
// 回归：Web 与桌面能力对齐（辅助核算/数量/期末处理）
// ---------------------------------------------------------------------------

/// Web 凭证支持辅助核算/数量/单价，且编辑时不丢原行未提交的要素。
#[tokio::test]
async fn web_voucher_aux_qty_roundtrip_and_preserve() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 140301 原材料（存货辅助 + 数量核算）100 = 5 × 20；贷 2001 短期借款 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": 1, "attachments": 0, "memo": "带辅助数量的凭证",
                "entries": [
                    {
                        "line": 1, "account_code": "140301", "summary": "采购原料",
                        "debit": "100", "credit": "0",
                        "aux": { "item": "RM01" }, "qty": "5", "price": "20"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "带辅助/数量的凭证应可保存");
    let body = body_string(resp).await;
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .expect("应返回凭证 id");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).unwrap();
    let e0 = &v["entries"][0];
    assert_eq!(e0["aux"]["item"], serde_json::json!("RM01"), "存货辅助应落库");
    assert_eq!(e0["qty"], serde_json::json!("5"), "数量应落库");
    assert_eq!(e0["price"], serde_json::json!("20"), "单价应落库");

    // 模拟旧版前端：更新时不提交 aux/qty/price，原行要素必须保留
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": id, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": 1, "attachments": 0, "memo": "旧前端编辑",
                "entries": [
                    { "line": 1, "account_code": "140301", "summary": "采购原料", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "旧前端编辑应兼容");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["entries"][0]["aux"]["item"],
        serde_json::json!("RM01"),
        "未提交的辅助核算不能被清空"
    );
    assert_eq!(v["entries"][0]["qty"], serde_json::json!("5"), "未提交的数量不能被清空");
}

/// Web 期末处理：结转损益 → 记账结转凭证 → 结账 → 反结账。
#[tokio::test]
async fn web_period_carry_close_unclose_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 6401 主营业务成本 100 / 贷 1001 库存现金 100（产生损益发生额）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20", "word": "记",
                "no": 1, "attachments": 0, "memo": "结转测试",
                "entries": [
                    { "line": 1, "account_code": "6401", "summary": "成本", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "付款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/post"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "业务凭证应能记账");

    // 结账前预检：应提示需要先结转损益（无未记账凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods/202601/precheck", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let chk: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(chk["pl_count"].as_i64().unwrap_or(0) > 0, "应有损益科目可结转");

    // 结转损益 → 生成一张待记账的结转凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/periods/202601/carry-forward", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结转损益应成功");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{cid}/post"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结转凭证应能记账");

    // 结账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/close",
            &sid,
            serde_json::json!({ "require_carry": true }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "结转并记账后应能结账：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods", &sid))
        .await
        .unwrap();
    let periods: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(periods["closed_upto"], serde_json::json!("2026-01"), "结账线应推进");

    // 反结账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/periods/202601/unclose", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "反结账应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods", &sid))
        .await
        .unwrap();
    let periods: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(periods["closed_upto"].is_null(), "反结账后不应有结账线");
}
