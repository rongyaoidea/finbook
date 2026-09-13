//! HTTP 请求处理函数

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use axum::routing::{delete, get, post, put};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use fincore::{Account, AuxEntity, AuxKind, AuxMask, AuxQuery, AuxRef, BookOptions, Direction, Entry, Money, Period, Role, User, Voucher, VoucherStatus};
use fincore::user::Perm;
use findb::accounts;
use findb::advanced;
use findb::{auxs, business, template};
use findb::balances::{self, BalanceSnapshot, BalanceQuery, BeginRow, LedgerQuery};
use findb::Db;
use findb::periods;
use findb::security;
use findb::users;
use findb::vouchers::{self, VoucherQuery};
use serde_json::json;

use crate::dto::*;
use crate::realm::RealmDb;
use crate::state::{
    period_to_str, parse_money, parse_period, clear_cookie_header, AppError, CurrentUser, RealmUser,
    WebState,
};

const SESSION_SECS: i64 = 60 * 60 * 24 * 7;
/// 单账号最多可自建账套数（防无限建账占满磁盘）
const MAX_BOOKS_PER_USER: i64 = 10;

/// 组装路由
pub fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/api/setup/status", get(get_setup_status))
        // 平台账套目录：列表（按归属过滤）+ 自建账套 + 选择当前账套
        .route("/api/books", get(list_books).post(create_book))
        .route("/api/books/:key/select", post(select_book))
        .route("/api/books/:key", delete(delete_book))
        .route("/api/login", post(post_login))
        .route("/api/logout", post(post_logout))
        .route("/api/me", get(get_me))
        .route("/api/change-password", post(post_change_password))
        // 平台账号管理（仅平台管理员，作用于全局身份库）
        .route(
            "/api/platform/users",
            get(list_platform_users).post(create_platform_user),
        )
        .route(
            "/api/platform/users/:username",
            put(update_platform_user).delete(delete_platform_user),
        )
        .route(
            "/api/platform/users/:username/reset-password",
            post(reset_platform_password),
        )
        .route(
            "/api/platform/users/:username/reset-device",
            post(reset_platform_device),
        )
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
        .route(
            "/api/users/:username/unlock",
            post(unlock_user),
        )
        .route("/api/roles", get(list_roles))
        // 账套参数 / 仪表盘 / 期间
        .route("/api/options", get(get_options).put(put_options))
        .route("/api/dashboard", get(get_dashboard))
        .route("/api/overview", get(get_overview))
        .route("/api/periods", get(get_periods))
        .route("/api/period", post(post_period))
        // 科目 / 凭证
        .route("/api/accounts", get(list_accounts))
        .route("/api/accounts/fill-defaults", post(fill_default_accounts))
        .route("/api/vouchers/next-no", get(next_voucher_no))
        .route("/api/vouchers", get(list_vouchers).post(save_voucher))
        .route("/api/vouchers/batch-post", post(voucher_batch_post))
        .route("/api/vouchers/:id", get(get_voucher))
        .route("/api/vouchers/:id/post", post(voucher_post))
        .route("/api/vouchers/:id/unpost", post(voucher_unpost))
        .route("/api/vouchers/:id/reverse", post(voucher_reverse))
        .route("/api/vouchers/:id/delete", post(voucher_delete))
        .route("/api/vouchers/renumber", post(voucher_renumber))
        // 发票管理
        .route("/api/invoices", get(list_invoices).post(create_invoice))
        .route("/api/invoices/summary", get(invoice_summary))
        .route(
            "/api/invoices/:id",
            put(update_invoice).delete(delete_invoice),
        )
        .route("/api/invoices/:id/status", post(invoice_set_status))
        // 数据导入（其他软件 / CSV）
        .route("/api/import/analyze", post(import_analyze))
        .route("/api/import/run", post(import_run))
        // 账簿 / 报表
        .route("/api/ledger", get(get_ledger))
        .route("/api/ledger/print-form", get(print_ledger_form))
        .route("/api/vouchers/print-form", get(print_voucher_form))
        .route("/api/reports/trial-balance", get(get_trial_balance))
        .route(
            "/api/reports/trial-balance/print",
            get(print_trial_balance),
        )
        .route(
            "/api/reports/trial-balance/export",
            get(export_trial_balance),
        )
        .route(
            "/api/reports/trial-balance/pdf",
            get(export_trial_balance_pdf),
        )
        // 高级功能：多栏账 / 摘要汇总表 / 财务指标
        .route("/api/reports/multi-column", get(get_multi_column))
        .route("/api/reports/summary-table", get(get_summary_table))
        .route("/api/reports/ratios", get(get_fin_ratios))
        // 财务核心：三大报表（资产负债表/利润表/现金流量表）
        .route("/api/reports/balance-sheet", get(get_balance_sheet))
        .route("/api/reports/balance-sheet/print", get(print_balance_sheet))
        .route("/api/reports/income-statement", get(get_income_statement))
        .route("/api/reports/income-statement/print", get(print_income_statement))
        .route("/api/reports/cash-flow", get(get_cash_flow))
        .route("/api/reports/cash-flow/print", get(print_cash_flow))
        // 财务核心：所有者权益变动表 / 报表对比 / 科目日报表 / 期末对账
        .route("/api/reports/equity", get(get_equity_statement))
        .route("/api/reports/equity/print", get(print_equity_statement))
        .route("/api/reports/compare", get(get_report_compare))
        .route("/api/reports/daily", get(get_account_daily))
        .route("/api/reports/reconcile", get(get_period_reconcile))
        // 存货核算：成本调整
        .route("/api/inventory/adjust", post(stock_adjust_endpoint))
        // 库存深度：序列号 / 多单位 / 账龄 / ABC / 组装拆卸 / 分仓库
        .route("/api/inventory/serial", get(list_serial).post(serial_in_endpoint))
        .route("/api/inventory/serial/out", post(serial_out_endpoint))
        .route("/api/inventory/unit", get(get_unit).post(set_unit))
        .route("/api/inventory/aging", get(get_inv_aging))
        .route("/api/inventory/abc", get(get_abc))
        .route("/api/inventory/assemble", post(assemble_endpoint))
        .route("/api/inventory/disassemble", post(disassemble_endpoint))
        .route("/api/inventory/warehouse-stock", get(get_warehouse_stock))
        .route("/api/inventory/transfer", get(get_transfer_report))
        // 采购/销售深度：暂估 / 对账 / 配额 / 订单变更
        .route("/api/procure/estimate", get(list_estimates).post(add_estimate))
        .route("/api/procure/estimate/:id/settle", post(settle_estimate))
        .route("/api/procure/reconcile", get(get_po_reconcile))
        .route("/api/procure/quota", get(get_quota).post(set_quota))
        .route("/api/sales/reconcile", get(get_so_reconcile))
        .route("/api/order/change-log", get(get_change_log))
        // 采购/销售全生命周期：请购 / 报价 / 到货 / 发货 / 付款 / 收款 / 退货 / 信用
        .route("/api/procure/req", get(list_purchase_req).post(save_purchase_req))
        .route("/api/procure/req/:id/approve", post(approve_purchase_req))
        .route("/api/procure/receipt", post(add_po_receipt))
        .route("/api/procure/payment", post(add_po_payment))
        .route("/api/procure/return", post(add_po_return))
        .route("/api/procure/price", get(get_price_history))
        .route("/api/procure/track", get(get_po_track))
        .route("/api/procure/stats", get(get_purchase_stats))
        .route("/api/sales/quote", get(list_quotation).post(save_quotation))
        .route("/api/sales/quote/:id/approve", post(approve_quotation))
        .route("/api/sales/shipment", post(add_so_shipment))
        .route("/api/sales/payment", post(add_so_payment))
        .route("/api/sales/return", post(add_so_return))
        .route("/api/sales/credit", get(get_credit_check))
        .route("/api/sales/track", get(get_so_track))
        .route("/api/sales/stats", get(get_sales_stats))
        // 预算预警
        .route("/api/budget/alerts", get(get_budget_alerts))
        // 坏账准备计提
        .route("/api/settle/bad-debt/provision", post(bad_debt_provision_endpoint))
        // 工艺路线 / 报工 / MRP
        .route("/api/routing/:item", get(get_routing).post(post_routing))
        .route("/api/routing/:item/delete", post(delete_routing))
        .route("/api/prod", get(list_prod_orders))
        .route("/api/prod/:id/ops", get(get_prod_ops))
        .route("/api/prod/op/report", post(report_prod_op))
        .route("/api/prod/op/finish", post(finish_prod_op))
        .route("/api/mrp/latest", get(get_mrp_latest))
        .route("/api/mrp/run", post(run_mrp))
        // 预算版本
        .route("/api/budget/versions", get(list_budget_versions).post(save_budget_version))
        .route("/api/budget/versions/:key/delete", post(delete_budget_version))
        .route("/api/budget/versions/:key/activate", post(activate_budget_version))
        .route("/api/budget/versions/copy", post(copy_budget_version))
        // 审批流
        .route("/api/approvals", get(list_approvals).post(start_approval))
        .route("/api/approvals/todo", get(list_approval_todo))
        .route("/api/approvals/:id", get(get_approval))
        .route("/api/approvals/:id/act", post(act_approval))
        .route("/api/approvals/:id/cancel", post(cancel_approval))
        // 报表附注
        .route("/api/reports/notes", get(list_notes).post(save_note))
        .route("/api/reports/notes/:id/delete", post(delete_note))
        // 会计电子档案
        .route("/api/archives", get(list_archives).post(create_archive))
        .route("/api/archives/:id", get(get_archive))
        .route("/api/archives/:id/verify", get(verify_archive))
        .route("/api/health", get(|| async { "ok" }))
        // 资金：票据 / 融资 / 资金日报 / 资金预测
        .route("/api/funds/bills", get(list_bills).post(save_bill))
        .route("/api/funds/bills/:id/status", post(bill_transition))
        .route("/api/funds/bills/:id/delete", post(delete_bill))
        .route("/api/funds/loans", get(list_loans).post(save_loan))
        .route("/api/funds/loans/:id/settle", post(loan_settle))
        .route("/api/funds/loans/:id/delete", post(delete_loan))
        .route("/api/funds/daily", get(get_funds_daily))
        .route("/api/funds/forecast", get(get_funds_forecast))
        // 预算分析
        .route("/api/budget/analysis", get(get_budget_analysis))
        // 成本：计价配置 + 期末结价
        .route("/api/cost/configs", get(list_cost_configs).post(save_cost_method))
        .route("/api/cost/configs/:item/delete", post(clear_cost_method))
        .route("/api/cost/period-end", get(run_period_end_cost).post(run_period_end_cost))
        // ---- 账套内基础资料与系统功能（对齐桌面端 finui 补齐）----
        .route("/api/accounts", post(create_account).put(update_account))
        .route("/api/accounts/:code", delete(delete_account))
        .route("/api/begin", get(list_begin).post(save_begin))
        .route("/api/logs", get(list_logs))
        .route("/api/backups", get(list_backups).post(create_backup))
        .route("/api/restore", post(restore_backup))
        .route("/api/templates", get(list_templates).post(create_template))
        .route("/api/templates/due", get(due_templates))
        .route("/api/templates/:id", put(update_template).delete(delete_template))
        .route("/api/templates/:id/generate", post(generate_template))
        .route("/api/aux", get(list_aux).post(create_aux))
        .route("/api/aux/:id", put(update_aux).delete(delete_aux))
        .route("/api/payroll", get(list_payroll).post(save_payroll))
        .route("/api/payroll/generate", post(generate_payroll))
        .route("/api/payroll/ytd", get(payroll_ytd))
        .route("/api/payroll/accrue", post(payroll_accrue))
        .route("/api/payroll/social-pay", post(payroll_social_pay))
        .route("/api/payroll/pay", post(payroll_pay))
        .route("/api/payroll/:id", delete(delete_payroll))
        .route("/api/claims", get(list_claims).post(create_claim))
        .route("/api/claims/next-no", get(next_claim_no))
        .route("/api/claims/:id", put(update_claim).delete(delete_claim))
        .route("/api/claims/:id/transition", post(claim_transition))
        .route("/api/claims/:id/voucher", post(claim_voucher))
        // SPA 首页：动态注入资源版本号，避免浏览器长期缓存旧版 JS/CSS
        .route("/", get(serve_index))
        .with_state(state)
}

/// 返回 SPA 首页，并把 `{{V}}` 占位符替换为当前资源版本号。
/// 版本号由静态文件 mtime 计算，任何前端改动都会使 URL 变化 → 浏览器缓存自动失效。
async fn serve_index(State(state): State<Arc<WebState>>) -> Response {
    use axum::response::Html;
    let path = state.static_dir.join("index.html");
    let html = match std::fs::read_to_string(&path) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("index.html 读取失败：{e}"),
            )
                .into_response()
        }
    };
    Html(html.replace("{{V}}", &state.assets_ver)).into_response()
}

// ---------------------------------------------------------------------------
// 认证 / 初始化状态
// ---------------------------------------------------------------------------

async fn get_setup_status(State(state): State<Arc<WebState>>) -> Result<Json<SetupStatus>, AppError> {
    // 平台管理员是否已初始化（首次启动已引导）
    let admin_set = state.realm.count_users()? > 0;
    Ok(Json(SetupStatus {
        admin_set,
        needs_setup: false,
        company: String::new(),
        version: state.version.clone(),
        book: None,
    }))
}

/// 账套列表（需登录）：管理员返回全部，普通用户只返回自己创建的
/// 同时返回平台身份摘要（前端在"已登录未选账套"状态下据此渲染选择页）
async fn list_books(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let books = state.realm.list_books_for(&user.username, user.is_admin)?;
    let items: Vec<serde_json::Value> = books
        .iter()
        .map(|b| json!({ "key": b.key, "company": b.company, "owner": b.owner_username }))
        .collect();
    Ok(Json(json!({
        "user": {
            "username": user.username,
            "display_name": user.display_name,
            "is_admin": user.is_admin,
            "must_change_pwd": user.must_change_pwd,
        },
        "books": items,
    })))
}

