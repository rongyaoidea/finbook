//! 客户视图 HTTP 层集成测试。
//!
//! 分工：findb 层的 `customers::tests` 验**口径**（余额怎么算），
//! 这层验**接口与权限**。唯一交叉点是 `customer_detail_balance_matches_list`
//! —— 若两层各自算一遍余额，界面上会「列表说欠 1000、点进去明细加起来 900」。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

use finweb::handlers;
use finweb::state::WebState;

mod common;
use common::{
    authed_get, authed_post, boss_in_b1, body_string, login, money_num, select_book, test_state,
};

/// 建一个客户档案（同步：内部只有一次 oneshot().await，故须 async；
/// 这里包一层是因为路由调用本身就是异步的）
async fn mk_customer(state: &Arc<WebState>, sid: &str, code: &str, name: &str) {
    let req = authed_post(
        "/api/aux",
        sid,
        serde_json::json!({
            // id: 0 —— AuxEntity 是整结构反序列化，每个字段都必填
            // （fincore/src/auxiliary.rs:44，没有 serde(default)）
            "id": 0,
            "kind": "customer", "code": code, "name": name,
            "parent_code": null, "disabled": false, "props": {}, "memo": ""
        }),
    );
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "建客户档案 {code} 应成功：{}",
        body_string(resp).await
    );
}

/// 记一笔**已记账**的应收（借 112201 / 贷 1001，辅助核算挂客户）
///
/// 三步（审核 → 出纳签字 → 记账）不能省：`test_state()` 的夹具**显式**关掉了
/// 那两道闸门，所以本层其实只需 audit + post。但这里仍走三步 ——
/// 将来若有人把夹具改回生产默认值（那才是该做的），本测试不会突然红。
async fn post_ar(state: &Arc<WebState>, sid: &str, date: &str, cust: &str, amount: &str) {
    let req = authed_post(
        "/api/vouchers",
        sid,
        serde_json::json!({
            "id": 0, "period": 202601, "date": date, "word": "记", "no": 0,
            "attachments": 0, "memo": "",
            "entries": [
                { "line": 1, "account_code": "112201", "summary": "应收",
                  "debit": amount, "credit": "0", "aux": { "customer": cust } },
                { "line": 2, "account_code": "1001", "summary": "收",
                  "debit": "0", "credit": amount }
            ]
        }),
    );
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录凭证：{}", body_string(resp).await);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = v["id"].as_i64().expect("应返回凭证 id");
    for step in ["audit", "sign", "post"] {
        let req = authed_post(&format!("/api/vouchers/{vid}/{step}"), sid, serde_json::json!({}));
        let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{step} 应成功：{}",
            body_string(resp).await
        );
    }
}

/// ① 列表能出数，且汇总字段与 rows 自洽
#[tokio::test]
async fn customers_list_returns_rows_and_consistent_totals() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_customer(&state, &sid, "C01", "客户甲").await;
    mk_customer(&state, &sid, "C02", "客户乙").await;
    post_ar(&state, &sid, "2026-01-05", "C01", "1000").await;
    post_ar(&state, &sid, "2026-01-06", "C02", "250").await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();

    let rows = v["rows"].as_array().expect("应返回 rows");
    assert_eq!(rows.len(), 2, "两个客户都该出现：{v}");
    assert_eq!(v["total"].as_u64(), Some(2), "total 要与 rows 长度一致");
    assert_eq!(v["open_count"].as_u64(), Some(2), "两人都未核销");
    assert_eq!(
        money_num(v["open_sum"].as_str().unwrap()),
        1250.0,
        "open_sum = 1000 + 250：{v}"
    );
    // 按未核销倒序：催款时先看到最该催的
    assert_eq!(rows[0]["code"].as_str(), Some("C01"), "欠款多的排前面");
    assert_eq!(
        money_num(rows[0]["balance"].as_str().unwrap()),
        1000.0,
        "余额取已记账分录"
    );
    assert_eq!(rows[0]["open_count"].as_u64(), Some(1), "未核销笔数");

    // only_open 过滤
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers?only_open=1", &sid))
        .await
        .unwrap();
    let v2: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v2["rows"].as_array().unwrap().len(), 2);

    // 搜索过滤（按名称）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers?q=%E4%B9%99", &sid)) // URL 编码的「乙」
        .await
        .unwrap();
    let v3: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows3 = v3["rows"].as_array().unwrap();
    assert_eq!(rows3.len(), 1, "按名称搜「乙」应只有一条");
    assert_eq!(rows3[0]["code"].as_str(), Some("C02"));
}

