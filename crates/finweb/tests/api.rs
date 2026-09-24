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

/// M-9：未登录探测不能区分"接口是否存在 / 方法是否注册"（401/404/405 统一 401）
#[tokio::test]
async fn m9_unauthenticated_probe_uniform_401() {
    let (state, _bd, _dir) = test_state();

    // 真实接口未登录 → 401（既有行为，取其响应作为基准）
    let resp_me = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp_me.status(), StatusCode::UNAUTHORIZED);
    let body_me = body_string(resp_me).await;

    // 不存在的接口 → 401 且响应与真实接口逐字一致（而不是 404）
    let resp_fake = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/definitely-not-a-route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp_fake.status(),
        StatusCode::UNAUTHORIZED,
        "不存在的接口对未登录探测也应 401"
    );
    assert_eq!(body_string(resp_fake).await, body_me);

    // 存在但方法未注册 → 401（而不是 405）
    let resp_method = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method(axum::http::Method::PUT)
                .uri("/api/books")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp_method.status(),
        StatusCode::UNAUTHORIZED,
        "方法不匹配对未登录探测也应 401"
    );

    // 已登录并选定账套后：不存在的接口回 404（功能可见性对登录用户不隐藏）
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(select_book(&state, &sid, "b1").await, StatusCode::OK);
    let resp_authed = handlers::router(state.clone())
        .oneshot(authed_get("/api/definitely-not-a-route", &sid))
        .await
        .unwrap();
    assert_eq!(resp_authed.status(), StatusCode::NOT_FOUND);

    // 公开门露：健康检查无需会话
    let resp_health = handlers::router(state.clone())
        .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp_health.status(), StatusCode::OK);
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
                // 明细备注留空：生成凭证时应回退用事由做摘要，不能因摘要为空被拒
                "items": [ { "expense_account": "660201", "amount": "500", "memo": "" } ]
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

    // 支付即自动落账：已付列表应带凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=paid", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(
        !s.contains("\"voucher_id\":null"),
        "支付应自动生成付款凭证：{s}"
    );

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

    // 幂等：重复请求返回同一张凭证（支付时已自动出账，手动请求不再新增）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/claims/{cid}/voucher"),
            &sid,
            serde_json::json!({ "pay_account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重复请求应幂等返回");
    let vid2: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    assert_eq!(vid2, vid, "幂等返回同一张凭证");

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

    // 空状态 = 全部（UI 下拉默认空串），不能退化成只看草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("BX202601-001"), "空状态应返回全部单据：{s}");
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

/// 可选审核环节：启用后 未记账→已审核→已记账；未审核不能记账、已审核不能改/删。
#[tokio::test]
async fn web_optional_audit_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 启用审核环节（先读回 options 再改，避免覆盖其他字段）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).unwrap();
    opts["enable_audit"] = serde_json::json!(true);
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "启用审核环节应成功");

    // 保存一张凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "审核流",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未审核不能记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/post"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未审核不应允许记账");

    // 审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/audit"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");

    // 已审核不能直接修改
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": id, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "改一下",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "改", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "改", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已审核凭证不应允许修改");

    // 审核后可以记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/post"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核后应能记账");

    // 已记账不能反审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/unaudit"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已记账不应允许反审核");

    // 反记账 → 反审核 → 可修改
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/unpost"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/unaudit"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "反审核应成功");
}

