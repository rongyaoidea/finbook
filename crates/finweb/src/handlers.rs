//! HTTP 请求处理函数

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use axum::routing::{get, post, put};
use chrono::{Datelike, NaiveDate};
use fincore::{AuxRef, Entry, Period, Role, User, Voucher};
use fincore::user::Perm;
use findb::accounts;
use findb::balances::{self, BalanceSnapshot, BalanceQuery, LedgerQuery};
use findb::periods;
use findb::security::{self, DeviceIdentity};
use findb::users;
use findb::vouchers::{self, VoucherQuery};
use serde_json::json;

use crate::dto::*;
use crate::state::{
    period_to_str, parse_money, parse_period, clear_cookie_header, AppError, CurrentUser, WebState,
};

const SESSION_SECS: i64 = 60 * 60 * 24 * 7;

/// 组装路由
pub fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/api/setup/status", get(get_setup_status))
        .route("/api/login", post(post_login))
        .route("/api/logout", post(post_logout))
        .route("/api/me", get(get_me))
        .route("/api/change-password", post(post_change_password))
        // 用户管理
        .route("/api/users", get(list_users).post(create_user))
        .route(
            "/api/users/:username",
            put(update_user).delete(delete_user),
        )
        .route(
            "/api/users/:username/reset-password",
            post(reset_user_password),
        )
        .route(
            "/api/users/:username/reset-device",
            post(reset_user_device),
        )
        // 账套参数 / 仪表盘 / 期间
        .route("/api/options", get(get_options).put(put_options))
        .route("/api/dashboard", get(get_dashboard))
        .route("/api/periods", get(get_periods))
        .route("/api/period", post(post_period))
        // 科目 / 凭证
        .route("/api/accounts", get(list_accounts))
        .route("/api/vouchers/next-no", get(next_voucher_no))
        .route("/api/vouchers", get(list_vouchers).post(save_voucher))
        .route("/api/vouchers/:id", get(get_voucher))
        .route("/api/vouchers/:id/post", post(voucher_post))
        .route("/api/vouchers/:id/audit", post(voucher_audit))
        .route("/api/vouchers/:id/unaudit", post(voucher_unaudit))
        .route("/api/vouchers/:id/delete", post(voucher_delete))
        // 账簿 / 报表
        .route("/api/ledger", get(get_ledger))
        .route("/api/reports/trial-balance", get(get_trial_balance))
        .route(
            "/api/reports/trial-balance/print",
            get(print_trial_balance),
        )
        .route(
            "/api/reports/trial-balance/export",
            get(export_trial_balance),
        )
        .route("/api/health", get(|| async { "ok" }))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// 认证 / 初始化状态
// ---------------------------------------------------------------------------

async fn get_setup_status(State(state): State<Arc<WebState>>) -> Result<Json<SetupStatus>, AppError> {
    let db = state.pool.get()?;
    let admin_set = users::admin_exists(&db)?;
    // 注意：不返回管理员用户名——该接口无需登录即可访问，
    // 暴露账号名等于替攻击者完成了一半的用户名枚举。
    Ok(Json(SetupStatus {
        admin_set,
        company: state.company.clone(),
        version: state.version.clone(),
        book: state.book_path.display().to_string(),
    }))
}