/// ② **跨接口口径一致**：详情余额 == 列表余额 == 明细逐行相加
///
/// 这是 findb 层与 web 层之间唯一的交叉点，也是「余额只有一处算法」
/// 这条约束的真正考验。
#[tokio::test]
async fn customer_detail_balance_matches_list() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_customer(&state, &sid, "C01", "客户甲").await;
    post_ar(&state, &sid, "2026-01-05", "C01", "1200").await;
    post_ar(&state, &sid, "2026-01-09", "C01", "300").await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let from_list = money_num(list["rows"][0]["balance"].as_str().unwrap());

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers/C01", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let det: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    // 注意路径是**平的**：CustomerDetail 上有 #[serde(flatten)] summary，
    // 所以 balance 在 customer.balance，不在 customer.summary.balance。
    let from_detail = money_num(det["customer"]["balance"].as_str().unwrap());

    assert_eq!(
        from_list, from_detail,
        "列表与详情的余额必须一致（同一处算法）：列表 {from_list} vs 详情 {from_detail}"
    );

    let lines = det["customer"]["lines"].as_array().unwrap();
    let sum: f64 = lines
        .iter()
        .map(|l| money_num(l["amount"].as_str().unwrap()))
        .sum();
    assert_eq!(
        sum, from_detail,
        "明细逐行相加应等于汇总（否则界面上「对不上账」）：{det}"
    );
    assert_eq!(lines.len(), 2, "2 张凭证 = 2 行明细，实际 {}", lines.len());
    for l in lines {
        assert!(l["voucher_id"].as_i64().unwrap_or(0) > 0, "明细行应有 voucher_id：{l}");
        assert!(!l["doc_no"].as_str().unwrap().is_empty(), "应有单据号：{l}");
    }
    assert_eq!(
        money_num(det["customer"]["opening_balance"].as_str().unwrap()),
        0.0,
        "没有期初挂账时该字段应为 0 而不是缺省（且路径是平的，见上）"
    );
}

/// ③ 权限分界：出纳能看，不能改档案
///
/// 设计 §4：出纳要看（催款）、要能核销，但**不该能改客户档案**。
/// 把「挂 Report 而不是 AuxEdit」这个决定钉住 ——
/// 将来若有人改成 AuxEdit（因为「客户管理页嘛」），这条会红。
#[tokio::test]
async fn cashier_can_read_customers_but_not_edit() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // 账套内子账号必须**先**在平台层开通同名账号 —— 直接 POST /api/users
    // 会报 400「该账号尚未开通」。helpers.js 的 addBookUser 注释记着这件事，
    // 我写测试时忘了。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss,
            serde_json::json!({
                "username": "csh", "display_name": "出纳",
                "password": "Csh@2026x", "is_admin": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通平台账号：{}", body_string(resp).await);

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({
                "username": "csh", "display_name": "出纳", "password": "",
                "role": "cashier", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "拉进出纳：{}", body_string(resp).await);

    let (st, sid) = login(&state, "csh", "Csh@2026x").await;
    assert_eq!(select_book(&state, &sid, "b1").await, StatusCode::OK);

    // 能读
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "出纳应能看客户列表（催款要用）：{}",
        body_string(resp).await
    );

    // 不能改档案
    // payload 必须**完整**：缺 `id` 会被反序列化挡下（422），
    // 根本走不到权限检查 —— 那时这条测试红的原因就不是它想验的那件事。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0,
                "kind": "customer", "code": "CX", "name": "越权新建",
                "parent_code": null, "disabled": false, "props": {}, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "出纳不该能新建客户档案 —— 若这条红了，说明 /api/aux 放宽了（payload 完整，故 403 来自权限而非反序列化）"
    );
}

/// ④ 不存在的客户 → 404 且报错点名是哪个客户
#[tokio::test]
async fn customer_missing_is_404_named() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers/NOPE", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let t = body_string(resp).await;
    assert!(
        t.contains("NOPE"),
        "报错要点名是哪个客户，否则用户分不清是编码打错还是没建档：{t}"
    );
}