/// Web 外币分录：币种/汇率/原币金额可存取（引擎校验 原币×汇率≈本位币）。
#[tokio::test]
async fn web_foreign_currency_entry_roundtrip() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新建一个美元科目（8888，不能与内置科目冲突）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/accounts",
            &sid,
            serde_json::json!({
                "account": {
                    "code": "8888", "name": "美元户", "category": "asset", "dir": "debit",
                    "aux": 0, "unit": null, "currency": "USD", "has_qty": false,
                    "is_cash": false, "is_bank": false, "cash_flow_item": null,
                    "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
                },
                "aux_kinds": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增美元科目应成功");

    // 借 8888 原币 100 × 7.2 = 720；贷 2001 720
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-18", "word": "记",
                "no": 1, "attachments": 0, "memo": "外币收款",
                "entries": [
                    {
                        "line": 1, "account_code": "8888", "summary": "美元收款",
                        "debit": "720", "credit": "0",
                        "currency": "USD", "rate": "7.2", "amount_for": "100"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "720" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "外币凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let e0 = &v["entries"][0];
    assert_eq!(e0["currency"], serde_json::json!("USD"), "币种应落库");
    assert_eq!(e0["rate"], serde_json::json!("7.2"), "汇率应落库");
    assert_eq!(e0["amount_for"], serde_json::json!("100.00"), "原币金额应落库");

    // 原币 × 汇率与金额不符时应拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-19", "word": "记",
                "no": 2, "attachments": 0, "memo": "错汇率",
                "entries": [
                    {
                        "line": 1, "account_code": "8888", "summary": "美元收款",
                        "debit": "700", "credit": "0",
                        "currency": "USD", "rate": "7.2", "amount_for": "100"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "700" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "原币×汇率不符应被拒");
}

/// Web 固定资产：建卡 → 计提折旧生成凭证 → 幂等 → 清理。
#[tokio::test]
async fn web_assets_depreciate_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets",
            &sid,
            serde_json::json!({
                "code": "GD0001", "name": "台式电脑", "category": "电子设备", "spec": "",
                "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
                "expense_account": "660201", "original_value": "12000",
                "residual_rate": "5", "life_months": 36, "method": "straight",
                "start_period": 202601, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增资产卡片应成功");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 计提 2026-01 折旧：12000 × 95% / 36 = 316.67
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/assets/depreciate", &sid, serde_json::json!({ "ymm": 202601 })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提折旧应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["already"], serde_json::json!(false));
    assert_eq!(r["total"], serde_json::json!("316.67"));
    let vid = r["voucher_id"].as_i64().expect("应返回折旧凭证 id");

    // 折旧凭证借贷平衡
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let sum = |k: &str| entries.iter().map(|e| e[k].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0)).sum::<f64>();
    assert!(
        (sum("debit") - sum("credit")).abs() < 0.005,
        "折旧凭证应借贷平衡"
    );

    // 幂等：再次计提不生成新凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/assets/depreciate", &sid, serde_json::json!({ "ymm": 202601 })))
        .await
        .unwrap();
    let r2: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r2["already"], serde_json::json!(true), "重复计提应幂等");
    assert_eq!(r2["voucher_id"].as_i64(), Some(vid), "幂等应返回原凭证");

    // 清理后再计提不再包含该资产
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{id}/dispose"),
            &sid,
            serde_json::json!({ "ymm": 202601, "amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资产清理应成功");
}

/// Web 银行对账：导入对账单 → 自动勾对 → 余额调节表平衡。
#[tokio::test]
async fn web_bank_reconcile_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 账面：借 100201 1000 / 贷 2001 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "银行收款",
                "entries": [
                    { "line": 1, "account_code": "100201", "summary": "收款", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/post"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 导入对账单（余额与账面一致，便于验证调节表勾稽）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/import",
            &sid,
            serde_json::json!({
                "ymm": 202601, "account": "100201",
                "text": "日期,摘要,结算号,借方,贷方,余额\n2026-01-10,收款,SN001,1000.00,0.00,1000.00\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入对账单应成功");
    let imp: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(imp["imported"], serde_json::json!(1));

    // 自动勾对
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/auto-match",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201", "tolerance": 31 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let m: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(m["matched"].as_u64().unwrap_or(0) >= 1, "应至少勾对一对：{m}");

    // 调节表：银行与账面调节后一致
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/bank?period=202601&account=100201", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(d["statements"][0]["entry_id"].as_i64(), Some(
        d["book"][0]["entry_id"].as_i64().unwrap()
    ), "对账单应挂到账面分录");
    assert_eq!(d["reconcile"]["balanced"], serde_json::json!(true), "调节表应平衡：{}", d["reconcile"]);
}

/// Web 往来核销：自动核销等额的一借一贷。
#[tokio::test]
async fn web_settle_auto_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 112201（客户 C01）1000 / 贷 1001 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let v1_body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "应收凭证应保存：{v1_body}");
    let v1 = serde_json::from_str::<serde_json::Value>(&v1_body).unwrap()["id"]
        .as_i64()
        .unwrap();
    // 收款：借 1001 / 贷 112201（客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20", "word": "记",
                "no": 2, "attachments": 0, "memo": "收款",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "112201", "summary": "核销", "debit": "0", "credit": "1000", "aux": { "customer": "C01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(&format!("/api/vouchers/{vid}/post"), &sid, serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "凭证 {vid} 应能记账");
    }

    // 核销前：两笔未核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/open?account=112201&upto=202601", &sid))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(open["rows"].as_array().unwrap().len(), 2, "核销前应有两笔未核销");

    // 自动核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/auto",
            &sid,
            serde_json::json!({ "account": "112201", "ymm": 202601, "tolerance": "0.01" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["pairs"], serde_json::json!(1), "应核销一对：{r}");

    // 核销后：无未核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/open?account=112201&upto=202601", &sid))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(open["rows"].as_array().unwrap().len(), 0, "核销后应无未核销");
}

/// Web 总账/日记账接口与 CSV 导出。
#[tokio::test]
async fn web_ledger_tabs_and_export() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-12", "word": "记",
                "no": 1, "attachments": 0, "memo": "账簿测试",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收现", "debit": "300", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "300" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{id}/post"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 总账
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/ledger/general?code=1001&from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let gl: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(gl.as_array().unwrap().len(), 1, "总账应按期间汇总一行：{gl}");
    assert_eq!(gl[0]["debit"], serde_json::json!("300.00"));

    // 日记账（对方科目）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/ledger/journal?code=1001&from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let jr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(jr.as_array().unwrap().len(), 1, "日记账应一行");
    assert!(
        jr[0]["opposite_accounts"].as_str().unwrap_or("").contains("短期借款"),
        "日记账应带对方科目：{jr}"
    );

    // 导出凭证 CSV
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/vouchers?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap().contains("text/csv"),
        "应返回 CSV"
    );
    let body = body_string(resp).await;
    assert!(body.contains("凭证号") && body.contains("库存现金"), "CSV 应含表头与科目名：{body}");

    // 导出明细账 CSV（含数量列与否均可）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/ledger?code=1001&from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("日期") && body.contains("余额"), "明细账 CSV 应含表头：{body}");
}

/// Web 凭证附件：multipart 上传 → 列表 → 下载 → 删除。
#[tokio::test]
async fn web_voucher_attachment_roundtrip() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-08", "word": "记",
                "no": 1, "attachments": 0, "memo": "附件测试",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "50", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "50" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // multipart 上传
    let boundary = "----finbooktest";
    let payload = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"receipt.txt\"\r\nContent-Type: text/plain\r\n\r\nhello attachment\r\n--{b}--\r\n",
        b = boundary
    );
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/vouchers/{id}/attachments"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::COOKIE, &sid)
        .body(Body::from(payload))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "上传附件应成功");
    let aid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 列表
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}/attachments"), &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "应有 1 个附件：{list}");
    assert_eq!(list[0]["name"], serde_json::json!("receipt.txt"));

    // 下载
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/attachments/{aid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "hello attachment");

    // 删除
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/attachments/{aid}"))
        .header(header::COOKIE, &sid)
        .body(Body::empty())
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除附件应成功");
}