async fn post_login(
    State(state): State<Arc<WebState>>,
    Json(req): Json<LoginReq>,
) -> Result<Response, AppError> {
    let db = state.pool.get()?;
    let username = req.username.trim().to_string();
    let count = users::count(&db)?;

    // 首次登录即管理员：账套尚无任何用户时，直接用本次登录创建管理员账号
    let mut setup = false;
    if count == 0 {
        if username.is_empty() || req.password.len() < 6 {
            return Err(AppError::bad_request(
                "首次使用请设置管理员账号：用户名不能为空，口令至少 6 位",
            ));
        }
        // 并发场景：两个请求同时看到 0 用户，后落库的那个会撞唯一约束。
        // 此时账套里已经有管理员了，走正常登录即可，而不是抛 500。
        if let Err(e) = users::create_admin(&db, &username, &req.password, &username) {
            if users::admin_exists(&db)? {
                // 已被并发请求初始化，继续正常登录流程
            } else {
                return Err(e.into());
            }
        } else {
            setup = true;
        }
    }

    let dev = DeviceIdentity::new(&req.device_id, &req.device_name);
    let res = security::login(&db, &username, &req.password, &state.policy, Some(&dev))?;
    let must_change = matches!(res, security::LoginResult::MustChangePassword(_));

    match res {
        security::LoginResult::Ok(u) | security::LoginResult::MustChangePassword(u) => {
            // 普通账号登录时踢掉旧会话（一人一机）；管理员不受限制，可多端并存
            if !u.is_admin() {
                state.sessions.remove_by_username(&u.username);
            }
            let token = state.sessions.create(&u.username, &req.device_id, state.default_period);
            let resp = LoginResp {
                user: PublicUser::from_user(&u),
                must_change_pwd: must_change,
                setup,
            };
            let body = Json(resp).into_response();
            let mut resp = body;
            resp.headers_mut()
                .insert(header::SET_COOKIE, crate::state::cookie_header(&token, SESSION_SECS));
            Ok(resp)
        }
        security::LoginResult::BadPassword { remaining } => Err(AppError::bad_request(format!(
            "口令错误，还剩 {remaining} 次机会"
        ))),
        security::LoginResult::Locked { minutes } => Err(AppError::bad_request(format!(
            "账户已被锁定，请 {minutes} 分钟后再试"
        ))),
        security::LoginResult::Disabled => {
            Err(AppError::forbidden("该账户已停用，请联系管理员"))
        }
        security::LoginResult::NoSuchUser => Err(AppError::unauthorized("用户不存在")),
        security::LoginResult::DeviceBound { device_name } => Err(AppError::forbidden(format!(
            "该账号已绑定设备「{device_name}」，如需在新设备登录请联系管理员在「安全中心 → 用户管理」中重置设备绑定"
        ))),
    }
}

async fn post_logout(State(state): State<Arc<WebState>>, jar: axum_extra::extract::cookie::CookieJar) -> Response {
    if let Some(c) = jar.get("finbook_sid") {
        state.sessions.remove(c.value());
    }
    let mut r = Json(json!({"ok": true})).into_response();
    r.headers_mut().insert(header::SET_COOKIE, clear_cookie_header());
    r
}

async fn get_me(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<PublicUser>, AppError> {
    let db = state.pool.get()?;
    let u = users::get(&db, user.username())?
        .ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    Ok(Json(PublicUser::from_user(&u)))
}

async fn post_change_password(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ChangePwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let db = state.pool.get()?;
    let r = security::change_password_checked(&db, user.username(), &req.old, &req.new, &state.policy)?;
    match r {
        Ok(()) => Ok(Json(json!({"ok": true}))),
        Err(msg) => Err(AppError::bad_request(msg)),
    }
}

// ---------------------------------------------------------------------------
// 用户管理（UserManage）
// ---------------------------------------------------------------------------

async fn list_users(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<PublicUser>>, AppError> {
    user.require(Perm::UserManage)?;
    let db = state.pool.get()?;
    let list = users::list(&db)?
        .into_iter()
        .map(|u| PublicUser::from_user(&u))
        .collect();
    Ok(Json(list))
}

async fn create_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CreateUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let username = req.username.trim().to_string();
    if username.is_empty() || req.password.len() < 6 {
        return Err(AppError::bad_request("用户名不能为空，口令至少 6 位"));
    }
    let db = state.pool.get()?;
    if users::get(&db, &username)?.is_some() {
        return Err(AppError::bad_request("该用户名已存在"));
    }
    let mut u = User::new(&username, &req.display_name, req.role);
    u.set_password(&req.password);
    // 管理员开的号，口令是管理员定的——首次登录必须自己改一次
    u.must_change_pwd = true;
    // 普通账户默认只能看自己填制的凭证（防越权翻看他人/全盘数据）；
    // 管理员不受此限制，可看到所有账套数据
    if !u.is_admin() {
        u.data_scope.own_voucher_only = true;
    }
    let id = users::insert(&db, &u)?;
    db.log(user.username(), "安全", "新建用户", &format!("创建账号「{username}」（{}）", req.role.label()))?;
    Ok(Json(json!({"id": id})))
}

async fn update_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let db = state.pool.get()?;
    let mut u = users::get(&db, &username)?
        .ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    if let Some(d) = req.display_name {
        u.display_name = d;
    }
    if let Some(r) = req.role {
        // 不允许把最后一个管理员改成其他角色
        if u.is_admin() && r != Role::Admin {
            let admins = users::list(&db)?.into_iter().filter(|x| x.is_admin()).count();
            if admins <= 1 {
                return Err(AppError::bad_request("至少保留一个系统管理员账号"));
            }
        }
        u.role = r;
    }
    if let Some(d) = req.disabled {
        u.disabled = d;
        if d {
            // 停用账号：立即下线其全部会话
            state.sessions.remove_by_username(&username);
        }
    }
    if let Some(m) = req.must_change_pwd {
        u.must_change_pwd = m;
    }
    users::update(&db, &u)?;
    db.log(user.username(), "安全", "修改用户", &format!("更新「{username}」的信息"))?;
    Ok(Json(json!({"ok": true})))
}