async fn post_login(
    State(state): State<Arc<WebState>>,
    Json(req): Json<LoginReq>,
) -> Result<Response, AppError> {
    // 全局登录（认平台身份库，而非某一套账）
    let username = req.username.trim().to_string();
    // 设备指纹必须非空：空串会让"一人一机"首次绑定写成空值从而永久绕过校验
    let device_id = req.device_id.trim().to_string();
    if device_id.is_empty() || device_id.len() > 128 {
        return Err(AppError::bad_request("缺少有效的设备标识，请刷新页面后重试"));
    }
    // 登录限流：先查是否已被锁定，避免锁定期内继续做昂贵/可枚举的密码校验
    if let Err(secs) = state.login_limiter.check(&username) {
        let mins = secs.div_ceil(60).max(1);
        return Err(AppError::rate_limited(
            format!("尝试过于频繁，请约 {mins} 分钟后再试"),
            secs,
        ));
    }
    let ru = match state.realm.authenticate(&username, &req.password) {
        Ok(Some(ru)) => ru,
        Ok(None) => {
            state.login_limiter.record_failure(&username);
            return Err(AppError::unauthorized("用户名或口令错误"));
        }
        Err(e) => return Err(AppError::from(e)),
    };
    // 登录成功，清空该账号的失败计数
    state.login_limiter.clear(&username);
    // "一人一机"（平台层）：普通账号绑定首个登录设备，换设备需管理员重置；管理员可多端
    if !ru.is_admin {
        match state.realm.bind_device(&username, &device_id)? {
            Ok(()) => {}
            Err(msg) => return Err(AppError::forbidden(msg)),
        }
    }
    // 普通账号登录时踢掉旧会话（一人一会话）；管理员不受限，可多端并存
    if !ru.is_admin {
        state.sessions.remove_by_username(&username);
    }
    let token = state.sessions.create(
        &username,
        ru.is_admin,
        &device_id,
        state.default_period,
        "", // 账套登录后由"选择账套"设定
    );
    // 返回该用户可进入的账套列表（管理员=全部，普通=本人创建）
    let books = state.realm.list_books_for(&username, ru.is_admin)?;
    let book_list: Vec<serde_json::Value> = books
        .iter()
        .map(|b| json!({ "key": b.key, "company": b.company, "owner": b.owner_username }))
        .collect();
    let resp = LoginResp {
        user: PlatformUser {
            username: ru.username,
            display_name: ru.display_name,
            is_admin: ru.is_admin,
            must_change_pwd: ru.must_change_pwd,
        },
        must_change_pwd: ru.must_change_pwd,
        setup: false,
        books: book_list,
    };
    let mut r = Json(resp).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, crate::state::cookie_header(&token, SESSION_SECS));
    Ok(r)
}

/// 生成唯一账套 key（文件名，不含扩展名）
fn make_book_key(raw: &str, owner: &str, realm: &RealmDb) -> Result<String, AppError> {
    let now = chrono::Local::now();
    let base = if raw.trim().is_empty() {
        format!("{}_{:04}{:02}", owner, now.year(), now.month())
    } else {
        let cleaned: String = raw
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let trimmed = cleaned.trim_matches('_').to_string();
        if trimmed.is_empty() {
            format!("{}_{:04}{:02}", owner, now.year(), now.month())
        } else {
            trimmed
        }
    };
    // 唯一化：若冲突则在末尾追加序号
    let mut key = base.clone();
    let mut n = 1;
    while realm.get_book(&key)?.is_some() {
        n += 1;
        key = format!("{base}_{n}");
    }
    Ok(key)
}

/// 普通用户自建账套：创建者为该账套的所有者，并成为账套内管理员
async fn create_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<CreateBookReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let owner = user.username.clone();
    // 限制每账号账套数量，防止无限建账占满磁盘
    let owned = state.realm.count_books_of(&owner)?;
    if owned >= MAX_BOOKS_PER_USER {
        return Err(AppError::bad_request(format!(
            "每个账号最多创建 {MAX_BOOKS_PER_USER} 个账套（当前已创建 {owned} 个）"
        )));
    }
    let company = req.company.trim().to_string();
    let key = make_book_key(&req.key, &owner, &state.realm)?;
    let path = state.books_dir.join(format!("{key}.fbk"));
    if path.exists() {
        return Err(AppError::bad_request("账套文件已存在"));
    }
    let mut opts = BookOptions::default();
    if !company.is_empty() {
        opts.company = company.clone();
    }
    if req.start_period > 0 {
        opts.start_period = period_checked(req.start_period)?;
    }
    let db = Db::create_no_admin(&path, &opts)?;
    // 种子所有者为账套内管理员（复制平台口令哈希，便于必要时直接登账套）
    let mut u = User::new(&owner, &user.display_name, Role::Admin);
    if let Ok(Some(ru)) = state.realm.get_user(&owner) {
        u.password_hash = ru.password_hash;
    }
    users::insert(&db, &u)?;
    drop(db);
    // 注册进运行期账套表 + 平台账套目录
    state.books.register(&path, 16);
    state
        .realm
        .register_book(&key, &path.to_string_lossy(), &owner, &company)?;
    Ok(Json(json!({ "key": key, "company": company })))
}

/// 选择当前账套（登录后进入某套账前调用）
async fn select_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let book = state
        .realm
        .get_book(&key)?
        .ok_or_else(|| AppError::not_found("账套不存在"))?;
    // 授权三层：平台管理员 / 归属者 / 账套内已有该用户的行（被邀请成员）。
    // 注意：账套打不开（文件损坏等）必须透出 500，不能吞成"无权访问"误导用户。
    let is_member = match state.db_for(&key) {
        Ok(db) => users::get(&db, &user.username).ok().flatten().is_some(),
        Err(e) => return Err(e.into()),
    };
    let allowed =
        user.is_admin || book.owner_username == user.username || is_member;
    if !allowed {
        return Err(AppError::forbidden("无权访问该账套"));
    }
    // 进入账套时把会话当前期间同步为该账套启用期间
    if let Ok(db) = state.db_for(&key) {
        let ymm = db.options().start_period.ymm();
        state.sessions.set_period(&user.token, ymm);
        drop(db);
    }
    state.sessions.set_book_key(&user.token, &key);
    Ok(Json(json!({ "ok": true })))
}

/// 删除账套（平台管理员 或 账套归属者）：解除「有账套的用户无法删除」的死锁
async fn delete_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let book = state
        .realm
        .get_book(&key)?
        .ok_or_else(|| AppError::not_found("账套不存在"))?;
    if !user.is_admin && book.owner_username != user.username {
        return Err(AppError::forbidden("无权删除该账套"));
    }
    // 顺序要点：先摘运行期注册（否则后续请求会把已删文件重新打开成空库），
    // 再清会话、删目录记录，最后落盘删除文件。
    state.books.unregister(&key);
    state.sessions.clear_book_key(&key);
    state.realm.delete_book(&key)?;
    let p = std::path::PathBuf::from(&book.path);
    if p.exists() {
        if let Err(e) = std::fs::remove_file(&p) {
            eprintln!("[finweb] 删除账套文件失败 {}: {e}", p.display());
        }
        let _ = std::fs::remove_file(format!("{}-wal", p.display()));
        let _ = std::fs::remove_file(format!("{}-shm", p.display()));
    }
    Ok(Json(json!({ "ok": true })))
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
    user: CurrentUser,
) -> Result<Json<PublicUser>, AppError> {
    // 直接使用身份对账后的用户：平台管理员查看他人账套时是临时身份
    // （不写入账套 user 表），回查数据库会 404
    Ok(Json(PublicUser::from_user(&user.user)))
}

/// 凡是要把口令写进存储的入口，都必须先过这道校验。
///
/// 单密码统一后所有口令入口都走应用级 PasswordPolicy（默认 8 位且需含字母+数字）：
/// Web 层先由本函数做 400 校验，realm 层再以 policy 参数复核，双层一致。
fn check_password(state: &WebState, pwd: &str) -> Result<(), AppError> {
    state.policy.check(pwd).map_err(AppError::bad_request)
}

async fn post_change_password(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<ChangePwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 改的是平台口令（与登录身份一致）
    check_password(&state, &req.new)?;
    let r = state.realm.change_password(&user.username, &req.old, &req.new, &state.policy)?;
    match r {
        Ok(()) => {
            // 同步到该用户出现过的所有账套内的同名用户行，保持单密码一致
            if let Ok(Some(ru)) = state.realm.get_user(&user.username) {
                let _ = state.realm.sync_password_to_books(
                    &state.books_dir,
                    &user.username,
                    &ru.password_hash,
                    ru.must_change_pwd,
                );
            }
            // 口令已变：吊销本人其他设备上的旧会话，当前设备保留
            state.sessions.remove_others(&user.username, &user.token);
            Ok(Json(json!({"ok": true})))
        }
        Err(msg) => Err(AppError::bad_request(msg)),
    }
}

// ---------------------------------------------------------------------------
// 平台账号管理（仅平台管理员）
// ---------------------------------------------------------------------------

async fn list_platform_users(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<Vec<PlatformUserItem>>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限平台管理员"));
    }
    let list = state
        .realm
        .list_users()?
        .into_iter()
        .map(|u| PlatformUserItem::from_realm(&u))
        .collect();
    Ok(Json(list))
}

async fn create_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<PlatformUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限平台管理员"));
    }
    if req.username.trim().is_empty() {
        return Err(AppError::bad_request("用户名不能为空"));
    }
    check_password(&state, &req.password)?;
    // 管理员开的号，口令是管理员定的——首次登录必须自己改一次
    let id = state
        .realm
        .create_user(&req.username, &req.display_name, &req.password, req.is_admin, true, &state.policy)?;
    Ok(Json(json!({ "id": id })))
}

async fn update_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
    Json(req): Json<UpdatePlatformUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限平台管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request("不能修改当前登录的账号，请使用改密功能"));
    }
    state
        .realm
        .update_user(&username, req.display_name.as_deref(), req.disabled, req.is_admin)?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限平台管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request("不能删除当前登录的账号"));
    }
    if state.realm.count_books_of(&username)? > 0 {
        return Err(AppError::bad_request("该用户仍拥有账套，请先删除其账套后再删除账号"));
    }
    state.realm.delete_user(&username)?;
    state.sessions.remove_by_username(&username);
    Ok(Json(json!({"ok": true})))
}

async fn reset_platform_password(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
    Json(req): Json<ResetPwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限平台管理员"));
    }
    check_password(&state, &req.new)?;
    state.realm.reset_password(&username, &req.new, &state.policy)?;
    if let Ok(Some(ru)) = state.realm.get_user(&username) {
        let _ = state.realm.sync_password_to_books(
            &state.books_dir,
            &username,
            &ru.password_hash,
            ru.must_change_pwd,
        );
    }
    Ok(Json(json!({"ok": true})))
}

/// 平台层重置设备绑定（Web"一人一机"）：解绑后该账号下次登录自动绑定新设备
async fn reset_platform_device(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限平台管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request("不能重置当前登录账号的设备"));
    }
    state.realm.clear_device(&username)?;
    // 强制重新登录：旧会话不能再沿用
    state.sessions.remove_by_username(&username);
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// 用户管理（UserManage）
// ---------------------------------------------------------------------------