/// Web 辅助账 / 数量金额账 / 自定义报表。
#[tokio::test]
async fn web_aux_qty_and_custom_reports() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收：借 112201 客户 C01 1000 / 贷 600101 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let v1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 存货：借 140301 存货 RM01 5×20=100 / 贷 1001 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-07", "word": "记",
                "no": 2, "attachments": 0, "memo": "入库",
                "entries": [
                    { "line": 1, "account_code": "140301", "summary": "入库", "debit": "100", "credit": "0", "aux": { "item": "RM01" }, "qty": "5", "price": "20" },
                    { "line": 2, "account_code": "1001", "summary": "付款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(&format!("/api/vouchers/{vid}/post"), &sid, serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // 辅助账（客户）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/aux-balance?kind=customer&from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(d["rows"][0]["key"], serde_json::json!("C01"), "辅助账应有客户 C01：{d}");
    assert_eq!(d["rows"][0]["debit"], serde_json::json!("1000.00"));

    // 数量金额账
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/qty-balance?from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let q: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = q["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account_code"] == serde_json::json!("140301"))
        .expect("数量金额账应含 140301");
    assert_eq!(row["qty_in"], serde_json::json!("5"), "入库数量应为 5：{row}");
    assert_eq!(row["qty_end"], serde_json::json!("5"), "期末数量应为 5");

    // 自定义报表：QM("1001") 期末余额
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/custom-reports",
            &sid,
            serde_json::json!({
                "key": "", "name": "资金小表",
                "columns": ["期末"],
                "lines": [{ "name": "库存现金", "indent": 0, "formulas": ["QM(\"1001\")"], "bold": false }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存自定义报表应成功");
    let saved: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(saved["errors"].as_array().unwrap().is_empty(), "公式应无语法错误：{saved}");
    let key = saved["key"].as_str().unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/custom-reports/{key}?period=202601"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(got["report"]["name"], serde_json::json!("资金小表"));
    let cell = got["values"][0][0].as_str().unwrap_or("");
    assert!(cell.contains("100.00"), "QM(\"1001\") 应算出 -100.00：{got}");
}

// ---------------------------------------------------------------------------
// 报表勾稽 / 凭证流转 / 导出 / 发票 / 银行手工勾对 / 核销手工 / 打印端点
// ---------------------------------------------------------------------------

fn money_num(s: &str) -> f64 {
    s.replace(',', "").parse::<f64>().unwrap_or(0.0)
}

/// 建一张已记账凭证（返回 id）
async fn post_voucher(
    state: &Arc<WebState>,
    sid: &str,
    no: i32,
    entries: serde_json::Value,
) -> i64 {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": no, "attachments": 0, "memo": "t",
                "entries": entries
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存凭证应成功");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账应成功");
    id
}

/// H-3 定案：试算平衡默认只含已记账——草稿凭证不入账，记账后进入。
#[tokio::test]
async fn trial_balance_default_posted_only_h3() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 只保存、不记账（草稿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "草稿口径",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "草稿", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "草稿", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 草稿状态：本期发生额不含它
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let d = money_num(tb["totals"]["debit"].as_str().unwrap());
    assert!((d - 0.0).abs() < 0.005, "H-3：草稿不应进试算平衡，本期借方 {d}：{tb}");

    // 记账后进入
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let d = money_num(tb["totals"]["debit"].as_str().unwrap());
    assert!((d - 100.0).abs() < 0.005, "H-3：记账后应进试算平衡，本期借方 {d}：{tb}");
}

/// 工资三类凭证回链：计提 / 社保缴纳 / 发放状态可见，且同类型重复生成被拦。
#[tokio::test]
async fn payroll_voucher_status_tracking() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 录入工资行（应发5000，个人社保200+公积金200，单位各500）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E01", "dept": "D01", "gross": "5000",
                "social": "200", "housing": "200", "deduction": "0",
                "additional": "0", "social_co": "500", "housing_co": "500", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录入工资行应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["id"].as_i64().unwrap() > 0);

    let get_rows = |sid: String| {
        let state = state.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_get("/api/payroll?period=202601", &sid))
                .await
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        }
    };

    // 计提凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/accrue?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "expense": "660201", "wage_payable": "221101",
                "social_payable": "221103", "housing_payable": "221104"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提凭证应成功");

    // 社保缴纳凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/social-pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "social_payable": "221103", "housing_payable": "221104",
                "personal_payable": "2241", "bank_account": "100201"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "社保缴纳凭证应成功");

    // 发放凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222107", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发放凭证应成功");

    // 三个回链都应写回工资行（响应为数组）
    let rows = get_rows(sid.clone()).await;
    let arr = rows.as_array().expect("工资列表应为数组");
    let row = &arr[0];
    assert!(!row["voucher_id"].is_null(), "计提凭证应回链：{rows}");
    assert!(!row["social_voucher_id"].is_null(), "社保凭证应回链：{rows}");
    assert!(!row["paid_voucher_id"].is_null(), "发放凭证应回链：{rows}");

    // 同类型重复生成被拦（同一期幂等/唯一约束）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222107", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复生成发放凭证应被拦");
}

/// 收付款单 API 流程：出凭证 + 自动核销 + 删除链（凭证先行）。
#[tokio::test]
async fn receipt_doc_api_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 先造一张应收挂账（客户辅助 C01）并记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 81, "attachments": 0, "memo": "挂账",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "挂账",
                      "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入",
                      "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "挂账凭证应可保存");
    let ar_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{ar_id}/post"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "挂账应记账");

    // 收款 600：出凭证 + 自动核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &sid,
            serde_json::json!({
                "date": "2026-01-10", "kind": "receipt", "fund_account": "100201",
                "party": "C01", "amount": "600", "memo": "回款"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增收款单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc_id = r["id"].as_i64().unwrap();
    let vid = r["voucher_id"].as_i64().expect("收款单应生成凭证");
    assert_eq!(r["settled"].as_i64().unwrap(), 1, "应自动核销 1 笔");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "100201");
    assert_eq!(v["entries"][1]["account_code"], "112201");
    assert_eq!(v["entries"][1]["aux"]["customer"], "C01");

    // 列表可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/receipts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1);

    // 删除链：凭证存在 → 拒；删凭证 → 单据可删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/funds/receipts/{doc_id}/delete"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "凭证存在时不能删单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{vid}/delete"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可删除");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/funds/receipts/{doc_id}/delete"), &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "凭证删除后单据可删");
}