async fn reset_user_password(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
    Json(req): Json<ResetPwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let db = state.pool.get()?;
    let r = security::admin_reset_password(&db, &username, &req.new, &state.policy)?;
    match r {
        Ok(()) => {
            db.log(user.username(), "安全", "重置口令", &format!("重置「{username}」的口令"))?;
            Ok(Json(json!({"ok": true})))
        }
        Err(msg) => Err(AppError::bad_request(msg)),
    }
}

async fn reset_user_device(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let db = state.pool.get()?;
    users::reset_device(&db, &username)?;
    // 立刻下线该用户全部会话：旧设备不能靠存量会话绕过"一人一机"
    state.sessions.remove_by_username(&username);
    db.log(user.username(), "安全", "重置设备绑定", &format!("重置「{username}」的设备绑定"))?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    if username == user.username() {
        return Err(AppError::bad_request("不能删除当前登录的账号"));
    }
    let db = state.pool.get()?;
    let u = users::get(&db, &username)?
        .ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    if u.is_admin() {
        let admins = users::list(&db)?.into_iter().filter(|x| x.is_admin()).count();
        if admins <= 1 {
            return Err(AppError::bad_request("至少保留一个系统管理员账号"));
        }
    }
    users::delete(&db, u.id)?;
    state.sessions.remove_by_username(&username);
    db.log(user.username(), "安全", "删除用户", &format!("删除账号「{username}」"))?;
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// 账套参数 / 仪表盘 / 期间
// ---------------------------------------------------------------------------

async fn get_options(
    State(state): State<Arc<WebState>>,
    _user: CurrentUser,
) -> Result<Json<fincore::BookOptions>, AppError> {
    let db = state.pool.get()?;
    Ok(Json(db.options()))
}

async fn put_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(opts): Json<fincore::BookOptions>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = state.pool.get()?;
    db.set_options(&opts)?;
    Ok(Json(json!({"ok": true})))
}

fn current_period(state: &WebState, user: &CurrentUser) -> Period {
    let ymm = user
        .token
        .is_empty()
        .then(|| state.default_period)
        .or_else(|| state.sessions.period(&user.token))
        .unwrap_or(state.default_period);
    Period::from_ymm(ymm)
}

async fn get_dashboard(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Dashboard>, AppError> {
    let db = state.pool.get()?;
    let (v, e, a) = db.stats()?;
    let start = db.options().start_period;
    let cur = current_period(&state, &user);
    let closed = periods::closed_upto(&db)?;
    Ok(Json(Dashboard {
        company: state.company.clone(),
        start_period: period_to_str(start),
        current_period: period_to_str(cur),
        closed_upto: closed.map(period_to_str),
        vouchers: v,
        entries: e,
        accounts: a,
    }))
}

async fn get_periods(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let db = state.pool.get()?;
    let start = db.options().start_period;
    let this_year = Period::default().year();
    let end = Period::new(this_year + 1, 12).unwrap_or(Period::default());
    let list: Vec<String> = if end >= start {
        Period::range(start, end)
            .into_iter()
            .map(period_to_str)
            .collect()
    } else {
        vec![period_to_str(start)]
    };
    let cur = current_period(&state, &user);
    let closed = periods::closed_upto(&db)?;
    Ok(Json(json!({
        "current": period_to_str(cur),
        "closed_upto": closed.map(period_to_str),
        "start": period_to_str(start),
        "list": list,
    })))
}

async fn post_period(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (year, month) = (req.ymm / 100, req.ymm % 100);
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) {
        return Err(AppError::bad_request(format!(
            "非法期间 {ymm}：应为 YYYYMM（年份 1970-9999，月份 1-12）",
            ymm = req.ymm
        )));
    }
    state.sessions.set_period(&user.token, req.ymm);
    Ok(Json(json!({"ok": true, "period": period_to_str(Period::from_ymm(req.ymm))})))
}