async fn list_users(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<PublicUser>>, AppError> {
    user.require(Perm::UserManage)?;
    let db = state.db_for(&user.book_key)?;
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
    if username.is_empty() {
        return Err(AppError::bad_request("用户名不能为空"));
    }
    // Web 端登录只认平台身份库：账套内子账号必须对应一个已存在的平台账号，
    // 否则开出来的账号无法登录（死账号）。单密码统一后账套内不再独立设口令，
    // 直接沿用平台口令哈希，前端若传了 password 则忽略（兼容旧前端）。
    let ru = state.realm.get_user(&username)?.ok_or_else(|| {
        AppError::bad_request(
            "该账号尚未开通平台账号，请先让平台管理员在「平台账号」中开通同名账号",
        )
    })?;
    // 账套管理员不能把平台管理员拉进自己的账套：账套内重置口令会重置平台口令，
    // 否则任何能建账的用户都能借此接管平台管理员账号。
    if ru.is_admin {
        return Err(AppError::forbidden("不能将平台管理员加入账套"));
    }
    let db = state.db_for(&user.book_key)?;
    if users::get(&db, &username)?.is_some() {
        return Err(AppError::bad_request("该用户名已存在"));
    }
    let mut u = User::new(&username, &req.display_name, req.role);
    u.password_hash = ru.password_hash.clone();
    // 管理员开的号，口令是管理员定的——首次登录必须自己改一次
    u.must_change_pwd = req.must_change_pwd;
    u.memo = req.memo;
    // 普通账户默认只能看自己填制的凭证（防越权翻看他人/全盘数据）；
    // 管理员不受此限制，可看到所有账套数据
    if !u.is_admin() {
        u.data_scope.own_voucher_only = true;
    }
    // 角色基础上的逐项覆盖（管理员角色忽略，避免把自己锁在门外）
    if !u.is_admin() {
        u.extra_perms = req.extra_perms;
        u.deny_perms = req.deny_perms;
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
    let db = state.db_for(&user.book_key)?;
    let mut u = users::get(&db, &username)?
        .ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    // 账套归属者不可被停用 / 降权 / 改名，否则账套会失去主人
    if let Some(owner) = state.realm.book_owner(&user.book_key)? {
        if username == owner {
            return Err(AppError::bad_request("不能修改账套归属者的账套内身份"));
        }
    }
    // 不能改自己的授权字段：`UserManage` 是一个可单独授予主管的权限，若能改自己，
    // 把 role 设成 Admin 或清空 deny_perms 就是一次自我提权。显示名/备注不涉及授权，
    // 允许自助修改。
    if username == user.username() {
        let touches_grant = req.role.is_some()
            || req.extra_perms.is_some()
            || req.deny_perms.is_some()
            || req.data_scope.is_some()
            || req.disabled == Some(true);
        if touches_grant {
            return Err(AppError::bad_request(
                "不能修改自己的角色、权限矩阵或停用本人，请由其他管理员操作",
            ));
        }
    }
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
    if let Some(m) = req.memo {
        u.memo = m;
    }
    if let Some(s) = req.data_scope {
        u.data_scope = s;
    }
    if let Some(p) = req.extra_perms {
        u.extra_perms = p;
    }
    if let Some(p) = req.deny_perms {
        u.deny_perms = p;
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
    // 单密码统一：账套内没有独立口令，重置即重置该用户的平台口令，
    // 再同步到其出现过的所有账套。目标必须是当前账套成员，防越权重置陌生人口令。
    let db = state.db_for(&user.book_key)?;
    if users::get(&db, &username)?.is_none() {
        return Err(AppError::not_found("该用户不在当前账套"));
    }
    let ru = state.realm.get_user(&username)?.ok_or_else(|| {
        AppError::bad_request("该账号尚未开通平台账号")
    })?;
    // 平台管理员的口令只能由平台管理员在「平台账号」中重置。账套管理员若能把
    // 平台管理员邀请进本套再调本接口，就能重置其平台口令并同步到全部账套。
    if ru.is_admin {
        return Err(AppError::forbidden(
            "平台管理员的口令请由平台管理员在「平台账号」中重置",
        ));
    }
    check_password(&state, &req.new)?;
    state.realm.reset_password(&username, &req.new, &state.policy)?;
    // 口令已变，立即吊销该账号的全部旧会话（被盗会话不能继续用满 7 天）
    state.sessions.remove_by_username(&username);
    if let Ok(Some(ru)) = state.realm.get_user(&username) {
        let _ = state.realm.sync_password_to_books(
            &state.books_dir,
            &username,
            &ru.password_hash,
            ru.must_change_pwd,
        );
    }
    db.log(user.username(), "安全", "重置口令", &format!("重置「{username}」的口令（平台口令）"))?;
    Ok(Json(json!({"ok": true})))
}

async fn reset_user_device(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let db = state.db_for(&user.book_key)?;
    users::reset_device(&db, &username)?;
    // 立刻下线该用户全部会话：旧设备不能靠存量会话绕过"一人一机"
    state.sessions.remove_by_username(&username);
    db.log(user.username(), "安全", "重置设备绑定", &format!("重置「{username}」的设备绑定"))?;
    Ok(Json(json!({"ok": true})))
}

async fn unlock_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let db = state.db_for(&user.book_key)?;
    security::unlock_user(&db, &username)?;
    db.log(user.username(), "安全", "解锁用户", &format!("解锁「{username}」"))?;
    Ok(Json(json!({"ok": true})))
}

/// 角色 → 权限矩阵（供前端在新建/改角色时实时预览）
async fn list_roles(user: CurrentUser) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let roles: Vec<serde_json::Value> = Role::all()
        .iter()
        .map(|r| {
            let perms: Vec<serde_json::Value> = r
                .perms()
                .iter()
                .map(|p| {
                    let code = serde_json::to_value(*p)
                        .ok()
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    json!({ "code": code, "label": p.label() })
                })
                .collect();
            json!({ "role": serde_json::to_value(*r).ok().and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default(), "label": r.label(), "perms": perms })
        })
        .collect();
    Ok(Json(json!(roles)))
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
    let db = state.db_for(&user.book_key)?;
    let u = users::get(&db, &username)?
        .ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    // 账套归属者不可删除（删了账套就没有主人了）
    if let Some(owner) = state.realm.book_owner(&user.book_key)? {
        if username == owner {
            return Err(AppError::bad_request("不能删除账套归属者"));
        }
    }
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
    user: CurrentUser,
) -> Result<Json<fincore::BookOptions>, AppError> {
    user.require(Perm::SysOption)?;
    let db = state.db_for(&user.book_key)?;
    Ok(Json(db.options()))
}

async fn put_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(opts): Json<fincore::BookOptions>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = state.db_for(&user.book_key)?;
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

/// 校验用户传入的期间（YYYYMM）：非法值返回 400。
/// 直接用 `Period::from_ymm` 会让非法期间一路流到 `first_day()/last_day()` 处 panic，
/// 在 `panic = "abort"` 的 release 下等于把整个服务打挂。
fn period_checked(ymm: i32) -> Result<Period, AppError> {
    Period::from_ymm_checked(ymm).map_err(|e| AppError::bad_request(e.to_string()))
}

async fn get_dashboard(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Dashboard>, AppError> {
    let db = state.db_for(&user.book_key)?;
    let (v, e, a) = db.stats()?;
    let start = db.options().start_period;
    let cur = current_period(&state, &user);
    let closed = periods::closed_upto(&db)?;
    Ok(Json(Dashboard {
        company: user.company.clone(),
        start_period: period_to_str(start),
        current_period: period_to_str(cur),
        closed_upto: closed.map(period_to_str),
        vouchers: v,
        entries: e,
        accounts: a,
    }))
}

/// 管理员「账目总览」：只读视角的账目全貌（仅系统管理员可访问）
async fn get_overview(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.user.is_admin() {
        return Err(AppError::forbidden("该入口仅限系统管理员使用"));
    }
    let db = state.db_for(&user.book_key)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let o = findb::reports::overview(&db, period)?;
    let recent: Vec<VoucherListItem> = o.recent.iter().map(to_item).collect();
    let a = findb::advanced::financial_analysis(&db, period, Some(&user.user))?;
    let trend: Vec<serde_json::Value> = a
        .trend
        .iter()
        .map(|t| {
            json!({
                "period": period_to_str(t.period),
                "revenue": t.revenue.fmt_money(),
                "cost": t.cost.fmt_money(),
                "net_profit": t.net_profit.fmt_money(),
                "cum_revenue": t.cum_revenue.fmt_money(),
                "cum_cost": t.cum_cost.fmt_money(),
                "cum_net_profit": t.cum_net_profit.fmt_money(),
                "anomaly_revenue": t.anomaly_revenue,
                "anomaly_cost": t.anomaly_cost,
                "anomaly_net_profit": t.anomaly_net_profit,
            })
        })
        .collect();
    let driver_json = |d: &findb::advanced::DriverItem| {
        json!({
            "name": d.name,
            "amount": d.amount.fmt_money(),
            "prev_amount": d.prev_amount.fmt_money(),
        })
    };
    Ok(Json(json!({
        "company": o.company,
        "period": period_to_str(o.period),
        "closed_upto": o.closed_upto.map(period_to_str),
        "vouchers": o.vouchers,
        "entries": o.entries,
        "accounts": o.accounts,
        "unposted": o.unposted,
        "posted": o.posted,
        "totals": {
            "total_asset": o.totals.total_asset.fmt_money(),
            "total_liab": o.totals.total_liab.fmt_money(),
            "equity": o.totals.equity.fmt_money(),
            "revenue": o.totals.revenue.fmt_money(),
            "cost": o.totals.cost.fmt_money(),
            "net_profit": o.totals.net_profit.fmt_money(),
        },
        "invoice_in": {"amount_tax": o.invoice_in.0.fmt_money(), "count": o.invoice_in.1},
        "invoice_out": {"amount_tax": o.invoice_out.0.fmt_money(), "count": o.invoice_out.1},
        "recent": recent,
        "analysis": {
            "trend": trend,
            "revenue_drivers": a.revenue_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "cost_drivers": a.cost_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "profit_drivers": a.profit_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "anomaly_notes": a.anomaly_notes,
            "ratios": a.ratios.iter().map(|r| json!({
                "key": r.key, "name": r.name, "value": r.value.fmt_plain(), "display": r.display, "formula": r.formula,
            })).collect::<Vec<_>>(),
        },
    })))
}

async fn get_periods(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let db = state.db_for(&user.book_key)?;
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
    Ok(Json(json!({"ok": true, "period": period_to_str(period_checked(req.ymm)?)})))
}

// ---------------------------------------------------------------------------
// 科目 / 凭证
// ---------------------------------------------------------------------------

async fn list_accounts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<fincore::Account>>, AppError> {
    let db = state.db_for(&user.book_key)?;
    Ok(Json(accounts::list(&db)?))
}

/// 补齐内置科目表（旧账套补入新版本新增的科目，仅管理员）
async fn fill_default_accounts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.user.is_admin() {
        return Err(AppError::forbidden("该操作仅限系统管理员"));
    }
    let db = state.db_for(&user.book_key)?;
    let before = accounts::list(&db)?.len();
    let inserted = accounts::fill_missing_defaults(&db)?;
    if inserted > 0 {
        db.log(
            user.username(),
            "科目",
            "补齐科目表",
            &format!("补入 {inserted} 个内置科目（{before} → {}）", before + inserted),
        )?;
    }
    Ok(Json(json!({
        "inserted": inserted,
        "total": before + inserted,
    })))
}

async fn next_voucher_no(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
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
    let db = state.db_for(&user.book_key)?;
    // 落地数据范围：仅本人凭证或按科目区间过滤
    let mut query = VoucherQuery::default().with_data_scope(&user.user);
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
    let mut list = vouchers::list(&db, &query)?;
    // 列表需要摘要与借贷合计：`vouchers::list` 只读表头，这里批量补充分录
    vouchers::fill_entries(&db, &mut list)?;
    // 数据范围：科目区间等限制需逐张过滤（查询层只处理了「仅本人凭证」）
    list.retain(|v| user.user.can_see_voucher(v));
    let list = list.iter().map(to_item).collect();
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
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    // 数据权限：非全量权限用户不得查看自己不可见的凭证
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该凭证"));
    }
    Ok(Json(VoucherDetail::from_voucher(v)))
}

/// 解析银行科目：支持尾号简写。
/// - 输入已是完整科目编码（如 `100201`）：原样返回
/// - 输入是纯数字尾号（如 `01`）：在 `1002*` 科目中查找编码以该尾号结尾的唯一科目，
///   解析为完整编码；无唯一匹配则返回 None（交给后续科目校验报错）
fn normalize_bank_account(chart: &fincore::Chart, account_code: &str) -> Option<String> {
    let trimmed = account_code.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 完整编码：科目表里能查到就直接用
    if chart.get(trimmed).is_some() {
        return Some(trimmed.to_string());
    }
    // 纯数字尾号：在银行科目里按尾号匹配
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut hits: Vec<&str> = Vec::new();
    for a in chart.all() {
        if a.code.starts_with("1002") && a.code.ends_with(trimmed) && a.code.len() > 4 {
            hits.push(&a.code);
        }
    }
    if hits.len() == 1 {
        Some(hits[0].to_string())
    } else {
        None
    }
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
    let db = state.db_for(&user.book_key)?;
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
        // 数据权限校验：非全量权限用户不得修改自己不可见的凭证
        if !user.user.can_see_voucher(&existing) {
            return Err(AppError::forbidden("无权修改该凭证"));
        }
        if !existing.status.can_edit() {
            return Err(AppError::forbidden(
                "该凭证已记账或已作废，不能修改（已记账请先反记账）",
            ));
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
    // 加载科目表：银行尾号简写需要按科目表解析为完整编码
    let chart = accounts::chart(&db)?;
    for e in &req.entries {
        let account_code = normalize_bank_account(&chart, &e.account_code)
            .unwrap_or_else(|| e.account_code.clone());
        let mut en = Entry::new(e.line, account_code.clone(), e.summary.clone());
        en.debit = parse_money(&e.debit);
        en.credit = parse_money(&e.credit);
        en.aux = AuxRef::default();
        // 如果是银行科目，将尾号存入辅助核算银行字段
        if en.account_code.starts_with("1002") {
            en.aux.bank = Some(account_code);
        }
        v.entries.push(en);
    }
    if !v.balanced() {
        return Err(AppError::bad_request("借贷不平衡，请检查分录金额"));
    }
    // 保存为「未记账」，核对无误后在界面点「记账」确认入账（无审核环节）
    v.status = VoucherStatus::Draft;
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
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if v.status == VoucherStatus::Posted {
        return Ok(Json(json!({"ok": true, "already_posted": true})));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("该凭证不参与账簿汇总"));
    }
    vouchers::post(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

async fn voucher_batch_post(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BatchPostReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherPost)?;
    let db = state.db_for(&user.book_key)?;
    let (n, errs) = vouchers::post_many(&db, &req.ids, user.username())?;
    db.log(user.username(), "凭证", "批量记账", &format!("{n} 张"))?;
    Ok(Json(json!({ "ok": n, "errors": errs })))
}

async fn voucher_unpost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 反记账（已记账 → 未记账），修改前需先反记账
    user.require(Perm::VoucherUnpost)?;
    let db = state.db_for(&user.book_key)?;
    vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    vouchers::unpost(&db, id)?;
    Ok(Json(json!({"ok": true, "action": "unposted"})))
}

/// 红字冲销请求：冲销凭证落到的期间与日期
#[derive(Deserialize, Default)]
struct ReverseVoucherReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
}

async fn voucher_reverse(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ReverseVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权冲销该凭证"));
    }
    let period = if req.period > 0 {
        parse_period(&req.period.to_string()).unwrap_or_else(|| current_period(&state, &user))
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let nid = vouchers::reverse(&db, id, user.username(), period, date)?;
    Ok(Json(json!({"id": nid})))
}

/// 断号重排请求：period + word
#[derive(Deserialize, Default)]
struct RenumberVoucherReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub word: String,
}

async fn voucher_renumber(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<RenumberVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 {
        parse_period(&req.period.to_string()).unwrap_or_else(|| current_period(&state, &user))
    } else {
        current_period(&state, &user)
    };
    let word = if req.word.trim().is_empty() {
        "记".to_string()
    } else {
        req.word.trim().to_string()
    };
    let n = vouchers::renumber(&db, period, &word)?;
    Ok(Json(json!({"ok": true, "renumbered": n})))
}