/// 对标金蝶流程：订单 CRUD + 状态流转 + 行金额服务端计算 + 客户信用卡控。
#[tokio::test]
async fn order_crud_and_credit_guard() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建销售订单：2 行（140301 50×12@13% → 不含税 600 / 税 78）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "customer_name": "客户甲", "status": "Draft", "memo": "",
                "lines": [
                    { "item_code": "140301", "qty_ordered": "50", "unit_price": "12", "tax_rate": "0.13" },
                    { "item_code": "140501", "qty_ordered": "2", "unit_price": "100", "tax_rate": "0.13" }
                ]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let body = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建销售订单应成功：{body}");
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = r["id"].as_i64().unwrap();
    assert_eq!(money_num(r["total_amount"].as_str().unwrap()), 800.0, "行金额服务端计算：50×12+2×100");
    assert_eq!(money_num(r["total_tax"].as_str().unwrap()), 104.0, "税额=金额×税率");

    // 列表可回读（含明细）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .expect("列表应含新订单");
    assert_eq!(row["lines"].as_array().unwrap().len(), 2);

    // 草稿 → 已确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap();
    assert_eq!(row["status"], "Confirmed");

    // 客户信用额度：超限确认被拒，额度内可确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "customer", "code": "C99", "name": "信用客户",
                "parent_code": null, "disabled": false,
                "props": { "credit_limit": "100" }, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建客户档案应成功");

    // 超限订单：草稿可存，确认被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-06", "customer_code": "C99",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "100", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿不受信用限制");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let over_id = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{over_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超信用额度应被拒");
    let s = body_string(resp).await;
    assert!(s.contains("信用额度不足"), "应提示信用额度不足：{s}");

    // 额度内订单：50×2=100 ≤100 → 确认成功
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-07", "customer_code": "C99",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "50", "unit_price": "2", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ok_id = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{ok_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "额度内订单应可确认");
    // 两笔占用合计 500+100 > 100 —— 再来一笔新的应被拒（占用按全部有效订单累计）
    // 注：超限单仍是草稿不计占用，占用=已确认的100；此处再确认原超限单 → 100+500=600>100 拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{over_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "累计占用超限仍应被拒");

    // 删除（草稿/已确认均可删，so_delete 无状态限制）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{ok_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除订单应成功");

    // 采购订单镜像：建单 + 确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-08", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "30", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pid = r["id"].as_i64().unwrap();
    assert_eq!(money_num(r["total_amount"].as_str().unwrap()), 270.0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{pid}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购订单确认应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{pid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 资金预算视图：当期现金/银行科目预算 vs 已记账实际（形态校验；口径见 findb 单测）
#[tokio::test]
async fn funds_budget_view_api() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/budget?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资金预算视图应 200");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["rows"].is_array(), "应返回 rows 数组：{r}");
}

/// 员工借支闭环：建单 → 支付出凭证 → 核销冲账出凭证；幂等与守卫。
#[tokio::test]
async fn advance_pay_settle_api_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建单（缺省编号自动生成）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/advances",
            &sid,
            serde_json::json!({
                "date": "2026-01-08", "employee": "张三", "purpose": "出差预借",
                "amount": "2000", "pay_account": "1001", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建单应成功");
    let aid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未支付不能核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/settle"),
            &sid,
            serde_json::json!({ "expense_account": "660201", "expense_amount": "500" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未支付不能核销");

    // 支付（默认今天）→ 凭证：借122105(员工) / 贷1001
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/pay"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "支付应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pvid = r["voucher_id"].as_i64().expect("支付应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{pvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "122105");
    assert!(v["entries"][0]["aux"]["employee"].as_str().unwrap().contains("张三"));
    assert_eq!(v["entries"][1]["account_code"], "1001");
    // 重复支付幂等
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/pay"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "重复支付不应重复出凭证：{r}");

    // 核销（冲账1500，退回500）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/settle"),
            &sid,
            serde_json::json!({ "expense_account": "660201", "expense_amount": "1500", "date": "2026-01-20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "核销应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let svid = r["voucher_id"].as_i64().expect("核销应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{svid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"].as_array().unwrap().len(), 3, "冲账+退回+贷员工，{v}");
    assert_eq!(v["entries"][0]["account_code"], "660201");
    // 已核销不可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已出凭证不可删除");
    // 员工/金额校验
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/advances",
            &sid,
            serde_json::json!({ "date": "2026-01-08", "employee": "", "amount": "100" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "借支人必填");
}

/// 出纳日清标记 + 支票登记簿：状态流转、查询与删除。
#[tokio::test]
async fn cashier_day_clear_and_checks() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 日清：标记 → 按期间查询 → 取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/day-clear",
            &sid,
            serde_json::json!({ "account_code": "1001", "date": "2026-01-10", "clear": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "标记日清应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/funds/day-clear?account=1001&from=2026-01-01&to=2026-01-31",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dates = r["dates"].as_array().unwrap();
    assert!(dates.iter().any(|d| d == "2026-01-10"), "应查到日清日期：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/day-clear",
            &sid,
            serde_json::json!({ "account_code": "1001", "date": "2026-01-10", "clear": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/funds/day-clear?account=1001&from=2026-01-01&to=2026-01-31",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["dates"].as_array().unwrap().is_empty(), "取消后应查不到：{r}");

    // 支票：新增 → 列表 → 作废 → 恢复 → 删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/checks",
            &sid,
            serde_json::json!({
                "no": "ZP100", "kind": "transfer", "bank_account": "100201",
                "payee": "供应商乙", "amount": "1200", "issued_date": "2026-01-16", "memo": "货款"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "登记支票应成功");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(money_num(rows[0]["amount"].as_str().unwrap()), 1200.0);
    assert_eq!(rows[0]["status"], "issued");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/status"),
            &sid,
            serde_json::json!({ "status": "void" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "作废应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"][0]["status"], "void");
    // 非法状态被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/status"),
            &sid,
            serde_json::json!({ "status": "bad" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["rows"].as_array().unwrap().is_empty());
}

/// 现金盘点：账面按资金日报（按日）口径快照 → 差异 → 盘盈盘亏凭证；幂等与守卫。
#[tokio::test]
async fn cash_count_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 现金 800 已记账（01-10）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 71, "attachments": 0, "memo": "盘点基数",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "800", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "800" }
                ]
            }),
        ))
        .await
        .unwrap();
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 盘点 01-12 实盘 850 → 账面 800、差异 +50
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/cash-counts",
            &sid,
            serde_json::json!({
                "date": "2026-01-12", "account_code": "1001", "counted": "850", "memo": "例行"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增盘点应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(r["book_amount"].as_str().unwrap()), 800.0, "账面快照");
    assert_eq!(money_num(r["diff"].as_str().unwrap()), 50.0, "差异 +50");
    let cid = r["id"].as_i64().unwrap();

    // 生成盘盈凭证：借1001 / 贷1901；幂等拒绝；挂凭证不可删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "盘盈凭证应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pvid = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{pvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "1001", "盘盈借现金");
    assert_eq!(v["entries"][1]["account_code"], "1901", "盘盈贷待处理损溢");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不可重复出凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已挂凭证不可删除");

    // 账实相符（实盘800）→ 差异0 → 凭证被拒、可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/cash-counts",
            &sid,
            serde_json::json!({ "date": "2026-01-13", "account_code": "1001", "counted": "800" }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(r["diff"].as_str().unwrap()), 0.0, "账实相符差异为0");
    let cid2 = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid2}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "相符不应出凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid2}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "相符记录可删除");
}

/// 资金日报（按日）：上日结余/本日收支/日末结存，仅已记账（H-3）。
#[tokio::test]
async fn funds_daily_by_date_report() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 01-10 现金凭证并记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 61, "attachments": 0, "memo": "日报取数",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "800", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "800" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账应成功");

    let fetch = |sid: String, date: &'static str| {
        let state = state.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_get(
                    &format!("/api/funds/daily-by-date?date={date}"),
                    &sid,
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "日报应 200");
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        }
    };

    // 前一日：无发生
    let r = fetch(sid.clone(), "2026-01-09").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .expect("应有 1001 行");
    assert_eq!(money_num(row["income"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 0.0);

    // 当日：收入 800、日末 800
    let r = fetch(sid.clone(), "2026-01-10").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .unwrap();
    assert_eq!(money_num(row["begin"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["income"].as_str().unwrap()), 800.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 800.0);

    // 次日：上日结余结转
    let r = fetch(sid.clone(), "2026-01-11").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .unwrap();
    assert_eq!(money_num(row["begin"].as_str().unwrap()), 800.0);
    assert_eq!(money_num(row["income"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 800.0);
}

/// 台账-总账联动：票据流转自动生成台账凭证；融资到账/结清出凭证；幂等与删除守卫。
#[tokio::test]
async fn funds_ledger_voucher_linkage() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收票据 → 贴现：流转即自动生成凭证（借 100201 / 贷 112101）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/bills",
            &sid,
            serde_json::json!({ "kind": "receivable", "no": "FL001", "period": 202601,
                "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "客户甲",
                "bank": "工行", "amount": "5000", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建票据应成功");
    let bid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/status"),
            &sid,
            serde_json::json!({ "status": "discounted", "date": "2026-01-15" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "贴现流转应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bvid = r["voucher_id"]
        .as_i64()
        .expect("资金流转应自动生成台账凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{bvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["status"], "draft", "台账凭证应为草稿（H-3 不入余额）");
    assert_eq!(v["entries"][0]["account_code"], "100201");
    assert_eq!(v["entries"][1]["account_code"], "112101");
    assert!(v["entries"][0]["debit"]
        .as_str()
        .unwrap()
        .starts_with("5000"));

    // 补出凭证 / 删除已挂凭证的票据 → 被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已出凭证应拒绝补出");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已挂凭证的票据不可删除");

    // 融资借款：到账凭证 + 结清自动生成还本凭证（幂等）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/loans",
            &sid,
            serde_json::json!({ "kind": "borrow", "no": "LN009", "bank": "工行",
                "principal": "10000", "rate_pct": "4.5", "start_date": "2026-01-01",
                "end_date": "2026-06-30", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建融资应成功");
    let lid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到账凭证应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dvid = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{dvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "100201", "到账：借银行");
    assert_eq!(v["entries"][1]["account_code"], "2001", "到账：贷短期借款");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "到账凭证不可重复生成");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结清应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let svid = r["voucher_id"]
        .as_i64()
        .expect("结清应自动生成还本凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{svid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "2001", "还本：借短期借款");
    assert_eq!(v["entries"][1]["account_code"], "100201", "还本：贷银行");
    // 结清幂等：重复结清不重复出凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-21" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "重复结清不应重复出凭证：{r}");
}

/// 出纳签字：签字/取消/幂等 + 权限（CashierSign），只读账号应被拒。
#[tokio::test]
async fn cashier_sign_and_unsign() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造一张现金凭证（草稿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 90, "attachments": 0, "memo": "签字流",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "付", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 签字 → 凭证详情带签字人；重复签字幂等
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重复签字应幂等");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["cashier"].as_str(),
        Some("boss"),
        "凭证详情应带签字人：{v}"
    );

    // 取消签字 → 清空
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unsign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(v["cashier"].is_null(), "取消后签字人应清空：{v}");

    // 只读账号（无 CashierSign）→ 403
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({
                "username": "nosign", "display_name": "nosign", "password": "Init123456"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通 nosign 平台账号");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "nosign", "display_name": "nosign", "password": "",
                "role": "viewer", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请 nosign 为 viewer");
    let (st, vsid) = login(&state, "nosign", "Init123456").await;
    assert_eq!(st, StatusCode::OK, "nosign 平台登录");
    // 平台账号默认首登强制改密；改密前只能访问改密/退出/登录（服务端拦截）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &vsid,
            serde_json::json!({ "old": "Init123456", "new": "Pass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "nosign 首登改密");
    assert_eq!(
        select_book(&state, &vsid, "b1").await,
        StatusCode::OK,
        "nosign 进入 b1"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &vsid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "只读账号无出纳签字权限"
    );
}

/// require_cashier：开启后现金/银行凭证须签字才能记账；非资金凭证不受限；
/// 签字人出现在出纳日记账。
#[tokio::test]
async fn require_cashier_gates_post_and_scopes_to_funds() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 开启出纳签字前置（先读回再改，避免覆盖其他字段）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).unwrap();
    opts["require_cashier"] = serde_json::json!(true);
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开启出纳签字前置应成功");

    // 现金凭证：未签字 → 记账被拒（提示含"出纳签字"）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-11", "word": "记",
                "no": 91, "attachments": 0, "memo": "现金待签字",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "付", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "未签字的现金凭证不应允许记账"
    );
    assert!(
        body_string(resp).await.contains("出纳签字"),
        "错误信息应提示出纳签字"
    );

    // 签字后可记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字后应能记账");

    // 出纳日记账应带签字人
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/ledger/journal?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        rows[0]["cashier"].as_str(),
        Some("boss"),
        "出纳日记账应显示签字人：{rows}"
    );

    // 非资金凭证（1901 待处理财产损溢 / 2001 短期借款）不受签字限制
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-12", "word": "记",
                "no": 92, "attachments": 0, "memo": "非资金不签字",
                "entries": [
                    { "line": 1, "account_code": "1901", "summary": "盘亏", "debit": "50", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "转", "debit": "0", "credit": "50" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "非资金凭证应可保存");
    let nid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{nid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "非资金凭证不受出纳签字限制"
    );
}

/// M-15 定案：借贷不平衡的凭证 Web 端必须 400 拒绝，错误信息说明借贷不平衡。
#[tokio::test]
async fn unbalanced_voucher_rejected_m15() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-11", "word": "记",
                "no": 1, "attachments": 0, "memo": "不平衡",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "99.99" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "借贷不平衡必须被拒");
    let body = body_string(resp).await;
    assert!(body.contains("借贷不平衡"), "{body}");
}

/// 三大报表勾稽：试算平衡、资产=负债+权益、利润表净利。
#[tokio::test]
async fn web_statements_tie() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "1001", "summary": "收", "debit": "1000", "credit": "0" },
        { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "1000" }
    ])).await;
    post_voucher(&state, &sid, 2, serde_json::json!([
        { "line": 1, "account_code": "660201", "summary": "费", "debit": "300", "credit": "0" },
        { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "300" }
    ])).await;

    // 试算平衡
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/trial-balance?from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ed = money_num(tb["totals"]["end_debit"].as_str().unwrap());
    let ec = money_num(tb["totals"]["end_credit"].as_str().unwrap());
    assert!((ed - ec).abs() < 0.005, "试算应平衡：{tb}");

    // 资产负债表：资产总计 == 负债和权益总计
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/balance-sheet?to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bs: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = bs["table"]["rows"].as_array().unwrap();
    let pick = |no: &str| {
        rows.iter()
            .find(|r| r["no"] == serde_json::json!(no))
            .and_then(|r| r["values"][0].as_str())
            .map(money_num)
            .unwrap_or(f64::NAN)
    };
    let asset = pick("17");
    let liab_eq = pick("36");
    assert!(!asset.is_nan() && !liab_eq.is_nan(), "应有资产/负债权益总计行：{bs}");
    assert!(
        (asset - liab_eq).abs() < 0.005,
        "资产总计 {asset} 应等于负债和权益总计 {liab_eq}"
    );
    assert!((asset - 700.0).abs() < 0.005, "资产总计应为 700：{asset}");

    // 利润表：净亏损 -300
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/income-statement?from=202601&to=202601", &sid))
        .await
        .unwrap();
    let is: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = is["table"]["rows"].as_array().unwrap();
    let net = rows
        .iter()
        .find(|r| r["no"] == serde_json::json!("15"))
        .and_then(|r| r["values"][0].as_str())
        .map(money_num)
        .unwrap();
    assert!((net + 300.0).abs() < 0.005, "净利润应为 -300：{net}");

    // 现金流量表可生成（未标注时全部未分配，但结构完整）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/cash-flow?from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 凭证流转：批量记账 / 红冲 / 删除 / 断号重排 / 取号。
#[tokio::test]
async fn web_voucher_state_ops() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 三张草稿
    let mut ids = Vec::new();
    for no in 1..=3 {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                    "no": no, "attachments": 0, "memo": "t",
                    "entries": [
                        { "line": 1, "account_code": "1001", "summary": "借", "debit": "10", "credit": "0" },
                        { "line": 2, "account_code": "2001", "summary": "贷", "debit": "0", "credit": "10" }
                    ]
                }),
            ))
            .await
            .unwrap();
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
                .as_i64()
                .unwrap(),
        );
    }

    // 批量记账前两张
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers/batch-post",
            &sid,
            serde_json::json!({ "ids": [ids[0], ids[1]] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], serde_json::json!(2), "应批量记账 2 张：{r}");

    // 红冲一张已记账凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{}/reverse", ids[0]),
            &sid,
            serde_json::json!({ "period": 202601, "date": "2026-01-16" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "红冲应成功");
    let rid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_ne!(rid, ids[0], "红冲应生成新凭证");

    // 删除第三张草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{}/delete", ids[2]),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除草稿应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{}", ids[2]), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "删除后应查不到");

    // 断号重排与取号
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers/renumber",
            &sid,
            serde_json::json!({ "period": 202601, "word": "记" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重排断号应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/vouchers/next-no?period=202601&word=记", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let n: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(n["no"].as_i64().unwrap_or(0) >= 1, "取号应返回正整数：{n}");
}

/// 发票 CRUD 与工资/报销 CSV 导出。
#[tokio::test]
async fn web_invoice_crud_and_exports() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 发票
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "in", "code": "044001", "number": "10001",
                "date": "2026-01-10", "buyer": "本公司", "seller": "供应商A",
                "amount_tax": "1130", "amount": "1000", "tax": "130",
                "tax_rate": "13", "status": "pending", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增发票应成功");
    let iid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?period=202601", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(list["rows"].as_array().unwrap().len(), 1);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/invoices/{iid}/status"),
            &sid,
            serde_json::json!({ "status": "verified" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发票认证应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices/summary", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发票汇总应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/invoices/{iid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除发票应成功");

    // 工资行 + 导出
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E001", "dept": "财务部", "gross": "10000",
                "social": "500", "housing": "300", "deduction": "0",
                "additional": "0", "social_co": "1000", "housing_co": "300", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存工资行应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/payroll?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("员工") && body.contains("E001"), "工资 CSV 应含员工：{body}");

    // 报销单 + 导出
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claims",
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-20", "applicant": "张三",
                "dept": "财务部", "reason": "差旅", "amount": "200",
                "items": [{ "expense_account": "660201", "amount": "200", "memo": "" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增报销单应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/claims?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("单号") && body.contains("张三"), "报销 CSV 应含申请人：{body}");
}

/// 银行手工勾对/取消/清空。
#[tokio::test]
async fn web_bank_manual_link_and_clear() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "100201", "summary": "收", "debit": "500", "credit": "0" },
        { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "500" }
    ])).await;

    // 对账单日期远离凭证日期，自动勾对容差 0 不命中
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/import",
            &sid,
            serde_json::json!({
                "ymm": 202601, "account": "100201",
                "text": "2026-01-25,收款,SN9,400.00,0.00,400.00\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/auto-match",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201", "tolerance": 0 }),
        ))
        .await
        .unwrap();
    let m: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(m["matched"], serde_json::json!(0), "金额不同不应自动勾对：{m}");

    // 手工勾对 → 取消 → 清空
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/bank?period=202601&account=100201", &sid))
        .await
        .unwrap();
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let stmt_id = d["statements"][0]["id"].as_i64().unwrap();
    let entry_id = d["book"][0]["entry_id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/link",
            &sid,
            serde_json::json!({ "stmt_id": stmt_id, "entry_id": entry_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "手工勾对应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/unlink",
            &sid,
            serde_json::json!({ "stmt_id": stmt_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消勾对应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/clear",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["removed"], serde_json::json!(1), "应清空 1 条对账单");
}

/// 往来手工核销 / 记录 / 账龄 / 取消核销。
#[tokio::test]
async fn web_settle_manual_records_aging() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "112201", "summary": "销售", "debit": "500", "credit": "0", "aux": { "customer": "C01" } },
        { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "500" }
    ])).await;
    post_voucher(&state, &sid, 2, serde_json::json!([
        { "line": 1, "account_code": "1001", "summary": "收款", "debit": "200", "credit": "0" },
        { "line": 2, "account_code": "112201", "summary": "核销", "debit": "0", "credit": "200", "aux": { "customer": "C01" } }
    ])).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/open?account=112201&upto=202601&all=1", &sid))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    let from_entry = rows.iter().find(|r| r["dir"] == serde_json::json!("借")).unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let to_entry = rows.iter().find(|r| r["dir"] == serde_json::json!("贷")).unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/run",
            &sid,
            serde_json::json!({ "from_entry": from_entry, "to_entry": to_entry, "amount": "200" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "手工核销应成功");
    let rec_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/records?account=112201", &sid))
        .await
        .unwrap();
    let recs: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(recs["rows"].as_array().unwrap().len(), 1, "应有 1 条核销记录");

    // 父级科目查询口径应与未核销/账龄一致（1122 含 112201 的记录）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/records?account=1122", &sid))
        .await
        .unwrap();
    let recs_parent: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        recs_parent["rows"].as_array().unwrap().len(),
        1,
        "父级科目应能查到下级核销记录"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/aging?account=112201&upto=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ag: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(!ag["buckets"].as_array().unwrap().is_empty(), "账龄应有分档");
    assert!(!ag["rows"].as_array().unwrap().is_empty(), "账龄应有数据");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/unsettle",
            &sid,
            serde_json::json!({ "id": rec_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消核销应成功");
}

/// 打印/导出端点冒烟（均返回 200 且内容类型正确）。
#[tokio::test]
async fn web_print_and_pdf_endpoints() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
        { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "100" }
    ])).await;

    let prints = [
        "/api/reports/balance-sheet/print?to=202601",
        "/api/reports/income-statement/print?from=202601&to=202601",
        "/api/reports/cash-flow/print?from=202601&to=202601",
        "/api/reports/equity/print?from=202601&to=202601",
        "/api/reports/trial-balance/print?from=202601&to=202601",
        "/api/vouchers/print-form?period=202601",
        "/api/ledger/print-form?code=1001&from=202601&to=202601&type=detail",
        "/api/ledger/print-form?code=1001&from=202601&to=202601&type=general",
    ];
    for uri in prints {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "打印端点应 200：{uri}");
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("text/html"), "应为 HTML：{uri} → {ct}");
    }

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/trial-balance/export?from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/trial-balance/pdf?from=202601&to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/pdf"), "应返回 PDF：{ct}");
}