// ---------------------------------------------------------------------------
// 科目 / 凭证
// ---------------------------------------------------------------------------

async fn list_accounts(
    State(state): State<Arc<WebState>>,
    _user: CurrentUser,
) -> Result<Json<Vec<fincore::Account>>, AppError> {
    let db = state.pool.get()?;
    Ok(Json(accounts::list(&db)?))
}

async fn next_voucher_no(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.pool.get()?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let word = q.get("word").cloned().unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no(&db, period, &word)?;
    Ok(Json(json!({"no": no})))
}

fn can_view_vouchers(user: &CurrentUser) -> bool {
    user.can(Perm::Report)
        || user.can(Perm::VoucherNew)
        || user.can(Perm::VoucherEdit)
        || user.can(Perm::VoucherAudit)
        || user.can(Perm::VoucherPost)
}

fn to_item(v: &Voucher) -> VoucherListItem {
    VoucherListItem {
        id: v.id,
        period: period_to_str(v.period),
        date: v.date.format("%Y-%m-%d").to_string(),
        word: v.word.clone(),
        no: v.no,
        voucher_no: v.voucher_no(),
        summary: v.first_summary(),
        debit_total: v.debit_total().fmt_money(),
        credit_total: v.credit_total().fmt_money(),
        status: serde_json::to_value(v.status)
            .ok()
            .and_then(|x| x.as_str().map(String::from))
            .unwrap_or_default(),
        status_label: v.status.label().to_string(),
        prepared_by: v.prepared_by.clone(),
    }
}

async fn list_vouchers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<VoucherListItem>>, AppError> {
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = state.pool.get()?;
    let mut query = VoucherQuery::default();
    query.asc = true;
    if let Some(p) = q.get("period").and_then(|s| parse_period(s)) {
        query.from = Some(p);
        query.to = Some(p);
    }
    if let Some(kw) = q.get("q") {
        let kw = kw.trim().to_string();
        if !kw.is_empty() {
            query.keyword = Some(kw);
        }
    }
    if let Some(st) = q.get("status").and_then(|s| parse_voucher_status(s)) {
        query.status = Some(st);
    }
    query.limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .or(Some(200));
    let list = vouchers::list(&db, &query)?
        .iter()
        .map(to_item)
        .collect();
    Ok(Json(list))
}

async fn get_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<VoucherDetail>, AppError> {
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = state.pool.get()?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    Ok(Json(VoucherDetail::from_voucher(v)))
}

async fn save_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SaveVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 新增需 VoucherNew，修改需 VoucherEdit
    if req.id > 0 {
        user.require(Perm::VoucherEdit)?;
    } else {
        user.require(Perm::VoucherNew)?;
    }
    let db = state.pool.get()?;
    let period = parse_period(&req.period.to_string())
        .unwrap_or_else(|| current_period(&state, &user));
    let date = NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    let word = if req.word.is_empty() {
        "记".to_string()
    } else {
        req.word.clone()
    };

    let mut v = if req.id > 0 {
        let mut existing = vouchers::get(&db, req.id)?
            .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
        if !existing.status.can_edit() {
            return Err(AppError::forbidden("该凭证已审核/记账，不能修改"));
        }
        // 日期不能漂移到凭证期间之外（期间本身不可改，改的是日期）
        if (date.year(), date.month()) != (existing.period.year(), existing.period.month()) {
            return Err(AppError::bad_request(format!(
                "凭证日期 {} 不在其所属期间 {} 内",
                date.format("%Y-%m-%d"),
                existing.period.label()
            )));
        }
        existing.date = date;
        existing.word = word.clone();
        existing.attachments = req.attachments;
        existing.memo = req.memo.clone();
        existing.entries.clear();
        existing
    } else {
        if (date.year(), date.month()) != (period.year(), period.month()) {
            return Err(AppError::bad_request(format!(
                "凭证日期 {} 不在所选期间 {} 内",
                date.format("%Y-%m-%d"),
                period.label()
            )));
        }
        let no = if req.no > 0 {
            req.no
        } else {
            vouchers::next_no(&db, period, &word)?
        };
        let mut v = Voucher::new(period, date, word, no);
        v.attachments = req.attachments;
        v.memo = req.memo.clone();
        v
    };
    v.prepared_by = user.username().to_string();
    for e in &req.entries {
        let mut en = Entry::new(e.line, e.account_code.clone(), e.summary.clone());
        en.debit = parse_money(&e.debit);
        en.credit = parse_money(&e.credit);
        en.aux = AuxRef::default();
        v.entries.push(en);
    }
    if !v.balanced() {
        return Err(AppError::bad_request("借贷不平衡，请检查分录金额"));
    }
    let id = vouchers::save(&db, &mut v)?;
    db.log(
        user.username(),
        "凭证",
        if req.id > 0 { "修改" } else { "新增" },
        &v.voucher_no(),
    )?;
    Ok(Json(json!({"id": id})))
}