async fn voucher_delete(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = state.db_for(&user.book_key)?;
    // 加载后做守卫校验：状态、结账期间、数据权限
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权删除该凭证"));
    }
    // 已记账凭证需先反记账才能删除（未记账凭证可直接删除）
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request(
            "已记账凭证不能直接删除，请先反记账",
        ));
    }
    // 期间是否已结账
    if let Some(closed) = findb::periods::closed_upto(&db)? {
        if v.period <= closed {
            return Err(AppError::bad_request(format!(
                "{} 及以前期间已结账，不能删除该凭证",
                closed.label()
            )));
        }
    }
    vouchers::delete(&db, id)?;
    db.log(user.username(), "凭证", "删除", &v.voucher_no())?;
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// 发票管理
// ---------------------------------------------------------------------------

/// 发票 → 响应 JSON（金额已格式化）
fn invoice_json(inv: &findb::invoices::Invoice) -> serde_json::Value {
    json!({
        "id": inv.id,
        "kind": inv.kind,
        "code": inv.code,
        "number": inv.number,
        "date": inv.date,
        "buyer": inv.buyer,
        "seller": inv.seller,
        "amount_tax": inv.amount_tax.fmt_money(),
        "amount": inv.amount.fmt_money(),
        "tax": inv.tax.fmt_money(),
        "tax_rate": inv.tax_rate,
        "status": inv.status,
        "status_label": inv.status_label(),
        "memo": inv.memo,
        "attach_id": inv.attach_id,
        "created_by": inv.created_by,
    })
}