// ---------------------------------------------------------------------------
// 读接口冒烟（不允许 5xx）/ 写接口冒烟 / 安全边界 / 年末结转
// ---------------------------------------------------------------------------

/// 全部只读接口冒烟：返回 2xx/4xx 均可，但不得 5xx（5xx = 引擎错误或 panic）。
#[tokio::test]
async fn web_read_endpoints_no_5xx() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let uris = [
        "/api/me",
        "/api/roles",
        "/api/dashboard",
        "/api/overview?period=202601",
        "/api/periods",
        "/api/accounts",
        "/api/vouchers/next-no?period=202601&word=记",
        "/api/vouchers?period=202601",
        "/api/invoices?period=202601",
        "/api/invoices/summary",
        "/api/ledger?code=1001&from=202601&to=202601",
        "/api/ledger/general?code=1001&from=202601&to=202601",
        "/api/ledger/journal?code=1001&from=202601&to=202601",
        "/api/reports/trial-balance?from=202601&to=202601",
        "/api/reports/multi-column?main=1001&cols=1002&from=202601&to=202601",
        "/api/reports/summary-table?from=202601&to=202601",
        "/api/reports/ratios?period=202601",
        "/api/reports/balance-sheet?to=202601",
        "/api/reports/income-statement?from=202601&to=202601",
        "/api/reports/cash-flow?from=202601&to=202601",
        "/api/reports/equity?from=202601&to=202601",
        "/api/reports/compare?report_key=balance_sheet&from=202601&to=202601",
        "/api/reports/daily?code=1001&from=202601&to=202601",
        "/api/reports/reconcile?period=202601",
        "/api/reports/aux-balance?kind=customer&from=202601&to=202601",
        "/api/reports/qty-balance?from=202601&to=202601",
        "/api/reports/notes?report_key=balance-sheet",
        "/api/inventory/aging",
        "/api/inventory/abc",
        "/api/inventory/serial?item=RM01",
        "/api/inventory/unit?item=RM01",
        "/api/inventory/warehouse-stock?item=RM01",
        "/api/inventory/transfer?period=202601",
        "/api/procure/reconcile?period=202601",
        "/api/procure/quota?supplier=S01&item=140301",
        "/api/procure/price?item=140301",
        "/api/procure/track?po_id=1",
        "/api/procure/stats?period=202601",
        "/api/procure/req?period=202601",
        "/api/sales/reconcile?period=202601",
        "/api/sales/credit?customer=C01&period=202601",
        "/api/sales/track?so_id=1",
        "/api/sales/stats?period=202601",
        "/api/sales/quote?period=202601",
        "/api/order/change-log?period=202601",
        "/api/budget/alerts?period=202601",
        "/api/budget/versions",
        "/api/budget/analysis?period=202601",
        "/api/routing/140301",
        "/api/prod",
        "/api/mrp/latest",
        "/api/approvals",
        "/api/approvals/todo",
        "/api/archives?period=202601",
        "/api/funds/bills?period=202601",
        "/api/funds/loans",
        "/api/funds/daily?period=202601",
        "/api/funds/forecast?period=202601",
        "/api/cost/configs",
        "/api/logs?limit=10",
        "/api/templates",
        "/api/templates/due?period=202601",
        "/api/payroll?period=202601",
        "/api/payroll/ytd?employee=E001&period=202601",
        "/api/claims?period=202601",
        "/api/claims/next-no?period=202601",
        "/api/assets?period=202601",
        "/api/bank?period=202601&account=100201",
        "/api/settle/open?account=112201&upto=202601",
        "/api/settle/records?account=112201",
        "/api/settle/aging?account=112201&upto=202601",
        "/api/custom-reports",
    ];
    let mut bad = Vec::new();
    for uri in uris {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        let code = resp.status().as_u16();
        if code >= 500 {
            bad.push(format!("{uri} → {code}"));
        }
    }
    assert!(bad.is_empty(), "只读接口出现 5xx：{bad:#?}");
}