async fn voucher_post(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherPost)?;
    let db = state.pool.get()?;
    vouchers::post(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

async fn voucher_audit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherAudit)?;
    let db = state.pool.get()?;
    vouchers::audit(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

async fn voucher_unaudit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherUnaudit)?;
    let db = state.pool.get()?;
    vouchers::unaudit(&db, id)?;
    Ok(Json(json!({"ok": true})))
}

async fn voucher_delete(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = state.pool.get()?;
    vouchers::delete(&db, id)?;
    db.log(user.username(), "凭证", "删除", &format!("凭证 #{id}"))?;
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// 账簿 / 报表
// ---------------------------------------------------------------------------

async fn get_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::balance::LedgerRow>>, AppError> {
    user.require(Perm::Report)?;
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码参数 code"));
    }
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let to = q
        .get("to")
        .and_then(|s| parse_period(s))
        .unwrap_or(from);
    let include_children = q
        .get("include_children")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(false);
    let db = state.pool.get()?;
    let chart = accounts::chart(&db)?;
    let lq = LedgerQuery {
        code,
        include_children,
        aux: None,
        from,
        to,
        posted_only,
    };
    let rows = balances::ledger(&db, &chart, &lq)?;
    Ok(Json(rows))
}

/// 计算科目余额表（供 JSON / 打印 / 导出复用）
fn trial_balance_data(
    state: &WebState,
    user: &CurrentUser,
    q: &HashMap<String, String>,
) -> Result<(Vec<fincore::balance::BalanceRow>, fincore::balance::TrialBalance), AppError> {
    let db = state.pool.get()?;
    let start = db.options().start_period;
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or(start);
    let to = q
        .get("to")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(state, user));
    let bq = BalanceQuery::range(from, to);
    let snap = BalanceSnapshot::load(&db, &bq)?;
    let chart = accounts::chart(&db)?;
    let rows = snap.account_table(&chart, &bq);
    let totals = snap.trial_balance(&chart);
    Ok((rows, totals))
}

async fn get_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let rows: Vec<TrialRow> = rows.iter().map(TrialRow::from_row).collect();
    Ok(Json(json!({
        "from": q.get("from").cloned().unwrap_or_default(),
        "to": q.get("to").cloned().unwrap_or_default(),
        "rows": rows,
        "totals": {
            "begin_debit": totals.begin_debit.fmt_money(), "begin_credit": totals.begin_credit.fmt_money(),
            "debit": totals.period_debit.fmt_money(), "credit": totals.period_credit.fmt_money(),
            "end_debit": totals.end_debit.fmt_money(), "end_credit": totals.end_credit.fmt_money(),
        },
    })))
}

async fn print_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 打印预览：所有有「账簿报表」权限的角色都可使用（不落地文件）
    user.require(Perm::Report)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let html = trial_balance_html(&state, &q, &rows, &totals);
    Ok((
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response())
}

async fn export_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 导出数据：仅管理员与财务主管（防止把整表数据带出）
    user.require(Perm::Export)?;
    let (rows, _totals) = trial_balance_data(&state, &user, &q)?;
    let csv = trial_balance_csv(&rows);
    let body = csv.into_bytes();
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"trial_balance.csv\"",
            ),
        ],
        body,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// 报表渲染
// ---------------------------------------------------------------------------