fn invoice_from_req(r: &InvoiceReq) -> findb::invoices::Invoice {
    findb::invoices::Invoice {
        id: r.id,
        kind: if r.kind.is_empty() { "in".to_string() } else { r.kind.clone() },
        code: r.code.clone(),
        number: r.number.clone(),
        date: r.date.clone(),
        buyer: r.buyer.clone(),
        seller: r.seller.clone(),
        amount_tax: parse_money(&r.amount_tax),
        amount: parse_money(&r.amount),
        tax: parse_money(&r.tax),
        tax_rate: r.tax_rate.clone(),
        status: if r.status.is_empty() { "pending".to_string() } else { r.status.clone() },
        memo: r.memo.clone(),
        attach_id: 0,
        created_by: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

async fn list_invoices(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<InvoiceListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::invoices::list(
        &db,
        &findb::invoices::InvoiceQuery {
            kind: if q.kind.is_empty() { None } else { Some(q.kind) },
            status: if q.status.is_empty() { None } else { Some(q.status) },
            keyword: if q.keyword.is_empty() { None } else { Some(q.keyword) },
            limit: None,
        },
    )?;
    let items: Vec<serde_json::Value> = rows.iter().map(invoice_json).collect();
    Ok(Json(json!({ "rows": items, "total": items.len() })))
}

async fn invoice_summary(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let sum = findb::invoices::summary(&db)?;
    let by_kind: serde_json::Map<String, serde_json::Value> = sum
        .into_iter()
        .map(|(k, tax_total, tax_amt, n)| {
            (
                k.clone(),
                json!({ "amount_tax": tax_total.fmt_money(), "tax": tax_amt.fmt_money(), "count": n }),
            )
        })
        .collect();
    Ok(Json(json!({ "by_kind": by_kind })))
}

async fn create_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<InvoiceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let inv = invoice_from_req(&req);
    let id = findb::invoices::insert(&db, &inv, user.username())?;
    Ok(Json(json!({ "id": id })))
}

async fn update_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<InvoiceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = state.db_for(&user.book_key)?;
    let mut inv = invoice_from_req(&req);
    inv.id = id;
    findb::invoices::update(&db, &inv)?;
    db.log(user.username(), "发票", "更新", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = state.db_for(&user.book_key)?;
    findb::invoices::delete(&db, id)?;
    db.log(user.username(), "发票", "删除", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn invoice_set_status(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<InvoiceStatusReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = state.db_for(&user.book_key)?;
    let inv = findb::invoices::set_status(&db, id, &req.status, user.username())?;
    Ok(Json(invoice_json(&inv)))
}

/// 发票列表查询参数
#[derive(Deserialize, Default)]
pub struct InvoiceListQuery {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub keyword: String,
}

// ---------------------------------------------------------------------------
// 数据导入（其他软件 / CSV / Excel）
// ---------------------------------------------------------------------------

/// 解析 base64（纯标准 base64 字母表，无依赖实现）
fn b64_decode(s: &str) -> Result<Vec<u8>, AppError> {
    let s = s.trim();
    // 兼容 data URL 前缀（data:application/...;base64,xxx）
    let s = s.split(',').last().unwrap_or(s);
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &b in s.as_bytes() {
        let v = match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a' + 26) as u32,
            b'0'..=b'9' => (b - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' | b' ' => continue,
            _ => return Err(AppError::bad_request("文件不是合法的 base64 编码")),
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

/// 预检：返回文件中引用但账套不存在的科目（供用户选择映射或忽略）
async fn import_analyze(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ImportAnalyzeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let has_file = req.file.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    if !has_file && req.text.trim().is_empty() {
        return Err(AppError::bad_request("请选择 Excel 文件或粘贴 CSV 内容"));
    }
    let file_bytes: Option<Vec<u8>> = if has_file {
        Some(b64_decode(req.file.as_deref().unwrap_or(""))?)
    } else {
        None
    };
    let key = user.book_key.clone();
    let state2 = state.clone();
    // 预检也要解析整份 Excel：同样放阻塞池
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, AppError> {
        let db = state2.db_for(&key)?;
        let tmpl = findb::imports::ImportTemplate::parse(&req.template);
        let is_begin = req.kind != "voucher";
        let text = if let Some(bytes) = &file_bytes {
            let rows = findb::imports::read_xlsx_bytes(bytes)?;
            findb::imports::xlsx_to_csv_text(&rows)
        } else {
            req.text.clone()
        };
        let missing = findb::imports::analyze_missing(&db, &text, tmpl, is_begin)?;
        let items: Vec<serde_json::Value> = missing
            .iter()
            .map(|m| json!({ "code": m.code, "count": m.count }))
            .collect();
        Ok(json!({ "missing": items }))
    })
    .await
    .map_err(|e| AppError::Internal(format!("导入预检失败：{e}")))??;
    Ok(Json(out))
}

/// 执行导入（期初余额表 / 凭证），带科目映射；支持 CSV 文本或 Excel 文件
async fn import_run(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ImportRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let has_file = req.file.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    if !has_file && req.text.trim().is_empty() {
        return Err(AppError::bad_request("请粘贴 CSV 内容或选择 Excel 文件"));
    }
    // 文件解码放请求线程，重活（解析 Excel + 批量写库）整体进阻塞池
    let file_bytes: Option<Vec<u8>> = if has_file {
        Some(b64_decode(req.file.as_deref().unwrap_or(""))?)
    } else {
        None
    };
    let key = user.book_key.clone();
    let who = user.username().to_string();
    let fallback_ymm = current_period(&state, &user).ymm();
    let explicit_ymm = if req.period > 0 { Some(period_checked(req.period)?.ymm()) } else { None };
    let state2 = state.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, AppError> {
        let db = state2.db_for(&key)?;
        let tmpl = findb::imports::ImportTemplate::parse(&req.template);
        let res = if req.kind == "voucher" {
            let period = fincore::Period::from_ymm(explicit_ymm.unwrap_or(fallback_ymm));
            if let Some(bytes) = &file_bytes {
                findb::imports::import_vouchers_bytes(&db, period, bytes, &who, &req.mapping, tmpl)?
            } else {
                findb::imports::import_vouchers(&db, period, &req.text, &who, &req.mapping, tmpl)?
            }
        } else if let Some(bytes) = &file_bytes {
            findb::imports::import_begin_bytes(&db, bytes, &who, &req.mapping, tmpl)?
        } else {
            findb::imports::import_begin(&db, &req.text, &who, &req.mapping, tmpl)?
        };
        Ok(json!({
            "ok": res.ok,
            "skipped": res.skipped,
            "warnings": res.warnings,
        }))
    })
    .await
    .map_err(|e| AppError::Internal(format!("导入任务失败：{e}")))??;
    Ok(Json(out))
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
    let db = state.db_for(&user.book_key)?;
    let chart = accounts::chart(&db)?;
    // 数据范围：非全量权限用户只能查看自己范围内的科目
    if !user.user.can_see_account(&code) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
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

async fn print_voucher_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = state.db_for(&user.book_key)?;
    let company = user.company.clone();
    let chart = accounts::chart(&db)?;
    let aux_names = findb::auxs::full_name_map(&db)?;

    let mut query = VoucherQuery::default().with_data_scope(&user.user);
    query.asc = true;
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let to = q
        .get("to")
        .and_then(|s| parse_period(s))
        .unwrap_or(from);
    query.from = Some(from);
    query.to = Some(to);
    query.limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .or(Some(500));
    let mut list = vouchers::list(&db, &query)?;
    vouchers::fill_entries(&db, &mut list)?;
    list.retain(|v| user.user.can_see_voucher(v));
    if list.is_empty() {
        return Err(AppError::bad_request("没有可打印的凭证"));
    }

    let aux_label = |aux: &fincore::AuxRef| {
        let mut parts = Vec::new();
        for k in fincore::account::AuxKind::ALL {
            if *k == fincore::account::AuxKind::CashFlow {
                continue;
            }
            if let Some(v) = aux.get(*k) {
                let name = aux_names
                    .get(&format!("{}:{}", k.code(), v))
                    .cloned()
                    .unwrap_or_default();
                parts.push(if name.is_empty() {
                    v.to_string()
                } else {
                    name
                });
            }
        }
        parts.join("/")
    };
    let prints = findb::printform::vouchers_to_print(&list, &chart, &aux_label);
    let html = findb::printform::voucher_form_html(
        &company,
        &format!("{}~{}", period_to_str(from), period_to_str(to)),
        &prints,
        true,
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 账簿套打（明细账 / 总账 / 日记账）HTML
async fn print_ledger_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码参数 code"));
    }
    if !user.user.can_see_account(&code) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let db = state.db_for(&user.book_key)?;
    let company = user.company.clone();
    let chart = accounts::chart(&db)?;
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let to = q
        .get("to")
        .and_then(|s| parse_period(s))
        .unwrap_or(from);
    let ktype = q.get("type").map(String::as_str).unwrap_or("detail");
    let include_children = q
        .get("include_children")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let lq = LedgerQuery {
        code: code.clone(),
        include_children,
        aux: None,
        from,
        to,
        posted_only,
    };
    let acct = chart
        .get(&code)
        .map(|a| format!("{} {}", code, a.name))
        .unwrap_or(code.clone());

    // 期初余额方向
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::range(from, from))?;
    let (bd, bamt) = fincore::signed_to_dir_amount(snap.for_account(&code, None).begin);
    let begin_dir = if bamt.is_zero() {
        "平".to_string()
    } else {
        bd.label().to_string()
    };

    let (title, rows) = match ktype {
        "general" => {
            let list = balances::general_ledger(&db, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.period.code(),
                    voucher_no: String::new(),
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("总账".to_string(), rows)
        }
        "journal" => {
            let list = balances::journal(&db, &chart, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.date.format("%Y-%m-%d").to_string(),
                    voucher_no: r.voucher_no,
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("日记账".to_string(), rows)
        }
        _ => {
            let list = balances::ledger(&db, &chart, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.date.format("%Y-%m-%d").to_string(),
                    voucher_no: r.voucher_no,
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("明细账".to_string(), rows)
        }
    };

    let ledger = findb::printform::LedgerPrint {
        title,
        account_name: acct,
        period_label: format!("{}~{}", period_to_str(from), period_to_str(to)),
        begin_dir,
        begin_balance: bamt,
        rows,
        page_from_1: true,
    };
    let html = findb::printform::ledger_form_html(&company, &ledger);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 计算科目余额表（供 JSON / 打印 / 导出复用）
fn trial_balance_data(
    state: &WebState,
    user: &CurrentUser,
    q: &HashMap<String, String>,
) -> Result<(Vec<fincore::balance::BalanceRow>, fincore::balance::TrialBalance), AppError> {
    let db = state.db_for(&user.book_key)?;
    let start = db.options().start_period;
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or(start);
    let to = q
        .get("to")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(state, user));
    let bq = BalanceQuery::range(from, to).with_data_scope(&user.user.data_scope);
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
    let html = trial_balance_html(&user.company, &q, &rows, &totals);
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

/// 科目余额表导出 PDF（仅管理员与财务主管）
async fn export_trial_balance_pdf(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let from = q.get("from").cloned().unwrap_or_default();
    let to = q.get("to").cloned().unwrap_or_default();
    let company = user.company.clone();
    let bytes = crate::pdf::trial_balance_pdf(&company, &from, &to, &rows, &totals)
        .map_err(AppError::bad_request)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"trial_balance.pdf\"",
            ),
        ],
        bytes,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// 报表渲染
// ---------------------------------------------------------------------------

fn trial_balance_html(
    company: &str,
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
        html_escape(company),
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
            csv_escape(&r.account_code),
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

// ---------------------------------------------------------------------------
// 高级功能：多栏账 / 摘要汇总表 / 财务指标 / 工艺路线 / 报工 / MRP / 预算版本 /
// 审批流 / 报表附注 / 电子档案
// ---------------------------------------------------------------------------

async fn get_multi_column(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let main = q.get("main").cloned().unwrap_or_default();
    if main.is_empty() {
        return Err(AppError::bad_request("缺少主科目 main"));
    }
    let cols: Vec<String> = q
        .get("cols")
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    if cols.is_empty() {
        return Err(AppError::bad_request("缺少栏目科目 cols（逗号分隔）"));
    }
    let from = q.get("from").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let to = q.get("to").and_then(|s| parse_period(s)).unwrap_or(from);
    let db = state.db_for(&user.book_key)?;
    // 数据范围：主科目与各栏目科目均须在可见范围内
    if !user.user.can_see_account(&main) || cols.iter().any(|c| !user.user.can_see_account(c)) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let rows = advanced::multi_column_table(&db, &main, &cols, from, to)?;
    Ok(Json(serde_json::json!({ "main": main, "cols": cols, "rows": rows })))
}

async fn get_summary_table(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let from = q.get("from").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let to = q.get("to").and_then(|s| parse_period(s)).unwrap_or(from);
    let db = state.db_for(&user.book_key)?;
    let rows = advanced::summary_table(&db, from, to, Some(&user.user))?;
    Ok(Json(serde_json::json!({ "from": period_to_str(from), "to": period_to_str(to), "rows": rows })))
}

async fn get_fin_ratios(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let from = q.get("from").and_then(|s| parse_period(s)).unwrap_or_else(|| {
        // 默认年初（同一会计年度 1 月）
        fincore::Period::new(period.year(), 1).unwrap_or(period)
    });
    let db = state.db_for(&user.book_key)?;
    let rows = advanced::fin_ratios(&db, period, from, Some(&user.user))?;
    Ok(Json(serde_json::json!({ "period": period_to_str(period), "ratios": rows })))
}

// ---- 所有者权益变动表 / 报表对比 / 科目日报表 / 期末对账 ----

async fn get_equity_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let from = q.get("from").and_then(|s| parse_period(s)).unwrap_or_else(|| {
        fincore::Period::new(period.year(), 1).unwrap_or(period)
    });
    let db = state.db_for(&user.book_key)?;
    let stmt = findb::reports::equity_statement(&db, from, period, Some(&user.user))?;
    Ok(Json(serde_json::json!({
        "from": period_to_str(from),
        "to": period_to_str(period),
        "statement": stmt,
    })))
}

async fn get_report_compare(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let key = q.get("key").cloned().unwrap_or_else(|| "balance_sheet".to_string());
    let cur = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let prev = q.get("prev").and_then(|s| parse_period(s)).unwrap_or(cur.prev());
    let yearly = q.get("yearly").map(|s| s == "1" || s == "true").unwrap_or(true);
    // 默认按年累计：当前期 1 月→当前期；上期 1 月→上期
    let (cur_from, prev_from) = if yearly {
        (fincore::Period::new(cur.year(), 1).unwrap_or(cur), fincore::Period::new(prev.year(), 1).unwrap_or(prev))
    } else {
        (cur, prev)
    };
    let db = state.db_for(&user.book_key)?;
    let rows = findb::reports::report_compare(&db, &key, cur_from, cur, prev_from, prev, Some(&user.user))?;
    Ok(Json(serde_json::json!({
        "key": key,
        "current": period_to_str(cur),
        "previous": period_to_str(prev),
        "rows": rows,
    })))
}

async fn get_account_daily(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码 code"));
    }
    let from = q.get("from").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let to = q.get("to").and_then(|s| parse_period(s)).unwrap_or(from);
    let db = state.db_for(&user.book_key)?;
    // 数据范围：仅可见科目
    if !user.user.can_see_account(&code) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let rows = findb::reports::account_daily_report(&db, &code, from, to, Some(&user.user))?;
    Ok(Json(serde_json::json!({ "code": code, "rows": rows })))
}

async fn get_period_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let db = state.db_for(&user.book_key)?;
    let items = findb::reports::period_reconcile(&db, period)?;
    Ok(Json(serde_json::json!({ "period": period_to_str(period), "items": items })))
}

// ---- 存货核算：成本调整 ----

#[derive(Deserialize)]
struct StockAdjustReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub item: String,
    #[serde(default)]
    pub warehouse: String,
    /// 调整金额（正=调增，负=调减），十进制字符串
    pub delta: String,
    #[serde(default)]
    pub memo: String,
}

async fn stock_adjust_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<StockAdjustReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    if req.item.trim().is_empty() {
        return Err(AppError::bad_request("缺少存货 item"));
    }
    let delta = parse_money(&req.delta);
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::business::stock_adjust(&db, period, date, &req.item, &req.warehouse, delta, &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

// ---- 库存深度：序列号 / 多单位 / 账龄 / ABC / 组装拆卸 / 分仓库 ----

#[derive(Deserialize)]
struct SerialInReq {
    pub item: String,
    pub serials: Vec<String>,
    #[serde(default)]
    pub batch_no: String,
    #[serde(default)]
    pub date: String,
}

async fn serial_in_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SerialInReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let n = findb::inventory2::serial_in(&db, &req.item, &req.serials, &req.batch_no, date)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": n })))
}

#[derive(Deserialize)]
struct SerialOutReq {
    pub serials: Vec<String>,
    #[serde(default)]
    pub date: String,
}

async fn serial_out_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SerialOutReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let n = findb::inventory2::serial_out(&db, &req.serials, date)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": n })))
}

async fn list_serial(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let item = q.get("item").cloned().unwrap_or_default();
    if item.is_empty() {
        return Err(AppError::bad_request("缺少 item"));
    }
    let db = state.db_for(&user.book_key)?;
    let rows = findb::inventory2::serial_list(&db, &item)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_unit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let item = q.get("item").cloned().unwrap_or_default();
    let db = state.db_for(&user.book_key)?;
    let u = findb::inventory2::unit_get(&db, &item)?;
    Ok(Json(serde_json::json!({ "unit": u })))
}

#[derive(Deserialize)]
struct UnitReq {
    pub item: String,
    #[serde(default)]
    pub base_unit: String,
    #[serde(default)]
    pub alt_unit: String,
    #[serde(default)]
    pub factor: String,
}

async fn set_unit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<UnitReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::inventory2::unit_set(&db, &findb::inventory2::ItemUnit {
        item: req.item,
        base_unit: req.base_unit,
        alt_unit: req.alt_unit,
        factor: parse_money(&req.factor),
    })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn get_inv_aging(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::inventory2::inv_aging(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_abc(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::inventory2::abc_analysis(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

#[derive(Deserialize)]
struct AssembleReq {
    pub parent: String,
    pub children: Vec<(String, String)>,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub date: String,
}

async fn assemble_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssembleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let children: Vec<(String, Money)> = req.children.into_iter().map(|(i, q)| (i, parse_money(&q))).collect();
    findb::inventory2::assemble(&db, period, date, &req.parent, &children, &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn disassemble_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssembleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let children: Vec<(String, Money)> = req.children.into_iter().map(|(i, q)| (i, parse_money(&q))).collect();
    findb::inventory2::disassemble(&db, period, date, &req.parent, &children, &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn get_warehouse_stock(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let item = q.get("item").cloned().unwrap_or_default();
    if item.is_empty() {
        return Err(AppError::bad_request("缺少 item"));
    }
    let db = state.db_for(&user.book_key)?;
    let rows = findb::inventory2::warehouse_stock(&db, &item)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_transfer_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::inventory2::transfer_report(&db, current_period(&state, &user))?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|m| {
            serde_json::json!({
                "date": m.biz_date.format("%Y-%m-%d").to_string(),
                "item": m.item,
                "warehouse": m.warehouse,
                "qty": m.qty.fmt_qty(),
                "memo": m.memo,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "rows": items })))
}

// ---- 采购/销售深度：暂估 / 对账 / 配额 / 订单变更 ----

#[derive(Deserialize)]
struct EstimateReq {
    pub po_id: i64,
    #[serde(default)]
    pub period: i32,
    pub item: String,
    #[serde(default)]
    pub est_amount: String,
}

async fn add_estimate(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<EstimateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let id = findb::scm2::po_estimate_add(&db, req.po_id, period, &req.item, parse_money(&req.est_amount))?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn list_estimates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let po_id = q
        .get("po_id")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if po_id == 0 {
        return Err(AppError::bad_request("缺少 po_id"));
    }
    let db = state.db_for(&user.book_key)?;
    let rows = findb::scm2::po_estimate_list(&db, po_id)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn settle_estimate(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::scm2::po_estimate_settle(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn get_po_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::scm2::po_reconcile(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_so_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::scm2::so_reconcile(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_quota(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let supplier = q.get("supplier").cloned().unwrap_or_default();
    let item = q.get("item").cloned().unwrap_or_default();
    let remaining = findb::scm2::quota_remaining(&db, period, &supplier, &item)?;
    Ok(Json(serde_json::json!({ "remaining": remaining })))
}

#[derive(Deserialize)]
struct QuotaReq {
    #[serde(default)]
    pub period: i32,
    pub supplier: String,
    pub item: String,
    #[serde(default)]
    pub quota_qty: String,
}

async fn set_quota(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<QuotaReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    findb::scm2::quota_set(&db, period, &req.supplier, &req.item, parse_money(&req.quota_qty))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn get_change_log(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let order_type = q.get("type").cloned().unwrap_or_else(|| "po".to_string());
    let order_id = q.get("id").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let rows = findb::scm2::change_log_list(&db, &order_type, order_id)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

// ---- 采购/销售全生命周期：请购 / 报价 / 到货 / 发货 / 付款 / 收款 / 退货 / 信用 ----

async fn list_purchase_req(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::procurement::pr_list(&db, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_purchase_req(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(mut req): Json<findb::procurement::PurchaseReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    if req.no.is_empty() {
        req.no = findb::procurement::pr_next_no(&db, req.period)?;
    }
    if req.requester.is_empty() {
        req.requester = user.user.display_name.clone();
    }
    let id = findb::procurement::pr_save(&db, &mut req)?;
    Ok(Json(serde_json::json!({ "id": id, "no": req.no })))
}

async fn approve_purchase_req(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::procurement::pr_approve(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ReceiptReq {
    pub po_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub qty: String,
    #[serde(default)]
    pub memo: String,
}

async fn add_po_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::procurement::po_receipt_add(&db, &findb::procurement::PoReceipt {
        id: 0, po_id: req.po_id, period, date, qty: parse_money(&req.qty), memo: req.memo,
    })?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn add_po_return(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::procurement::po_return_add(&db, req.po_id, period, date, parse_money(&req.qty), &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct PaymentReq {
    pub po_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}

async fn add_po_payment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PaymentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::procurement::po_payment_add(&db, &findb::procurement::PoPayment {
        id: 0, po_id: req.po_id, period, date, amount: parse_money(&req.amount), memo: req.memo,
    })?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn get_price_history(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let item = q.get("item").cloned().unwrap_or_default();
    let rows = findb::procurement::price_history(&db, &item)?;
    let rows: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(s, p, d)| serde_json::json!({ "supplier": s, "unit_price": p.fmt_money(), "date": d }))
        .collect();
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_po_track(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::procurement::po_execution_track(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_purchase_stats(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::procurement::purchase_stats(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn list_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::sales::quo_list(&db, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(mut req): Json<findb::sales::Quotation>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    if req.no.is_empty() {
        req.no = findb::sales::quo_next_no(&db, req.period)?;
    }
    if req.prepared_by.is_empty() {
        req.prepared_by = user.user.display_name.clone();
    }
    let id = findb::sales::quo_save(&db, &mut req)?;
    Ok(Json(serde_json::json!({ "id": id, "no": req.no })))
}

async fn approve_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::sales::quo_approve(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ShipmentReq {
    pub so_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub qty: String,
    #[serde(default)]
    pub memo: String,
}

async fn add_so_shipment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ShipmentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::sales::so_shipment_add(&db, req.so_id, period, date, parse_money(&req.qty), &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn add_so_return(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ShipmentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::sales::so_return_add(&db, req.so_id, period, date, parse_money(&req.qty), &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct SoPaymentReq {
    pub so_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}

async fn add_so_payment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SoPaymentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::sales::so_payment_add(&db, req.so_id, period, date, parse_money(&req.amount), &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn get_credit_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let customer = q.get("customer").cloned().unwrap_or_default();
    let (receivable, limit, over) = findb::sales::credit_check(&db, &customer, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({
        "receivable": receivable.fmt_money(),
        "limit": limit.fmt_money(),
        "over": over,
    })))
}

async fn get_so_track(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::sales::so_execution_track(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_sales_stats(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::sales::sales_stats(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

// ---- 预算预警 ----

async fn get_budget_alerts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let from = q.get("from").and_then(|s| parse_period(s)).unwrap_or_else(|| fincore::Period::new(period.year(), 1).unwrap_or(period));
    let threshold = q.get("threshold").and_then(|s| s.parse::<i64>().ok()).unwrap_or(90);
    let rows = findb::mgmt::budget_alerts(&db, period, from, threshold)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

/// 坏账准备计提：按应收账龄生成「借 资产减值损失 / 贷 坏账准备」凭证
async fn bad_debt_provision_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let date = chrono::Local::now().date_naive();
    match findb::settle::bad_debt_provision_voucher(&db, period, date, user.username())? {
        Some(id) => {
            db.log(user.username(), "往来", "计提坏账准备", &format!("凭证 #{id}"))?;
            Ok(Json(json!({"ok": true, "voucher_id": id})))
        }
        None => Ok(Json(json!({"ok": true, "voucher_id": 0, "message": "无可计提的坏账准备"}))),
    }
}

// ---- 工艺路线 / 报工 ----

#[derive(Deserialize)]
struct RoutingOpDto {
    #[serde(default)]
    pub seq: i32,
    pub op_code: String,
    pub op_name: String,
    #[serde(default)]
    pub work_center: String,
    #[serde(default)]
    pub std_hours: String,
    #[serde(default)]
    pub rate: String,
}

async fn get_routing(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let ops = advanced::routing_list(&db, &item)?;
    Ok(Json(serde_json::json!({ "item_code": item, "ops": ops })))
}

async fn post_routing(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
    Json(req): Json<Vec<RoutingOpDto>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let ops: Vec<advanced::RoutingOp> = req
        .into_iter()
        .map(|d| advanced::RoutingOp {
            id: 0,
            item_code: item.clone(),
            version: String::new(),
            seq: d.seq,
            op_code: d.op_code,
            op_name: d.op_name,
            work_center: d.work_center,
            std_hours: parse_money(&d.std_hours),
            rate: parse_money(&d.rate),
        })
        .collect();
    advanced::routing_save(&db, &item, &ops)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": ops.len() })))
}

async fn delete_routing(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    advanced::routing_delete(&db, &item)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_prod_orders(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let orders = findb::scm::prod_list(&db, period, None)?;
    let items: Vec<serde_json::Value> = orders
        .iter()
        .map(|o| {
            serde_json::json!({
                "id": o.id,
                "no": o.no,
                "item_code": o.item_code,
                "item_name": o.item_name,
                "planned_qty": o.planned_qty.fmt_qty(),
                "completed_qty": o.completed_qty.fmt_qty(),
                "status": format!("{:?}", o.status),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "period": period_to_str(period), "orders": items })))
}

async fn get_prod_ops(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let ops = advanced::prod_op_list(&db, id)?;
    Ok(Json(serde_json::json!({ "po_id": id, "ops": ops })))
}

#[derive(Deserialize)]
struct OpReportReq {
    pub op_id: i64,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub hours: String,
}

async fn report_prod_op(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OpReportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    advanced::prod_op_report(&db, req.op_id, parse_money(&req.qty), parse_money(&req.hours))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct OpFinishReq {
    pub op_id: i64,
}

async fn finish_prod_op(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OpFinishReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    advanced::prod_op_finish(&db, req.op_id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- MRP ----

async fn get_mrp_latest(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let rows = advanced::mrp_latest(&db)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

#[derive(Deserialize)]
struct MrpDemandDto {
    pub item_code: String,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub source: String,
}

#[derive(Deserialize)]
struct MrpRunReq {
    #[serde(default)]
    pub demands: Vec<MrpDemandDto>,
    /// 若为 true 且 demands 为空，则从已确认销售订单收集需求
    #[serde(default)]
    pub from_sales: bool,
}

async fn run_mrp(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<MrpRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let mut demands: Vec<(String, Money, String)> = req
        .demands
        .into_iter()
        .map(|d| (d.item_code, parse_money(&d.qty), d.source))
        .collect();
    if demands.is_empty() && req.from_sales {
        demands = advanced::mrp_demands_from_sales(&db, current_period(&state, &user))?;
    }
    if demands.is_empty() {
        return Err(AppError::bad_request("请提供需求清单，或勾选「从销售订单收集」"));
    }
    let run_at = advanced::mrp_run(&db, &demands)?;
    let rows = advanced::mrp_by_run(&db, &run_at)?;
    Ok(Json(serde_json::json!({ "run_at": run_at, "rows": rows })))
}

// ---- 预算版本 ----

async fn list_budget_versions(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let versions = advanced::bversion_list(&db)?;
    let current = advanced::bversion_current(&db)?;
    Ok(Json(serde_json::json!({ "versions": versions, "current": current })))
}

#[derive(Deserialize)]
struct BVersionReq {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub is_current: bool,
    #[serde(default)]
    pub memo: String,
}

// 预算版本 / 审批流 / 报表附注 / 会计档案的写操作统一要求 Perm::AccountEdit，
// 与其余业务写路由保持一致。不要用 Perm::Report 把关：Report 是只读权限，
// 且每个角色（含只读 Viewer）都自带它，等于对只读账号开放了写入。
async fn save_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BVersionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    advanced::bversion_save(
        &db,
        &advanced::BudgetVersion {
            key: req.key,
            name: req.name,
            is_current: req.is_current,
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            memo: req.memo,
        },
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    advanced::bversion_delete(&db, &key)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn activate_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let mut v = advanced::bversion_list(&db)?
        .into_iter()
        .find(|v| v.key == key)
        .ok_or_else(|| AppError::NotFound(format!("预算版本 {key} 不存在")))?;
    v.is_current = true;
    advanced::bversion_save(&db, &v)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct BCopyReq {
    pub from: String,
    pub to: String,
}

async fn copy_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BCopyReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let n = advanced::bversion_copy(&db, &req.from, &req.to)?;
    Ok(Json(serde_json::json!({ "ok": true, "copied": n })))
}

// ---- 审批流 ----

async fn list_approvals(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = advanced::approval_list(&db, 100)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn list_approval_todo(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = advanced::approval_todo(&db, user.username())?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let ap = advanced::approval_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("审批流不存在".to_string()))?;
    Ok(Json(serde_json::json!({ "approval": ap })))
}

#[derive(Deserialize)]
struct ApprovalStartReq {
    pub biz_kind: String,
    pub biz_id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub approvers: Vec<String>,
}

async fn start_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ApprovalStartReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let id = advanced::approval_start(
        &db,
        &req.biz_kind,
        req.biz_id,
        &req.title,
        user.username(),
        &req.approvers,
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct ApprovalActReq {
    pub approve: bool,
    #[serde(default)]
    pub comment: String,
}

async fn act_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ApprovalActReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let ap = advanced::approval_act(&db, id, user.username(), req.approve, &req.comment)?;
    Ok(Json(serde_json::json!({ "approval": ap })))
}

async fn cancel_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    advanced::approval_cancel(&db, id, user.username())?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- 报表附注 ----

async fn list_notes(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let report_key = q.get("report_key").cloned().unwrap_or_else(|| "balance_sheet".to_string());
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let db = state.db_for(&user.book_key)?;
    let rows = advanced::note_list(&db, &report_key, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

#[derive(Deserialize)]
struct NoteReq {
    #[serde(default)]
    pub id: i64,
    pub report_key: String,
    pub period: i32,
    #[serde(default)]
    pub seq: i32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: String,
}

async fn save_note(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<NoteReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let mut n = advanced::ReportNote {
        id: req.id,
        report_key: req.report_key,
        period: period_checked(req.period)?,
        seq: req.seq,
        title: req.title,
        content: req.content,
        updated_by: user.username().to_string(),
        updated_at: String::new(),
    };
    let id = advanced::note_save(&db, &mut n)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn delete_note(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    advanced::note_delete(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- 会计电子档案 ----

async fn list_archives(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let kind = q.get("kind").map(|s| s.as_str());
    let db = state.db_for(&user.book_key)?;
    let rows = advanced::archive_list(&db, period, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

#[derive(Deserialize)]
struct ArchiveReq {
    pub period: i32,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub file_no: String,
    pub payload: String,
}

async fn create_archive(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ArchiveReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = period_checked(req.period)?;
    let file_no = if req.file_no.trim().is_empty() {
        advanced::archive_next_no(&db, period, &req.kind)?
    } else {
        req.file_no
    };
    let id = advanced::archive_create(&db, period, &req.kind, &req.title, &file_no, &req.payload, user.username())?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id, "file_no": file_no })))
}

async fn get_archive(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let a = advanced::archive_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("档案不存在".to_string()))?;
    Ok(Json(serde_json::json!({ "archive": a })))
}

async fn verify_archive(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let a = advanced::archive_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("档案不存在".to_string()))?;
    let ok = advanced::archive_verify(&a);
    Ok(Json(serde_json::json!({ "ok": ok, "content_hash": a.content_hash })))
}

// ---------------------------------------------------------------------------
// 资金：票据 / 融资 / 资金日报 / 资金预测
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BillReq {
    #[serde(default)]
    pub id: i64,
    pub kind: String,
    pub no: String,
    pub period: i32,
    pub issue_date: String,
    pub due_date: String,
    #[serde(default)]
    pub counterpart: String,
    #[serde(default)]
    pub bank: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
struct BillTransitionReq {
    pub status: String,
    pub date: String,
}

async fn list_bills(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let kind = q.get("kind").map(|s| s.as_str());
    let rows = findb::funds::bill_list(&db, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_bill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BillReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let parse_date = |s: &str| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
    };
    let mut b = findb::funds::Bill {
        id: req.id,
        kind: req.kind,
        no: req.no,
        period: period_checked(req.period)?,
        issue_date: parse_date(&req.issue_date)?,
        due_date: parse_date(&req.due_date)?,
        counterpart: req.counterpart,
        bank: req.bank,
        amount: parse_money(&req.amount),
        status: req.status,
        handled_date: None,
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::bill_save(&db, &mut b)?;
    db.log(user.username(), "资金", "保存票据", &format!("#{id} {}", b.no))?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn bill_transition(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<BillTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let date = chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    let to = findb::funds::BillStatus::parse(&req.status);
    findb::funds::bill_transition(&db, id, to, date)?;
    db.log(user.username(), "资金", "票据流转", &format!("#{id} → {}", to.label()))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_bill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::funds::bill_delete(&db, id)?;
    db.log(user.username(), "资金", "删除票据", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LoanReq {
    #[serde(default)]
    pub id: i64,
    pub kind: String,
    pub no: String,
    #[serde(default)]
    pub bank: String,
    #[serde(default)]
    pub principal: String,
    #[serde(default)]
    pub rate_pct: String,
    pub start_date: String,
    pub end_date: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub memo: String,
}

async fn list_loans(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let kind = q.get("kind").map(|s| s.as_str());
    let rows = findb::funds::loan_list(&db, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_loan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<LoanReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let parse_date = |s: &str| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
    };
    let mut l = findb::funds::Loan {
        id: req.id,
        kind: req.kind,
        no: req.no,
        bank: req.bank,
        principal: parse_money(&req.principal),
        rate_pct: parse_money(&req.rate_pct),
        start_date: parse_date(&req.start_date)?,
        end_date: parse_date(&req.end_date)?,
        status: req.status,
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::loan_save(&db, &mut l)?;
    db.log(user.username(), "资金", "保存融资", &format!("#{id} {}", l.no))?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn loan_settle(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::funds::loan_settle(&db, id)?;
    db.log(user.username(), "资金", "结清融资", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_loan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::funds::loan_delete(&db, id)?;
    Ok(Json(json!({ "ok": true })))
}

async fn get_funds_daily(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let rows = findb::funds::funds_daily(&db, period)?;
    Ok(Json(serde_json::json!({ "period": period.label(), "rows": rows })))
}

async fn get_funds_forecast(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let fc = findb::funds::funds_forecast(&db, period)?;
    Ok(Json(serde_json::json!({ "period": period.label(), "forecast": fc })))
}

// ---------------------------------------------------------------------------
// 预算分析
// ---------------------------------------------------------------------------

async fn get_budget_analysis(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let year = q.get("year").and_then(|s| s.parse::<i32>().ok()).unwrap_or_else(|| current_period(&state, &user).year());
    let version = q.get("version").cloned().unwrap_or_default();
    let upto = q.get("upto").and_then(|s| parse_period(s));
    let rows = findb::mgmt::budget_analysis(&db, year, &version, upto)?;
    let summary = findb::mgmt::budget_analysis_summary(&db, year, &version)?;
    Ok(Json(serde_json::json!({ "year": year, "rows": rows, "summary": summary })))
}

// ---------------------------------------------------------------------------
// 成本：计价配置 + 期末结价
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CostMethodReq {
    pub item: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub standard_cost: String,
}

async fn list_cost_configs(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows = findb::business::cost_configs(&db)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_cost_method(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CostMethodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let sc = parse_money(&req.standard_cost);
    findb::business::item_cost_method_set(&db, &req.item, Some(&req.method), sc)?;
    db.log(user.username(), "成本", "设置计价方式", &format!("{} → {}", req.item, req.method))?;
    Ok(Json(json!({ "ok": true })))
}

async fn clear_cost_method(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::business::item_cost_method_clear(&db, &item)?;
    Ok(Json(json!({ "ok": true })))
}

async fn run_period_end_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    method: axum::http::Method,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let db = state.db_for(&user.book_key)?;
    let period = q.get("period").and_then(|s| parse_period(s)).unwrap_or_else(|| current_period(&state, &user));
    let apply = q.get("apply").map(|s| s == "1" || s == "true").unwrap_or(false);
    // 落库动作不能藏在 GET 里：顶层导航/预取都可能带 Cookie 触发写操作
    if apply && method != axum::http::Method::POST {
        return Err(AppError::bad_request(
            "期末结价落库请使用 POST /api/cost/period-end?apply=1",
        ));
    }
    let rows = findb::business::period_end_cost(&db, period, apply)?;
    Ok(Json(serde_json::json!({ "period": period.label(), "rows": rows, "apply": apply })))
}

// ---------------------------------------------------------------------------
// 三大报表：资产负债表 / 利润表 / 现金流量表（JSON + 打印预览）
// ---------------------------------------------------------------------------

/// 从 query 解析 起/止期间（默认当前期间）
fn report_range(state: &WebState, user: &CurrentUser, q: &HashMap<String, String>) -> (Period, Period) {
    let cur = current_period(state, user);
    let from = q.get("from").and_then(|s| parse_period(s)).unwrap_or_else(|| {
        fincore::Period::new(cur.year(), 1).unwrap_or(cur)
    });
    let to = q.get("to").and_then(|s| parse_period(s)).unwrap_or(cur);
    (from, to)
}

/// 加载报表定义（优先账套内自定义，否则内置模板）
fn report_def(db: &findb::Db, key: &str, fallback: fincore::report::ReportDef) -> fincore::report::ReportDef {
    findb::reports::get_def(db, key)
        .ok()
        .flatten()
        .unwrap_or(fallback)
}

/// 资产负债表 / 利润表共用渲染：返回 ReportTable
fn statement_table(
    db: &findb::Db,
    user: &CurrentUser,
    key: &str,
    from: Period,
    to: Period,
    kind_maps: Vec<Box<dyn Fn(fincore::report::AmountKind) -> fincore::report::AmountKind>>,
) -> Result<fincore::report::ReportTable, AppError> {
    let def = match key {
        "balance_sheet" => report_def(db, key, fincore::report::balance_sheet::balance_sheet_def()),
        _ => report_def(db, key, fincore::report::income::income_statement_def()),
    };
    let mut bq = BalanceQuery::range(from, to);
    bq = bq.with_data_scope(&user.user.data_scope);
    let snap = BalanceSnapshot::load(db, &bq)?;
    let company = db.options().company;
    let subtitle = format!("{} 至 {}", from.label(), to.label());
    Ok(fincore::report::render(&def, &snap, &company, &subtitle, kind_maps))
}

/// ReportTable → JSON（行/值/样式）
fn table_json(t: &fincore::report::ReportTable) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = t
        .rows
        .iter()
        .map(|r| {
            json!({
                "no": r.no,
                "name": r.name,
                "indent": r.indent,
                "style": format!("{:?}", r.style).to_lowercase(),
                "values": r.values.iter().map(|v| v.fmt_money()).collect::<Vec<_>>(),
                "negative": r.show_negative_red,
            })
        })
        .collect();
    json!({
        "title": t.title,
        "subtitle": t.subtitle,
        "company": t.company,
        "columns": t.columns,
        "rows": rows,
    })
}

async fn get_balance_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let (from, to) = report_range(&state, &user, &q);
    let t = statement_table(
        &db,
        &user,
        "balance_sheet",
        from,
        to,
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    )?;
    Ok(Json(json!({ "from": period_to_str(from), "to": period_to_str(to), "table": table_json(&t) })))
}

async fn print_balance_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let (from, to) = report_range(&state, &user, &q);
    let t = statement_table(
        &db,
        &user,
        "balance_sheet",
        from,
        to,
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    )?;
    let html = crate::report_html::report_table_html(&t, &user.company, "元");
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

fn to_ytd(k: fincore::report::AmountKind) -> fincore::report::AmountKind {
    match k {
        fincore::report::AmountKind::PeriodDebit => fincore::report::AmountKind::YearDebit,
        fincore::report::AmountKind::PeriodCredit => fincore::report::AmountKind::YearCredit,
        other => other,
    }
}

async fn get_income_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let (from, to) = report_range(&state, &user, &q);
    let t = statement_table(
        &db,
        &user,
        "income_statement",
        from,
        to,
        vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
    )?;
    Ok(Json(json!({ "from": period_to_str(from), "to": period_to_str(to), "table": table_json(&t) })))
}

async fn print_income_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let (from, to) = report_range(&state, &user, &q);
    let t = statement_table(
        &db,
        &user,
        "income_statement",
        from,
        to,
        vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
    )?;
    let html = crate::report_html::report_table_html(&t, &user.company, "元");
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

async fn get_cash_flow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let (from, to) = report_range(&state, &user, &q);
    let cf = findb::reports::cash_flow_statement(&db, from, to, Some(&user.user))?;
    let line = |l: &fincore::report::cashflow::CashFlowLine| {
        json!({ "code": l.code, "name": l.name, "inflow": l.inflow.fmt_money(), "outflow": l.outflow.fmt_money(), "net": l.net.fmt_money() })
    };
    Ok(Json(json!({
        "from": period_to_str(from), "to": period_to_str(to),
        "operating": cf.operating.iter().map(line).collect::<Vec<_>>(),
        "operating_net": cf.operating_net.fmt_money(),
        "investing": cf.investing.iter().map(line).collect::<Vec<_>>(),
        "investing_net": cf.investing_net.fmt_money(),
        "financing": cf.financing.iter().map(line).collect::<Vec<_>>(),
        "financing_net": cf.financing_net.fmt_money(),
        "net_increase": cf.net_increase.fmt_money(),
        "begin_cash": cf.begin_cash.fmt_money(),
        "end_cash": cf.end_cash.fmt_money(),
        "unassigned": cf.unassigned.fmt_money(),
        "ties": cf.ties(),
    })))
}

async fn print_cash_flow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let (from, to) = report_range(&state, &user, &q);
    let cf = findb::reports::cash_flow_statement(&db, from, to, Some(&user.user))?;
    let subtitle = format!("{} 至 {}", from.label(), to.label());
    let html = crate::report_html::cash_flow_html(&cf, &user.company, &subtitle);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

async fn print_equity_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let (from, to) = report_range(&state, &user, &q);
    let stmt = findb::reports::equity_statement(&db, from, to, Some(&user.user))?;
    let subtitle = format!("{} 至 {}", from.label(), to.label());
    let html = crate::report_html::equity_html(&stmt, &user.company, &subtitle);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

// ===========================================================================
// 账套内基础资料与系统功能（对齐桌面端 finui 补齐）
// ===========================================================================

// ---------------- 会计科目 ----------------
async fn create_account(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AccountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let mut acc = req.account;
    acc.aux = build_aux_mask(&req.aux_kinds);
    let db = state.db_for(&user.book_key)?;
    if accounts::get(&db, &acc.code)?.is_some() {
        return Err(AppError::bad_request("科目已存在"));
    }
    accounts::insert(&db, &acc)?;
    db.log(user.username(), "基础资料", "新建科目", &format!("{} {}", acc.code, acc.name))?;
    Ok(Json(json!({"ok": true})))
}

async fn update_account(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AccountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let mut acc = req.account;
    acc.aux = build_aux_mask(&req.aux_kinds);
    let db = state.db_for(&user.book_key)?;
    if accounts::get(&db, &acc.code)?.is_none() {
        return Err(AppError::not_found("科目不存在"));
    }
    accounts::update(&db, &acc)?;
    db.log(user.username(), "基础资料", "修改科目", &format!("{} {}", acc.code, acc.name))?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_account(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    if accounts::get(&db, &code)?.is_none() {
        return Err(AppError::not_found("科目不存在"));
    }
    let (vouchers, entries) = accounts::usage(&db, &code)?;
    if vouchers > 0 || entries > 0 {
        return Err(AppError::bad_request("科目已被凭证使用，无法删除"));
    }
    accounts::delete(&db, &code)?;
    db.log(user.username(), "基础资料", "删除科目", &code)?;
    Ok(Json(json!({"ok": true})))
}

// ---------------- 期初建账 ----------------
#[derive(Deserialize)]
struct BeginRowInput {
    account_code: String,
    #[serde(default)]
    aux: AuxRef,
    dir: Direction,
    yb: String,
    #[serde(default)]
    ad: String,
    #[serde(default)]
    ac: String,
    #[serde(default)]
    qty: Option<String>,
}

#[derive(Deserialize)]
struct AccountReq {
    account: Account,
    #[serde(default)]
    aux_kinds: Vec<String>,
}

/// 前端传辅助核算维度的 kind 字符串列表（如 ["customer","project"]），
/// 由后端统一转成 AuxMask 位掩码，避免前端依赖位序。
fn build_aux_mask(kinds: &[String]) -> AuxMask {
    let mut mask = AuxMask::NONE;
    for k in kinds {
        if let Some(kind) = AuxKind::from_code(k) {
            mask.set(kind, true);
        }
    }
    mask
}

async fn list_begin(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<BeginRow>>, AppError> {
    user.require(Perm::Opening)?;
    let db = state.db_for(&user.book_key)?;
    Ok(Json(balances::list_begin(&db)?))
}

async fn save_begin(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(rows): Json<Vec<BeginRowInput>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let db = state.db_for(&user.book_key)?;
    let mut n = 0;
    for r in rows {
        let code = r.account_code.trim();
        if code.is_empty() {
            continue;
        }
        let yb = Money::parse_or_zero(&r.yb);
        let year_begin = match r.dir {
            Direction::Debit => yb,
            Direction::Credit => -yb,
        };
        let br = BeginRow {
            id: 0,
            account_code: code.to_string(),
            aux: r.aux,
            year_begin,
            debit_accum: Money::parse_or_zero(&r.ad),
            credit_accum: Money::parse_or_zero(&r.ac),
            qty_begin: r
                .qty
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .map(|s| Money::parse_or_zero(s)),
        };
        balances::upsert_begin(&db, &br)?;
        n += 1;
    }
    db.log(user.username(), "期初", "保存期初余额", &format!("保存 {} 条", n))?;
    Ok(Json(json!({"ok": true, "count": n})))
}

// ---------------- 操作日志 ----------------
async fn list_logs(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::user::AuditLog>>, AppError> {
    user.require(Perm::AuditLog)?;
    let db = state.db_for(&user.book_key)?;
    let limit: i64 = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(200);
    let logs = match q.get("q") {
        Some(kw) if !kw.trim().is_empty() => db.search_logs(kw.trim(), limit)?,
        _ => db.recent_logs(limit)?,
    };
    Ok(Json(logs))
}

// ---------------- 备份 / 恢复 ----------------
#[derive(Deserialize)]
struct BackupRestoreReq {
    file: String,
}

/// 强制把 WAL 合并回主文件（TRUNCATE），使文件级复制（`fs::copy`）拿到一致快照。
///
/// 数据库为 WAL 模式，最新提交可能滞留在 `-wal` 文件中；若不先 checkpoint 直接复制
/// 主文件，备份会缺最新数据，恢复后丢账。checkpoint 后主文件即完整、`-wal` 被清空。
fn checkpoint_wal(db: &findb::Db) -> Result<(), AppError> {
    let _row: (i64, i64, i64) = db
        .conn()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(findb::DbError::from)?;
    Ok(())
}

async fn list_backups(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    let dir = state.books_dir.join("backups");
    // 备份目录是全局共享的，只能列出属于当前账套的备份（文件名后缀 _<key>.fbk），
    // 否则租户 A 能枚举甚至恢复租户 B 的全套账。
    let suffix = format!("_{}.fbk", user.book_key);
    let mut items = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("fbk") {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.ends_with(&suffix) {
                    continue;
                }
                if let Ok(meta) = std::fs::metadata(&p) {
                    items.push(json!({
                        "name": name,
                        "size": meta.len(),
                        "mtime": meta.modified().map(|t| format!("{:?}", t)).unwrap_or_default(),
                    }));
                }
            }
        }
    }
    items.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    Ok(Json(json!({ "items": items })))
}

async fn create_backup(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    let key = user.book_key.clone();
    if !state.books_dir.join(format!("{key}.fbk")).exists() {
        return Err(AppError::not_found("账套文件不存在"));
    }
    let state2 = state.clone();
    let username = user.username().to_string();
    // checkpoint + fs::copy 都是阻塞 IO，放阻塞池，别占死 async worker
    let name = tokio::task::spawn_blocking(move || -> Result<String, AppError> {
        let src = state2.books_dir.join(format!("{key}.fbk"));
        let dir = state2.books_dir.join("backups");
        let _ = std::fs::create_dir_all(&dir);
        // 先 checkpoint 把 WAL 合并回主文件，再复制主文件即可得到完整一致快照
        let db = state2.db_for(&key)?;
        checkpoint_wal(&db)?;
        drop(db);
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let dst_name = format!("{}_{}.fbk", stamp, key);
        std::fs::copy(&src, dir.join(&dst_name))?;
        let db = state2.db_for(&key)?;
        db.log(&username, "系统", "备份账套", &format!("备份 {dst_name}"))?;
        Ok(dst_name)
    })
    .await
    .map_err(|e| AppError::Internal(format!("备份任务失败：{e}")))??;
    Ok(Json(json!({"ok": true, "name": name})))
}

async fn restore_backup(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BackupRestoreReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    // 路径安全：仅允许 backups 目录下的纯文件名，禁止任何路径穿越；
    // 且只接受属于当前账套的备份（后缀 _<key>.fbk），防止跨租户恢复。
    let file_name = match std::path::Path::new(&req.file).file_name().and_then(|s| s.to_str()) {
        Some(n) if !n.contains("..") && !n.contains('/') && !n.contains('\\') => n.to_string(),
        _ => return Err(AppError::bad_request("非法的备份文件名")),
    };
    if !file_name.ends_with(&format!("_{}.fbk", user.book_key)) {
        return Err(AppError::bad_request("该备份不属于当前账套"));
    }
    let src = state.books_dir.join("backups").join(&file_name);
    if !src.exists() {
        return Err(AppError::not_found("备份文件不存在"));
    }
    let key = user.book_key.clone();
    let username = user.username().to_string();
    let state2 = state.clone();
    // 覆盖主库 + 删 WAL + 重注册账套都是阻塞操作，放阻塞池
    tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        let dst = state2.books_dir.join(format!("{key}.fbk"));
        // 恢复前先 checkpoint 当前账套并自动备份一次，避免覆盖无法回退
        let _ = std::fs::create_dir_all(state2.books_dir.join("backups"));
        let db = state2.db_for(&key)?;
        checkpoint_wal(&db)?;
        drop(db);
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let auto_name = format!("auto_{}_{}.fbk", stamp, key);
        let _ = std::fs::copy(&dst, state2.books_dir.join("backups").join(&auto_name));
        std::fs::copy(&src, &dst)?;
        for ext in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(state2.books_dir.join(format!("{key}.fbk{ext}")));
        }
        // 重新注册账套，使后续请求以新文件重新打开
        if let Ok(path) = std::fs::canonicalize(&dst) {
            state2.books.unregister(&key);
            state2.books.register(&path, 16);
        }
        let db = state2.db_for(&key)?;
        db.log(&username, "系统", "恢复账套", &format!("从 {file_name} 恢复"))?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("恢复任务失败：{e}")))??;
    Ok(Json(json!({"ok": true})))
}

// ---------------- 凭证模板 ----------------
async fn list_templates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<template::Template>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    Ok(Json(template::list(&db)?))
}

async fn create_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(t): Json<template::Template>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let id = template::insert(&db, &t)?;
    db.log(user.username(), "凭证模板", "新建模板", &t.name)?;
    Ok(Json(json!({"id": id})))
}

async fn update_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(t): Json<template::Template>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let mut t = t;
    t.id = id;
    template::update(&db, &t)?;
    db.log(user.username(), "凭证模板", "修改模板", &t.name)?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = state.db_for(&user.book_key)?;
    template::delete(&db, id)?;
    db.log(user.username(), "凭证模板", "删除模板", &id.to_string())?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct TemplateGenerateReq {
    /// 期间 ymm，如 202601，缺省当前期间
    #[serde(default)]
    period: Option<i32>,
    /// 凭证日期，缺省期间末日
    #[serde(default)]
    date: String,
}

/// 由模板直接生成凭证并回写 last_period（周期性模板据此推进「本期到期」判断）
async fn generate_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<TemplateGenerateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let t = template::get(&db, id)?.ok_or_else(|| AppError::not_found("模板不存在"))?;
    let period = req
        .period
        .filter(|ym| *ym > 0)
        .map(Period::from_ymm)
        .unwrap_or_else(|| current_period(&state, &user));
    let date = req_date(&req.date, period.last_day())?;
    let word = "记";
    let no = vouchers::next_no(&db, period, word)?;
    let mut v = t.to_voucher(period, date, word, no as i64, user.username())?;
    let vid = vouchers::save(&db, &mut v)?;
    template::mark_generated(&db, id, period)?;
    db.log(user.username(), "凭证模板", "生成凭证", &format!("{} → 凭证 #{vid}", t.name))?;
    Ok(Json(json!({ "id": vid })))
}

async fn due_templates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<template::Template>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = q
        .get("period")
        .and_then(|s| s.parse::<i32>().ok())
        .map(Period::from_ymm)
        .unwrap_or_else(|| db.options().start_period);
    Ok(Json(template::due_list(&db, period)?))
}

// ---------------- 辅助核算档案 ----------------
async fn list_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AuxEntity>>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = state.db_for(&user.book_key)?;
    let kind = q
        .get("kind")
        .and_then(|s| AuxKind::from_code(s))
        .unwrap_or(AuxKind::Customer);
    Ok(Json(auxs::list(&db, &AuxQuery::kind(kind))?))
}

async fn create_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(e): Json<AuxEntity>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = state.db_for(&user.book_key)?;
    let id = auxs::insert(&db, &e)?;
    db.log(user.username(), "档案", "新建档案", &format!("{} {}", e.kind.label(), e.name))?;
    Ok(Json(json!({"id": id})))
}

async fn update_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(e): Json<AuxEntity>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = state.db_for(&user.book_key)?;
    let mut e = e;
    e.id = id;
    auxs::update(&db, &e)?;
    db.log(user.username(), "档案", "修改档案", &e.name)?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = state.db_for(&user.book_key)?;
    auxs::delete(&db, id)?;
    db.log(user.username(), "档案", "删除档案", &id.to_string())?;
    Ok(Json(json!({"ok": true})))
}

// ===========================================================================
// 工资管理（对齐桌面端 finui：工资表 / 个税明细 / 凭证生成，入口权限 VoucherNew）
// ===========================================================================

/// 从查询串取期间，缺省用当前期间
fn query_period(state: &WebState, user: &CurrentUser, q: &HashMap<String, String>) -> Period {
    q.get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(state, user))
}

/// 凭证生成请求里的日期，缺省用期间末日
fn req_date(s: &str, fallback: NaiveDate) -> Result<NaiveDate, AppError> {
    if s.trim().is_empty() {
        return Ok(fallback);
    }
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
}

#[derive(Deserialize)]
struct PayrollInput {
    employee: String,
    #[serde(default)]
    dept: String,
    gross: String,
    #[serde(default)]
    social: String,
    #[serde(default)]
    housing: String,
    #[serde(default)]
    deduction: String,
    /// 专项附加扣除
    #[serde(default)]
    additional: String,
    #[serde(default)]
    social_co: String,
    #[serde(default)]
    housing_co: String,
    #[serde(default)]
    memo: String,
}

async fn list_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<business::Payroll>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let rows = business::payroll_list(&db, period)?;
    // 「仅看本人经手的业务单据」：工资按员工姓名匹配当前登录人
    let rows: Vec<business::Payroll> = if user.user.data_scope.own_doc_only {
        rows.into_iter()
            .filter(|r| r.employee == user.user.display_name || r.employee == user.user.username)
            .collect()
    } else {
        rows
    };
    Ok(Json(rows))
}

/// 录入/修改一条工资：后端按累计预扣预缴法算个税与实发，前端无需自己算税
async fn save_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollInput>,
) -> Result<Json<business::Payroll>, AppError> {
    user.require(Perm::VoucherNew)?;
    let employee = r.employee.trim();
    if employee.is_empty() {
        return Err(AppError::bad_request("员工编码必填"));
    }
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let p = business::payroll_calc(
        &db,
        period,
        employee,
        r.dept.trim(),
        parse_money(&r.gross),
        parse_money(&r.social),
        parse_money(&r.housing),
        parse_money(&r.deduction),
        parse_money(&r.additional),
        parse_money(&r.social_co),
        parse_money(&r.housing_co),
        r.memo.trim(),
    )?;
    let id = business::payroll_upsert(&db, &p)?;
    db.log(
        user.username(),
        "工资",
        "保存工资行",
        &format!("{} {} 应发 {} 实发 {}", period.label(), employee, p.gross, p.net),
    )?;
    let mut out = p;
    out.id = id;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct GeneratePayrollReq {
    #[serde(default)]
    period: Option<String>,
    rows: Vec<PayrollInput>,
}

/// 批量生成本月工资表（逐条累计预扣预缴算税后落库）
async fn generate_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<GeneratePayrollReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = req
        .period
        .as_deref()
        .and_then(parse_period)
        .unwrap_or_else(|| current_period(&state, &user));
    let rows: Vec<(String, String, Money, Money, Money, Money, Money, Money, Money)> = req
        .rows
        .iter()
        .filter(|r| !r.employee.trim().is_empty())
        .map(|r| {
            (
                r.employee.trim().to_string(),
                r.dept.trim().to_string(),
                parse_money(&r.gross),
                parse_money(&r.social),
                parse_money(&r.housing),
                parse_money(&r.deduction),
                parse_money(&r.additional),
                parse_money(&r.social_co),
                parse_money(&r.housing_co),
            )
        })
        .collect();
    if rows.is_empty() {
        return Err(AppError::bad_request("没有可导入的工资行"));
    }
    let n = business::payroll_generate(&db, period, &rows)?;
    db.log(user.username(), "工资", "批量生成工资表", &format!("{} 生成 {} 条", period.label(), n))?;
    Ok(Json(json!({ "ok": true, "count": n })))
}

async fn delete_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = state.db_for(&user.book_key)?;
    // 删除前拦截已生成凭证的工资行
    let existing = business::payroll_get_by_id(&db, id)?;
    match existing {
        Some(p) if p.voucher_id.is_some() => {
            return Err(AppError::bad_request("该工资行已生成凭证，不能删除"));
        }
        None => return Err(AppError::not_found("工资行不存在")),
        _ => {}
    }
    business::payroll_delete(&db, id)?;
    db.log(user.username(), "工资", "删除工资行", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

async fn payroll_ytd(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<business::YtdPayroll>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let employee = q.get("employee").cloned().unwrap_or_default();
    if employee.trim().is_empty() {
        return Err(AppError::bad_request("缺少 employee 参数"));
    }
    Ok(Json(business::payroll_ytd(&db, period, employee.trim())?))
}

#[derive(Deserialize)]
struct PayrollAccrueReq {
    #[serde(default)]
    date: String,
    expense: String,
    wage_payable: String,
    social_payable: String,
    housing_payable: String,
}

/// 工资计提凭证
async fn payroll_accrue(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollAccrueReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_accrue_voucher(
        &db,
        period,
        date,
        r.expense.trim(),
        r.wage_payable.trim(),
        r.social_payable.trim(),
        r.housing_payable.trim(),
        user.username(),
    )?;
    db.log(user.username(), "工资", "生成计提凭证", &format!("{} 凭证 {:?}", period.label(), id))?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct PayrollSocialReq {
    #[serde(default)]
    date: String,
    social_payable: String,
    housing_payable: String,
    personal_payable: String,
    bank_account: String,
}

/// 缴纳社保公积金凭证
async fn payroll_social_pay(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollSocialReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_social_voucher(
        &db,
        period,
        date,
        r.social_payable.trim(),
        r.housing_payable.trim(),
        r.personal_payable.trim(),
        r.bank_account.trim(),
        user.username(),
    )?;
    db.log(user.username(), "工资", "生成社保缴纳凭证", &format!("{} 凭证 {:?}", period.label(), id))?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct PayrollPayReq {
    #[serde(default)]
    date: String,
    payable_account: String,
    bank_account: String,
    tax_account: String,
    social_account: String,
}

/// 工资发放凭证
async fn payroll_pay(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollPayReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_pay_voucher(
        &db,
        period,
        date,
        r.payable_account.trim(),
        r.bank_account.trim(),
        r.tax_account.trim(),
        r.social_account.trim(),
        user.username(),
    )?;
    db.log(user.username(), "工资", "生成发放凭证", &format!("{} 凭证 {:?}", period.label(), id))?;
    Ok(Json(json!({ "id": id })))
}

// ===========================================================================
// 费用报销（对齐桌面端 finui：草稿→提交→审批→支付→生成凭证，入口权限 VoucherNew）
// ===========================================================================

#[derive(Deserialize)]
struct ClaimItemInput {
    expense_account: String,
    amount: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
struct ClaimInput {
    /// 期间 ymm，如 202601
    #[serde(default)]
    period: Option<i32>,
    /// 业务日期 YYYY-MM-DD
    biz_date: String,
    applicant: String,
    #[serde(default)]
    dept: String,
    reason: String,
    amount: String,
    #[serde(default)]
    items: Vec<ClaimItemInput>,
}

fn claim_items(items: &[ClaimItemInput]) -> Vec<business::ClaimItem> {
    items
        .iter()
        .map(|i| business::ClaimItem {
            expense_account: i.expense_account.trim().to_string(),
            amount: parse_money(&i.amount),
            memo: i.memo.trim().to_string(),
        })
        .collect()
}

async fn list_claims(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<business::Claim>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let status = q.get("status").map(|s| business::ClaimStatus::parse(s));
    let rows = business::claim_list(&db, period, status)?;
    // 「仅看本人经手的业务单据」：报销按申请人匹配当前登录人
    let rows: Vec<business::Claim> = if user.user.data_scope.own_doc_only {
        rows.into_iter()
            .filter(|c| c.applicant == user.user.display_name || c.applicant == user.user.username)
            .collect()
    } else {
        rows
    };
    Ok(Json(rows))
}

async fn next_claim_no(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let no = business::claim_next_no(&db, period)?;
    Ok(Json(json!({ "no": no })))
}

async fn create_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(r): Json<ClaimInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let applicant = r.applicant.trim();
    if applicant.is_empty() {
        return Err(AppError::bad_request("申请人必填"));
    }
    let db = state.db_for(&user.book_key)?;
    let period = r
        .period
        .filter(|ym| *ym > 0)
        .map(Period::from_ymm)
        .unwrap_or_else(|| current_period(&state, &user));
    let date = req_date(&r.biz_date, period.last_day())?;
    let c = business::Claim {
        id: 0,
        period,
        no: business::claim_next_no(&db, period)?,
        biz_date: date,
        applicant: applicant.to_string(),
        dept: r.dept.trim().to_string(),
        reason: r.reason.trim().to_string(),
        amount: parse_money(&r.amount),
        status: business::ClaimStatus::Draft,
        items: claim_items(&r.items),
        approver: String::new(),
        approved_at: None,
        payer: String::new(),
        paid_at: None,
        voucher_id: None,
        created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    let id = business::claim_insert(&db, &c)?;
    db.log(user.username(), "报销", "新增报销单", &format!("{} {} {}", c.no, applicant, c.amount))?;
    Ok(Json(json!({ "id": id, "no": c.no })))
}

async fn update_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let mut c = business::claim_get(&db, id)?
        .ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !c.status.editable() {
        return Err(AppError::bad_request("当前状态不允许修改内容"));
    }
    if c.voucher_id.is_some() {
        return Err(AppError::bad_request("已生成凭证的报销单不能修改"));
    }
    let date = req_date(&r.biz_date, c.period.last_day())?;
    c.biz_date = date;
    c.applicant = r.applicant.trim().to_string();
    c.dept = r.dept.trim().to_string();
    c.reason = r.reason.trim().to_string();
    c.amount = parse_money(&r.amount);
    c.items = claim_items(&r.items);
    business::claim_update(&db, &c)?;
    db.log(user.username(), "报销", "修改报销单", &format!("{} {}", c.no, c.amount))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = state.db_for(&user.book_key)?;
    let c = business::claim_get(&db, id)?
        .ok_or_else(|| AppError::not_found("报销单不存在"))?;
    // 预检：引擎的通用错误会映射成 500，这里给出明确的 400
    if c.voucher_id.is_some() {
        return Err(AppError::bad_request("该报销单已生成凭证，请先删除凭证"));
    }
    let label = c.no.clone();
    business::claim_delete(&db, id)?;
    db.log(user.username(), "报销", "删除报销单", &label)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ClaimTransitionReq {
    /// draft / submitted / approved / rejected / paid
    status: String,
}

async fn claim_transition(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let to = business::ClaimStatus::parse(&r.status);
    business::claim_transition(&db, id, to, user.username())?;
    db.log(user.username(), "报销", "状态流转", &format!("#{id} → {}", to.label()))?;
    Ok(Json(json!({ "ok": true, "status": to })))
}

#[derive(Deserialize)]
struct ClaimVoucherReq {
    /// 贷方支付科目（如 100201 银行存款）
    pay_account: String,
}

async fn claim_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let pay = r.pay_account.trim();
    if pay.is_empty() {
        return Err(AppError::bad_request("请填写支付科目"));
    }
    // 预检给出友好的 400（引擎层也有同样拦截，此处避免落到 500）
    let c = business::claim_get(&db, id)?
        .ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if c.voucher_id.is_some() {
        return Err(AppError::bad_request("该报销单已生成过凭证"));
    }
    let vid = business::claim_voucher(&db, id, pay, user.username())?;
    db.log(user.username(), "报销", "生成凭证", &format!("#{id} 凭证 #{vid}"))?;
    Ok(Json(json!({ "id": vid })))
}