/// 写接口冒烟：状态码 <500，并抽查若干返回 200。
#[tokio::test]
async fn web_write_endpoints_smoke() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let post = |uri: &'static str, body: serde_json::Value| {
        let state = state.clone();
        let sid = sid.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_post(uri, &sid, body))
                .await
                .unwrap();
            (resp.status(), body_string(resp).await)
        }
    };

    // 逐项写入并断言 200（发现引擎错误/500 即为问题）
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("/api/period", serde_json::json!({ "ymm": 202601 })),
        ("/api/accounts/fill-defaults", serde_json::json!({})),
        ("/api/inventory/unit", serde_json::json!({ "item": "RM01", "base_unit": "个", "alt_unit": "箱", "factor": "12" })),
        ("/api/inventory/serial", serde_json::json!({ "item": "RM01", "serials": ["SN001", "SN002"], "batch_no": "B1", "date": "2026-01-10" })),
        ("/api/inventory/serial/out", serde_json::json!({ "serials": ["SN001"], "date": "2026-01-11" })),
        ("/api/inventory/adjust", serde_json::json!({ "period": 202601, "date": "2026-01-10", "item": "RM01", "delta": "5", "memo": "t" })),
        ("/api/inventory/assemble", serde_json::json!({ "parent": "140501", "children": [["140301", "1"]], "date": "2026-01-10", "memo": "t" })),
        ("/api/inventory/disassemble", serde_json::json!({ "parent": "140501", "children": [["140301", "1"]], "date": "2026-01-10", "memo": "t" })),
        ("/api/procure/req", serde_json::json!({ "period": 202601, "date": "2026-01-10", "item_code": "140301", "item_name": "原料", "qty": "1", "requester": "admin", "memo": "" })),
        ("/api/sales/quote", serde_json::json!({ "id": 0, "period": 202601, "date": "2026-01-10", "customer_code": "C01", "customer_name": "客户", "item_code": "140501", "item_name": "成品", "qty": "1", "unit_price": "10", "status": "draft", "memo": "" })),
        ("/api/routing/140301", serde_json::json!([{ "seq": 1, "op_code": "OP1", "op_name": "车", "work_center": "WC1", "std_hours": "1", "rate": "10" }])),
        ("/api/mrp/run", serde_json::json!({ "demands": [{ "item_code": "140501", "qty": "10", "source": "手工" }] })),
        ("/api/budget/versions", serde_json::json!({ "key": "V1", "name": "版本1", "is_current": false, "memo": "" })),
        ("/api/approvals", serde_json::json!({ "biz_kind": "test", "biz_id": 1, "title": "t", "approvers": ["boss"] })),
        ("/api/reports/notes", serde_json::json!({ "report_key": "balance-sheet", "period": 202601, "content": "附注" })),
        ("/api/archives", serde_json::json!({ "period": 202601, "kind": "voucher", "title": "t", "payload": "{}" })),
        ("/api/funds/bills", serde_json::json!({ "kind": "receivable", "no": "B001", "period": 202601, "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "客户", "bank": "工行", "amount": "1000", "memo": "" })),
        ("/api/funds/loans", serde_json::json!({ "kind": "borrow", "no": "L001", "bank": "工行", "principal": "10000", "rate_pct": "4.5", "start_date": "2026-01-01", "end_date": "2026-12-31", "memo": "" })),
        ("/api/cost/configs", serde_json::json!({ "item": "140501", "method": "fifo", "standard_cost": "0" })),
        ("/api/templates", serde_json::json!({ "id": 0, "name": "月度模板", "memo": "", "entries": [] })),
    ];
    let mut bad = Vec::new();
    for (uri, body) in cases {
        let (st, text) = post(uri, body).await;
        if st.as_u16() >= 500 {
            bad.push(format!("{uri} → {st} {text}"));
        }
    }
    assert!(bad.is_empty(), "写接口出现 5xx：{bad:#?}");

    // 抽查关键写接口确实成功（避免上面只证明"没炸"）
    for (uri, body) in [
        ("/api/inventory/unit", serde_json::json!({ "item": "RM02", "base_unit": "个", "alt_unit": "箱", "factor": "6" })),
        ("/api/funds/bills", serde_json::json!({ "kind": "payable", "no": "B002", "period": 202601, "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "供应商", "bank": "工行", "amount": "500", "memo": "" })),
        ("/api/cost/configs", serde_json::json!({ "item": "140502", "method": "moving_average", "standard_cost": "0" })),
        // 单号可省略（服务端自动生成）：UI 就是这么发的，缺 no 不能 422
        ("/api/procure/req", serde_json::json!({ "period": 202601, "date": "2026-01-10", "item_code": "140301", "item_name": "原料", "qty": "3", "status": "draft", "requester": "admin", "memo": "" })),
        ("/api/sales/quote", serde_json::json!({ "id": 0, "period": 202601, "date": "2026-01-10", "customer_code": "C01", "customer_name": "客户", "item_code": "140501", "item_name": "成品", "qty": "1", "unit_price": "10", "status": "draft", "prepared_by": "", "memo": "" })),
    ] {
        let (st, text) = post(uri, body).await;
        assert_eq!(st, StatusCode::OK, "{uri} 应成功：{text}");
    }

    // 列表接口的空筛选值应视为「全部」：UI 下拉默认值就是空串，
    // 若被当成具体类型去过滤（kind=''）会永远查不到已保存的数据。
    for uri in [
        "/api/funds/bills?kind=",
        "/api/funds/loans?kind=",
        "/api/archives?period=202601&kind=",
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri} 应成功");
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(
            !v["rows"].as_array().unwrap().is_empty(),
            "{uri} 空筛选应返回全部数据：{v}"
        );
    }
}

/// 安全边界：CSRF 跨站拒绝、退出登录、强制改密拦截、来源 IP 限流。
#[tokio::test]
async fn web_security_boundaries() {
    let (state, _bd, _dir) = test_state();

    // CSRF：跨站 Origin 拒绝（需带 Host 才能比较）
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://evil.example")
        .body(Body::from(
            serde_json::json!({ "username": "x", "password": "y", "device_id": "d1" }).to_string(),
        ))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "跨站 Origin 应被拒");

    // CSRF：Sec-Fetch-Site 跨站拒绝
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from(
            serde_json::json!({ "username": "x", "password": "y", "device_id": "d1" }).to_string(),
        ))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "跨站请求应被拒");

    // 同源 Origin 放行（凭据错误应为 401，而不是被 CSRF 拦成 403）
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .body(Body::from(
            serde_json::json!({
                "username": "ghost", "password": "bad",
                "device_id": "d1", "device_name": "测试机"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "同源请求应进入登录校验");

    // 退出登录后会话失效
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/logout", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "退出后旧会话应失效");

    // 强制改密：未改密前除改密/退出外一律 401
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "mc1", "display_name": "待改密", "password": "Init123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (st, mc_sid) = login(&state, "mc1", "Init123456").await;
    assert_eq!(st, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &mc_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "未改密应被拦截");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &mc_sid,
            serde_json::json!({ "old": "Init123456", "new": "Init654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改密应放行");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &mc_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改密后应可访问");

    // 来源 IP 限流：同一 XFF 连续失败 50 次后 429（用户名各不相同，避开账号维度）
    let mut last = StatusCode::UNAUTHORIZED;
    for i in 0..51 {
        let req = Request::builder()
            .method("POST")
            .uri("/api/login")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", "10.9.9.9")
            .body(Body::from(
                serde_json::json!({
                    "username": format!("ghost{i}"),
                    "password": "bad",
                    "device_id": "dev-xff",
                    "device_name": "测试机"
                })
                .to_string(),
            ))
            .unwrap();
        let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
        last = resp.status();
        if last == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS, "同一来源连续失败应被限流");
}

/// 年末结转：本年利润 → 未分配利润。
#[tokio::test]
async fn web_year_end_carry() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 6401 100 / 贷 1001 100，并结转损益（生成 4103 贷方 100）
    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "6401", "summary": "成本", "debit": "100", "credit": "0" },
        { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "100" }
    ])).await;
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
    assert_eq!(resp.status(), StatusCode::OK);

    // 年末结转：4103 余额 -100 → 转入未分配利润
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/periods/202601/year-end", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "年末结转应成功：{}",
        body_string(resp).await
    );
    // 幂等性：第二次调用 4103 已清零，应 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/periods/202601/year-end", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "余额为 0 时年末结转应被拒");
}