fn trial_balance_html(
    state: &WebState,
    q: &HashMap<String, String>,
    rows: &[fincore::balance::BalanceRow],
    totals: &fincore::balance::TrialBalance,
) -> String {
    let from = q.get("from").cloned().unwrap_or_default();
    let to = q.get("to").cloned().unwrap_or_default();
    let mut body = String::new();
    for r in rows {
        let (b_dir, b_amt) = r.begin_dir_amount();
        let (e_dir, e_amt) = r.end_dir_amount();
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td class='r'>{} {}</td>\
             <td class='r'>{}</td><td class='r'>{}</td>\
             <td class='r'>{} {}</td><td class='r'>{} {}</td></tr>",
            html_escape(&r.account_code),
            html_escape(&r.account_name),
            b_dir.label(),
            b_amt.fmt_money(),
            r.debit.fmt_money(),
            r.credit.fmt_money(),
            e_dir.label(),
            e_amt.fmt_money(),
            r.ytd_debit.fmt_money(),
            r.ytd_credit.fmt_money(),
        ));
    }
    format!(
        "<!doctype html><html lang='zh-CN'><head><meta charset='utf-8'>\
         <title>科目余额表</title>\
         <style>body{{font-family:-apple-system,'Microsoft YaHei',sans-serif;color:#222;}}\
         h2{{text-align:center;margin:8px 0;}}.meta{{text-align:center;color:#666;font-size:13px;}}\
         table{{border-collapse:collapse;width:100%;margin-top:12px;font-size:13px;}}\
         th,td{{border:1px solid #bbb;padding:4px 8px;}}\
         th{{background:#f0f3f7;}}td.r{{text-align:right;}}\
         tfoot td{{font-weight:bold;background:#fafafa;}}\
         @media print{{body{{font-size:12px;}}}}</style></head>\
         <body><h2>{} 科目余额表</h2>\
         <div class='meta'>期间：{} 至 {}　打印时间：{}</div>\
         <table><thead><tr>\
         <th>科目编码</th><th>科目名称</th><th>期初</th>\
         <th>本期借方</th><th>本期贷方</th><th>期末</th><th>本年累计</th>\
         </tr></thead><tbody>{}</tbody>\
         <tfoot><tr><td colspan='2'>合计</td>\
         <td class='r'>借 {} / 贷 {}</td>\
         <td class='r'>{}</td><td class='r'>{}</td>\
         <td class='r'>借 {} / 贷 {}</td>\
         <td class='r'>—</td></tr></tfoot></table>\
         <script>window.onload=function(){{setTimeout(function(){{window.print();}},300);}};</script>\
         </body></html>",
        html_escape(&state.company),
        html_escape(&from),
        html_escape(&to),
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        body,
        totals.begin_debit.fmt_money(),
        totals.begin_credit.fmt_money(),
        totals.period_debit.fmt_money(),
        totals.period_credit.fmt_money(),
        totals.end_debit.fmt_money(),
        totals.end_credit.fmt_money(),
    )
}

fn trial_balance_csv(rows: &[fincore::balance::BalanceRow]) -> String {
    let mut s = String::from("科目编码,科目名称,期初方向,期初余额,本期借方,本期贷方,期末方向,期末余额,本年累计借方,本年累计贷方\n");
    for r in rows {
        let (b_dir, b_amt) = r.begin_dir_amount();
        let (e_dir, e_amt) = r.end_dir_amount();
        s.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{}\n",
            r.account_code,
            csv_escape(&r.account_name),
            b_dir.label(),
            b_amt.fmt_plain(),
            r.debit.fmt_plain(),
            r.credit.fmt_plain(),
            e_dir.label(),
            e_amt.fmt_plain(),
            r.ytd_debit.fmt_plain(),
            r.ytd_credit.fmt_plain(),
        ));
    }
    s
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn csv_escape(s: &str) -> String {
    // 防公式注入：以 = + - @ 开头的单元格在 Excel 里会被当公式执行
    let guarded = if s.starts_with('=') || s.starts_with('+') || s.starts_with('-') || s.starts_with('@') {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if guarded.contains(',') || guarded.contains('"') || guarded.contains('\n') {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

fn parse_voucher_status(s: &str) -> Option<fincore::VoucherStatus> {
    match s {
        "draft" => Some(fincore::VoucherStatus::Draft),
        "audited" => Some(fincore::VoucherStatus::Audited),
        "posted" => Some(fincore::VoucherStatus::Posted),
        "void" => Some(fincore::VoucherStatus::Void),
        _ => None,
    }
}