/// Web 导入预检：报告缺失科目 → 带映射导入成功。
#[tokio::test]
async fn web_import_analyze_and_map() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let _ = select_book(&state, &sid, "b1").await;

    // 预检：9999 不在科目表中，2001 存在
    let csv = "9999,借,100\n2001,贷,100\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/analyze",
            &sid,
            serde_json::json!({ "kind": "begin", "template": "generic", "text": csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入预检应成功");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let missing = v["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 1, "只应报告缺失科目 9999：{v}");
    assert_eq!(missing[0]["code"], serde_json::json!("9999"));
    assert_eq!(missing[0]["count"], serde_json::json!(1));

    // 映射 9999→1001 后导入：两行都应写入
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin", "template": "generic", "text": csv,
                "mapping": { "9999": "1001" },
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "带映射的导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 行：{s}");
    assert!(s.contains("\"skipped\":0"), "不应跳过：{s}");
}

/// 红字冲销：未传日期时取期间末日（历史期间冲销不会因"今天"不在期间内而失败）。
#[tokio::test]
async fn web_reverse_defaults_to_period_last_day() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let id = post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "冲销测试", "debit": "100", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "冲销测试", "debit": "0", "credit": "100" }
        ]),
    )
    .await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/reverse"),
            &sid,
            serde_json::json!({ "period": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "缺省日期红冲应成功");
    let rid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_ne!(rid, id, "红冲应生成新凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{rid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["date"], serde_json::json!("2026-01-31"), "应取期间末日：{v}");
}
