//! HTTP 请求处理函数

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
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
    period_to_str, parse_money_checked, parse_period, clear_cookie_header, AppError, CurrentUser, RealmUser,
    WebState,
};

const SESSION_SECS: i64 = 60 * 60 * 24 * 7;
/// 单账号最多可自建账套数（防无限建账占满磁盘）
const MAX_BOOKS_PER_USER: i64 = 10;

/// M-9 会话门禁：在路由匹配前把未认证的 /api/* 统一拦成 401——
/// 否则未登录探测者可用 401（真实接口）/ 404（不存在）/ 405（方法未注册）
/// 的差异枚举出全部接口清单。公开接口放行；只做令牌校验且文案与
/// RealmUser/CurrentUser 提取器共用 session_of 逐字一致；完整身份对账
/// 仍由各接口自己的提取器完成，不改变任何既有授权语义。
async fn api_auth_gate(
    State(state): State<Arc<WebState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, AppError> {
    let path = req.uri().path();
    if path.starts_with("/api/")
        && path != "/api/health"
        && path != "/api/login"
        && path != "/api/logout"
        && path != "/api/setup/status"
    {
        crate::state::session_of(req.headers(), &state)?;
    }
    Ok(next.run(req).await)
}

/// 统一 fallback：/api/* 未匹配 → 已登录回 404、未登录回 401（M-9）；
/// 其余路径走静态资源（SPA 资产；"/" 已由 serve_index 路由处理）。
/// 静态服务从 main::build_app 移入此处，正是为了让 /api/* 不再落到
/// 静态 404——门禁与本 fallback 双重覆盖，对 axum 的 layer/fallback
/// 包裹顺序不敏感。
async fn spa_fallback(
    State(state): State<Arc<WebState>>,
    req: axum::extract::Request,
) -> Response {
    if req.uri().path().starts_with("/api/") {
        let (mut parts, _body) = req.into_parts();
        return match CurrentUser::from_request_parts(&mut parts, &state).await {
            Ok(_) => AppError::not_found("接口不存在").into_response(),
            Err(e) => e.into_response(),
        };
    }
    use tower::ServiceExt;
    tower_http::services::ServeDir::new(state.static_dir.clone())
        .oneshot(req)
        .await
        // tower-http 的响应体类型映射为 axum Body
        .map(|resp| resp.map(axum::body::Body::new))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

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
        // 期末处理（与桌面端对齐）：预检 / 结转损益 / 年末结转 / 结账 / 反结账
        .route("/api/periods/:ymm/precheck", get(period_precheck))
        .route("/api/periods/:ymm/carry-forward", post(period_carry_forward))
        .route("/api/periods/:ymm/year-end", post(period_year_end))
        .route("/api/periods/:ymm/close", post(period_close))
        .route("/api/periods/:ymm/unclose", post(period_unclose))
        // 科目 / 凭证
        .route("/api/accounts", get(list_accounts))
        .route("/api/accounts/fill-defaults", post(fill_default_accounts))
        .route("/api/vouchers/next-no", get(next_voucher_no))
        .route("/api/vouchers", get(list_vouchers).post(save_voucher))
        .route("/api/vouchers/batch-post", post(voucher_batch_post))
        .route("/api/vouchers/:id", get(get_voucher))
        .route("/api/vouchers/:id/post", post(voucher_post))
        .route("/api/vouchers/:id/unpost", post(voucher_unpost))
        .route("/api/vouchers/:id/audit", post(voucher_audit))
        .route("/api/vouchers/:id/unaudit", post(voucher_unaudit))
        .route("/api/vouchers/:id/sign", post(voucher_sign))
        .route("/api/vouchers/:id/unsign", post(voucher_unsign))
        .route("/api/vouchers/:id/reverse", post(voucher_reverse))
        .route("/api/vouchers/:id/delete", post(voucher_delete))
        .route("/api/vouchers/renumber", post(voucher_renumber))
        // 凭证附件（上传/下载/删除）
        .route(
            "/api/vouchers/:id/attachments",
            get(list_voucher_attachments).post(upload_voucher_attachment),
        )
        .route(
            "/api/attachments/:id",
            get(download_attachment).delete(delete_attachment),
        )
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
        .route("/api/ledger/general", get(get_general_ledger))
        .route("/api/ledger/journal", get(get_journal))
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
        // 数据导出（CSV，需 Export 权限）
        .route("/api/export/vouchers", get(export_vouchers))
        .route("/api/export/ledger", get(export_ledger))
        .route("/api/export/payroll", get(export_payroll))
        .route("/api/export/claims", get(export_claims))
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
        // 辅助账 / 数量金额账（与桌面端对齐）
        .route("/api/reports/aux-balance", get(get_aux_balance))
        .route("/api/reports/qty-balance", get(get_qty_balance))
        // 自定义报表（UFO 公式，与桌面端对齐）
        .route(
            "/api/custom-reports",
            get(list_custom_reports).post(save_custom_report),
        )
        .route("/api/custom-reports/:key", get(get_custom_report))
        .route("/api/custom-reports/:key/delete", post(delete_custom_report))
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
        .route("/api/procure/po", get(list_po).post(save_po))
        .route("/api/procure/po/:id/transition", post(transition_po))
        .route("/api/procure/po/:id/delete", post(delete_po))
        .route("/api/procure/po/print-form", get(print_po_form))
        .route("/api/sales/quote", get(list_quotation).post(save_quotation))
        .route("/api/sales/quote/:id/approve", post(approve_quotation))
        .route("/api/sales/quote/:id/to-order", post(convert_quotation))
        .route("/api/sales/shipment", post(add_so_shipment))
        .route("/api/sales/payment", post(add_so_payment))
        .route("/api/sales/return", post(add_so_return))
        .route("/api/sales/credit", get(get_credit_check))
        .route("/api/sales/track", get(get_so_track))
        .route("/api/sales/stats", get(get_sales_stats))
        .route("/api/sales/so", get(list_so).post(save_so))
        .route("/api/sales/so/:id/transition", post(transition_so))
        .route("/api/sales/so/:id/delete", post(delete_so))
        .route("/api/sales/so/print-form", get(print_so_form))
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
        // 资金：票据 / 融资 / 资金日报 / 资金预测（台账与总账联动：流转/结清自动生成凭证）
        .route("/api/funds/bills", get(list_bills).post(save_bill))
        .route("/api/funds/bills/:id/status", post(bill_transition))
        .route("/api/funds/bills/:id/delete", post(delete_bill))
        .route("/api/funds/bills/:id/voucher", post(bill_voucher))
        .route("/api/funds/loans", get(list_loans).post(save_loan))
        .route("/api/funds/loans/:id/settle", post(loan_settle))
        .route("/api/funds/loans/:id/delete", post(delete_loan))
        .route("/api/funds/loans/:id/voucher", post(loan_voucher))
        .route("/api/funds/daily", get(get_funds_daily))
        .route("/api/funds/daily-by-date", get(get_funds_daily_date))
        .route("/api/funds/budget", get(get_funds_budget))
        .route(
            "/api/funds/cash-counts",
            get(list_cash_counts).post(save_cash_count),
        )
        .route("/api/funds/cash-counts/:id/delete", post(delete_cash_count))
        .route("/api/funds/cash-counts/:id/voucher", post(cash_count_voucher))
        .route("/api/funds/day-clear", get(list_day_clear).post(set_day_clear))
        .route("/api/funds/checks", get(list_checks).post(save_check))
        .route("/api/funds/checks/:id/status", post(check_status))
        .route("/api/funds/checks/:id/delete", post(delete_check))
        .route("/api/funds/advances", get(list_advances).post(save_advance))
        .route("/api/funds/advances/:id/pay", post(pay_advance))
        .route("/api/funds/advances/:id/settle", post(settle_advance))
        .route("/api/funds/advances/:id/delete", post(delete_advance))
        .route("/api/funds/receipts", get(list_receipts).post(create_receipt))
        .route("/api/funds/receipts/:id/delete", post(delete_receipt))
        .route("/api/funds/receipts/print-form", get(print_receipt_form))
        .route("/api/funds/forecast", get(get_funds_forecast))
        // 预算分析
        .route("/api/budget/analysis", get(get_budget_analysis))
        // 成本：计价配置 + 期末结价
        .route("/api/cost/configs", get(list_cost_configs).post(save_cost_method))
        .route("/api/cost/configs/:item/delete", post(clear_cost_method))
        .route("/api/cost/period-end", get(run_period_end_cost).post(run_period_end_cost))
        // 固定资产（与桌面端对齐）：卡片 / 折旧计划 / 计提 / 清理
        .route("/api/assets", get(list_assets).post(create_asset))
        .route("/api/assets/:id", put(update_asset).delete(delete_asset))
        .route("/api/assets/:id/depreciations", get(list_asset_deps))
        .route("/api/assets/:id/dispose", post(dispose_asset))
        .route("/api/assets/depreciate", post(depreciate_assets))
        .route("/api/assets/depreciations/delete-period", post(delete_asset_deps_period))
        // 银行对账（与桌面端对齐）
        .route("/api/bank", get(get_bank))
        .route("/api/bank/import", post(import_bank))
        .route("/api/bank/auto-match", post(auto_match_bank))
        .route("/api/bank/link", post(link_bank))
        .route("/api/bank/unlink", post(unlink_bank))
        .route("/api/bank/clear", post(clear_bank))
        // 往来核销（与桌面端对齐）
        .route("/api/settle/open", get(get_settle_open))
        .route("/api/settle/auto", post(auto_settle_endpoint))
        .route("/api/settle/run", post(manual_settle_endpoint))
        .route("/api/settle/records", get(list_settle_records))
        .route("/api/settle/unsettle", post(unsettle_endpoint))
        .route("/api/settle/aging", get(get_settle_aging))
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
        .layer(axum::middleware::from_fn(csrf_guard))
        // 附件上传最大 10MB（引擎限制），请求体留一点余量
        .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024))
        // M-9 会话门禁：路由匹配前统一 401 未认证的 /api/*（公开接口除外）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api_auth_gate,
        ))
        // M-9 统一 fallback：/api/* 未匹配 → 已登录 404 / 未登录 401；其余走静态资源
        .fallback(spa_fallback)
        .with_state(state)
}

/// CSRF 纵深防御：对状态变更请求校验 Sec-Fetch-Site / Origin / Referer。
///
/// 主防线仍是 SameSite=Lax + JSON Content-Type（跨站表单拿不到 Cookie 也发不出 JSON），
/// 这里额外拦"同站被攻破的其他源/旧浏览器/代理改写"等场景。非浏览器客户端
/// （curl/脚本）不带这些头，不拦截——它们本来也不依赖 Cookie 自动携带。
async fn csrf_guard(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use axum::http::Method;
    let method = req.method().clone();
    if matches!(method, Method::POST | Method::PUT | Method::DELETE | Method::PATCH) {
        let reject = || {
            (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "跨站请求被拒绝" })),
            )
                .into_response()
        };
        if let Some(site) = req
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
        {
            if site != "same-origin" && site != "none" {
                return reject();
            }
        }
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_ascii_lowercase());
        if let Some(origin) = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
        {
            if !origin_host_matches(origin, host.as_deref()) {
                return reject();
            }
        } else if let Some(referer) = req
            .headers()
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
        {
            if !origin_host_matches(referer, host.as_deref()) {
                return reject();
            }
        }
    }
    next.run(req).await
}

/// `scheme://host[:port]/...` 的主机是否与 Host 头一致（无 Host 头时无法比较，放行）
fn origin_host_matches(url: &str, host: Option<&str>) -> bool {
    let Some(host) = host else { return true };
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let origin_host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    !origin_host.is_empty() && origin_host == host
}

/// 返回 SPA 首页，并把 `{{V}}` 占位符替换为当前资源版本号。
/// 版本号由静态文件 mtime 计算，任何前端改动都会使 URL 变化 → 浏览器缓存自动失效。
async fn serve_index(State(state): State<Arc<WebState>>) -> Response {
    use axum::response::Html;
    let path = state.static_dir.join("index.html");
    let html = match std::fs::read_to_string(&path) {
        Ok(h) => h,
        Err(e) => {
            // 路径只进服务端日志，避免未登录即可探测文件系统布局
            eprintln!("[finweb] 读取 index.html 失败 {}: {e}", path.display());
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "服务器内部错误，请稍后重试" })),
            )
                .into_response();
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

/// 取来源 IP：优先反代写入的 X-Forwarded-For（首个地址）/ X-Real-IP。
/// 直连（未配反代）时取不到，返回 None，仅跳过 IP 维度限流，账号维度仍生效。
fn client_ip(headers: &HeaderMap) -> Option<String> {
    let pick = |v: &str| -> Option<String> {
        let s = v.split(',').next().unwrap_or("").trim().to_string();
        (!s.is_empty() && s.len() <= 64).then_some(s)
    };
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(pick)
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(pick)
        })
}

async fn post_login(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Result<Response, AppError> {
    // 全局登录（认平台身份库，而非某一套账）
    let username = req.username.trim().to_string();
    // 设备指纹必须非空：空串会让"一人一机"首次绑定写成空值从而永久绕过校验
    let device_id = req.device_id.trim().to_string();
    if device_id.is_empty() || device_id.len() > 128 {
        return Err(AppError::bad_request("缺少有效的设备标识，请刷新页面后重试"));
    }
    let ip = client_ip(&headers);
    // 登录限流：先查是否已被锁定，避免锁定期内继续做昂贵/可枚举的密码校验
    if let Err(secs) = state.login_limiter.check(&username) {
        let mins = secs.div_ceil(60).max(1);
        return Err(AppError::rate_limited(
            format!("尝试过于频繁，请约 {mins} 分钟后再试"),
            secs,
        ));
    }
    if let Some(ip) = &ip {
        if let Err(secs) = state.login_ip_limiter.check(ip) {
            let mins = secs.div_ceil(60).max(1);
            return Err(AppError::rate_limited(
                format!("该来源尝试过于频繁，请约 {mins} 分钟后再试"),
                secs,
            ));
        }
    }
    let ru = match state.realm.authenticate(&username, &req.password) {
        Ok(Some(ru)) => ru,
        Ok(None) => {
            state.login_limiter.record_failure(&username);
            if let Some(ip) = &ip {
                state.login_ip_limiter.record_failure(ip);
            }
            return Err(AppError::unauthorized("用户名或口令错误"));
        }
        Err(e) => return Err(AppError::from(e)),
    };
    // 登录成功，清空该账号的失败计数
    state.login_limiter.clear(&username);
    if let Some(ip) = &ip {
        state.login_ip_limiter.clear(ip);
    }
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
    // 权限调整收归管理员：即使被额外授予 UserManage，非管理员也不能铸号分配角色/权限
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以创建账号并分配权限"));
    }
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
    // 权限调整收归管理员（2026-09 审计）：非管理员即使持有 UserManage，也不能调整
    // 其他账号的角色/权限矩阵/数据范围/停用；显示名、备注等非授权字段仍可代改。
    if username != user.username() {
        let touches_grant = req.role.is_some()
            || req.extra_perms.is_some()
            || req.deny_perms.is_some()
            || req.data_scope.is_some()
            || req.disabled.is_some();
        if touches_grant && !user.user.is_admin() {
            return Err(AppError::forbidden("只有管理员可以调整其他账号的权限"));
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
    // 平台口令是全局的：账套管理员只允许重置「仅属于本账套、且不拥有任何账套」的
    // 成员。否则"把任意平台账号邀请进自己的账套，再重置其全局口令"即可跨租户
    // 接管/锁死他人账号（受害者口令被改，且会同步覆盖其名下所有账套）。
    let caller_is_platform_admin = state
        .realm
        .get_user(user.username())?
        .map(|u| u.is_admin)
        .unwrap_or(false);
    if !caller_is_platform_admin {
        if state.realm.count_books_of(&username)? > 0 {
            return Err(AppError::forbidden(
                "该账号拥有自己的账套，账套管理员不能重置其平台口令，请由平台管理员处理",
            ));
        }
        if state.realm.count_books_containing(&state.books_dir, &username)? > 1 {
            return Err(AppError::forbidden(
                "该账号还属于其他账套，账套管理员不能重置其平台口令，请由平台管理员处理",
            ));
        }
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
    // 目标必须是本账套成员：否则凭用户名即可强制下线任意平台用户的全部会话
    if users::get(&db, &username)?.is_none() {
        return Err(AppError::not_found("该用户不在当前账套"));
    }
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
    // 删号 = 调整他人账号，收归管理员
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以删除账号"));
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
    // 落库前校验：非法启用期间会灌进会话并污染年度累计口径，后续 first_day/last_day panic
    period_checked(opts.start_period.ymm())?;
    if opts.code_scheme.is_empty()
        || opts.code_scheme.len() > 8
        || opts.code_scheme.iter().any(|s| !(1..=12u8).contains(s))
    {
        return Err(AppError::bad_request("科目编码级长非法（每级 1-12 位，最多 8 级）"));
    }
    if opts.voucher_words.is_empty() || opts.voucher_words.iter().any(|w| w.trim().is_empty()) {
        return Err(AppError::bad_request("凭证字号不能为空"));
    }
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
    // 会话/配置中的期间也做校验：脏数据不能让后续 first_day/last_day panic
    Period::from_ymm_checked(ymm)
        .or_else(|_| Period::from_ymm_checked(state.default_period))
        .unwrap_or_else(|_| Period::default())
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
    user.require(Perm::Report)?;
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
    user.require(Perm::Report)?;
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

/// 期末结账请求
#[derive(Deserialize, Default)]
struct ClosePeriodReq {
    /// 是否要求先结转损益才能结账
    #[serde(default)]
    pub require_carry: bool,
}

/// 期末预检：返回结账前需要处理的问题清单与损益概况
async fn period_precheck(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = state.db_for(&user.book_key)?;
    let issues = periods::precheck(&db, period, true)?;
    let chart = accounts::chart(&db)?;
    // 预检展示与【结转】按钮同口径：结转按含草稿取数（见 period_carry_forward）
    let snap =
        BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let pl_rows = snap.profit_loss_rows(&chart);
    let profit = snap
        .for_account(fincore::engine::period_end::PROFIT_ACCOUNT, None)
        .end();
    Ok(Json(json!({
        "period": period_to_str(period),
        "issues": issues,
        "pl_count": pl_rows.len(),
        "profit_balance": profit.fmt_money(),
        "closed_upto": periods::closed_upto(&db)?.map(period_to_str),
    })))
}

/// 结转损益：生成一张把损益类科目净额转入「本年利润」的凭证
async fn period_carry_forward(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CarryForward)?;
    let period = period_checked(ymm)?;
    let db = state.db_for(&user.book_key)?;
    let chart = accounts::chart(&db)?;
    // 结转是"操作"不是报表：按含未记账（排除作废）取数。结转生成的凭证本身是草稿，
    // 若按已记账口径取数，草稿看不见 → 第二次调用时损益仍未清零 → 重复结转。
    // 结账前 checklist 要求全部记账，届时两种口径结果相同（H-3 定案的已记账口径
    // 用于余额表/报表/账簿，不用于本操作的幂等判定）。
    let snap =
        BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let rows = snap.profit_loss_rows(&chart);
    if rows.is_empty() {
        return Err(AppError::bad_request("本期损益类科目没有发生额，无需结转"));
    }
    let date = period.last_day();
    let word = db
        .voucher_words()
        .first()
        .cloned()
        .unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no(&db, period, &word)?;
    let mut v = fincore::engine::period_end::generate_carry_forward(
        period,
        date,
        &word,
        no,
        &rows,
        &chart,
        fincore::engine::period_end::PROFIT_ACCOUNT,
        user.username(),
    )?;
    let id = vouchers::save(&db, &mut v)?;
    db.log(
        user.username(),
        "期末",
        "结转损益",
        &format!("{} 凭证 #{}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id, "voucher_no": v.voucher_no() })))
}

/// 年末结转：把「本年利润」余额转入「利润分配—未分配利润」
async fn period_year_end(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CarryForward)?;
    let period = period_checked(ymm)?;
    let db = state.db_for(&user.book_key)?;
    let chart = accounts::chart(&db)?;
    // 同 period_carry_forward：含草稿取数，保证第一次生成的结转草稿能清零 4103，
    // 第二次调用（余额 0）才会被拒，否则同一笔利润会被结转两次。
    let snap =
        BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let profit = snap
        .for_account(fincore::engine::period_end::PROFIT_ACCOUNT, None)
        .end();
    let date = period.last_day();
    let word = db
        .voucher_words()
        .first()
        .cloned()
        .unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no(&db, period, &word)?;
    let mut v = fincore::engine::period_end::generate_year_end_carry(
        period,
        date,
        &word,
        no,
        profit,
        fincore::engine::period_end::UNDISTRIBUTED_ACCOUNT,
        &chart,
        user.username(),
    )?;
    let id = vouchers::save(&db, &mut v)?;
    db.log(
        user.username(),
        "期末",
        "年末结转",
        &format!("{} 凭证 #{}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id, "voucher_no": v.voucher_no() })))
}

/// 期末结账：有未处理问题时返回 400 + 问题清单
async fn period_close(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
    Json(req): Json<ClosePeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = state.db_for(&user.book_key)?;
    let issues = periods::close(&db, period, user.username(), req.require_carry)?;
    if !issues.is_empty() {
        return Err(AppError::bad_request(issues.join("；")));
    }
    Ok(Json(json!({ "ok": true, "closed": period_to_str(period) })))
}

/// 反结账
async fn period_unclose(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = state.db_for(&user.book_key)?;
    periods::unclose(&db, period, user.username())?;
    Ok(Json(json!({ "ok": true, "unclosed": period_to_str(period) })))
}

// ---------------------------------------------------------------------------
// 科目 / 凭证
// ---------------------------------------------------------------------------

async fn list_accounts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<fincore::Account>>, AppError> {
    user.require(Perm::Report)?;
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

/// 「仅看本人经手的业务单据」（own_doc_only）：工资按员工、报销按申请人匹配
/// 当前登录人的用户名/显示名。只过滤列表是不够的——按 id/参数的读写入口都要过这里。
fn doc_in_scope(user: &CurrentUser, who: &str) -> bool {
    if !user.user.data_scope.own_doc_only {
        return true;
    }
    let who = who.trim();
    who == user.user.username || who == user.user.display_name
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
    // 上限 1000：limit 传负数时 SQLite 视为不限制，会把全库凭证+分录读进内存
    query.limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| v.clamp(1, 1000))
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

    // 修改时保留原分录的未提交要素（辅助核算/数量/外币/结算号等）
    let mut prev_entries: Vec<Entry> = Vec::new();
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
        // 启用审核环节的账套：已审核凭证须先反审核（预检给 400，避免落到引擎错误的 500）
        if existing.status == VoucherStatus::Audited && db.options().enable_audit {
            return Err(AppError::bad_request("已审核凭证不能修改，请先反审核"));
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
        prev_entries = existing.entries.clone();
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
    for (i, e) in req.entries.iter().enumerate() {
        let account_code = normalize_bank_account(&chart, &e.account_code)
            .unwrap_or_else(|| e.account_code.clone());
        let mut en = Entry::new(e.line, account_code.clone(), e.summary.clone());
        en.debit = parse_money_checked(&e.debit)?;
        en.credit = parse_money_checked(&e.credit)?;
        // 未提交的要素沿用原行：Web 编辑桌面录入的凭证时，辅助/数量/外币/结算号不丢
        if let Some(prev) = prev_entries.get(i) {
            en.aux = prev.aux.clone();
            en.qty = prev.qty;
            en.price = prev.price;
            en.currency = prev.currency.clone();
            en.rate = prev.rate;
            en.amount_for = prev.amount_for;
            en.settle_type = prev.settle_type.clone();
            en.settle_no = prev.settle_no.clone();
            en.biz_date = prev.biz_date;
        }
        if let Some(aux) = &e.aux {
            en.aux = aux.clone();
        }
        if let Some(q) = e.qty {
            en.qty = Some(q);
        }
        if let Some(p) = e.price {
            en.price = Some(p);
        }
        if let Some(cur) = e
            .currency
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            en.currency = Some(cur.to_ascii_uppercase());
        }
        if let Some(r) = e.rate {
            en.rate = Some(r);
        }
        if let Some(a) = e.amount_for {
            en.amount_for = Some(a);
        }
        if let Some(cf) = e.cf.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            en.aux.cash_flow = Some(cf.to_string());
        }
        // 银行科目：尾号存入辅助核算银行字段（仅在未显式提供时兜底）
        if en.account_code.starts_with("1002") && en.aux.bank.is_none() {
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
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权记账该凭证"));
    }
    if v.status == VoucherStatus::Posted {
        return Ok(Json(json!({"ok": true, "already_posted": true})));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("该凭证不参与账簿汇总"));
    }
    // 启用审核环节的账套：先审核再记账（预检给 400，避免落到引擎错误的 500）
    if db.options().enable_audit && v.status != VoucherStatus::Audited {
        return Err(AppError::bad_request("该账套启用了审核环节，请先审核凭证再记账"));
    }
    vouchers::post(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 审核（未记账 → 已审核）
async fn voucher_audit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherAudit)?;
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权审核该凭证"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能审核"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能审核"));
    }
    vouchers::audit(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 反审核（已审核 → 未记账）
async fn voucher_unaudit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherUnaudit)?;
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权反审核该凭证"));
    }
    if v.status != VoucherStatus::Audited {
        return Err(AppError::bad_request("只有已审核凭证才能反审核"));
    }
    vouchers::unaudit(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 出纳签字（未记账凭证记录签字人；require_cashier 账套作为现金/银行凭证记账前置）
async fn voucher_sign(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权对该凭证签字"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能签字，请先反记账"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能签字"));
    }
    vouchers::sign(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 取消出纳签字
async fn voucher_unsign(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权对该凭证取消签字"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能取消签字，请先反记账"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能取消签字"));
    }
    vouchers::unsign(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

async fn voucher_batch_post(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BatchPostReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherPost)?;
    let db = state.db_for(&user.book_key)?;
    // 数据范围过滤：仅记账本人可见的凭证，越权 id 直接拒掉并计数
    let mut allowed: Vec<i64> = Vec::new();
    let mut denied = 0usize;
    for id in &req.ids {
        match vouchers::get(&db, *id)? {
            Some(v) if user.user.can_see_voucher(&v) => allowed.push(*id),
            _ => denied += 1,
        }
    }
    let (n, errs) = vouchers::post_many(&db, &allowed, user.username())?;
    db.log(user.username(), "凭证", "批量记账", &format!("{n} 张（拒绝 {denied} 张越权）"))?;
    Ok(Json(json!({ "ok": n, "errors": errs, "denied": denied })))
}

async fn voucher_unpost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 反记账（已记账 → 未记账），修改前需先反记账
    user.require(Perm::VoucherUnpost)?;
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权反记账该凭证"));
    }
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
    // 缺省用期间末日：红字冲销常发生在已结账的历史期间，用"今天"会与所属期间不一致
    let date = if req.date.is_empty() {
        period.last_day()
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
    // 断号重排会重排该期间全部字号（含他人凭证），数据范围受限的账号不得操作
    let scope = &user.user.data_scope;
    if scope.own_voucher_only
        || !scope.account_from.trim().is_empty()
        || !scope.account_to.trim().is_empty()
    {
        return Err(AppError::forbidden("数据范围受限的账号不能执行断号重排"));
    }
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
    // 启用审核环节的账套：已审核凭证须先反审核
    if v.status == VoucherStatus::Audited && db.options().enable_audit {
        return Err(AppError::bad_request("已审核凭证不能删除，请先反审核"));
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
// 凭证附件（上传/下载/删除；数据超过阈值落 .attachments 目录，否则入库）
// ---------------------------------------------------------------------------

fn attachment_json(a: &findb::attach::Attachment) -> serde_json::Value {
    json!({
        "id": a.id,
        "voucher_id": a.voucher_id,
        "name": a.name,
        "kind": a.kind,
        "size": a.size,
        "size_text": a.size_text(),
        "is_image": a.is_image(),
        "added_by": a.added_by,
        "added_at": a.added_at,
    })
}

async fn list_voucher_attachments(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该凭证的附件"));
    }
    let list: Vec<serde_json::Value> = findb::attach::list(&db, id)?
        .iter()
        .map(attachment_json)
        .collect();
    Ok(Json(list))
}

async fn upload_voucher_attachment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = state.db_for(&user.book_key)?;
    let v = vouchers::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权为该凭证上传附件"));
    }
    let mut name = String::new();
    let mut data: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("上传解析失败：{e}")))?
    {
        if field.name() == Some("file") {
            name = field.file_name().unwrap_or("attachment").to_string();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("读取上传内容失败：{e}")))?;
            data = Some(bytes.to_vec());
            break;
        }
    }
    let data = data.ok_or_else(|| AppError::bad_request("缺少文件字段 file"))?;
    if data.is_empty() {
        return Err(AppError::bad_request("上传文件为空"));
    }
    if data.len() > findb::attach::MAX_FILE_SIZE {
        return Err(AppError::bad_request("文件超过 10MB 上限"));
    }
    let aid = findb::attach::add(&db, id, &name, &data, user.username())?;
    db.log(
        user.username(),
        "凭证",
        "上传附件",
        &format!("{} 附件#{} {}", v.voucher_no(), aid, name),
    )?;
    Ok(Json(json!({ "id": aid })))
}

async fn download_attachment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let a = findb::attach::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("附件不存在".to_string()))?;
    let v = vouchers::get(&db, a.voucher_id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该附件"));
    }
    let data = findb::attach::read(&db, id)?;
    let ctype = match a.kind.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    };
    let encoded: String = a.name.bytes().map(|b| format!("%{b:02X}")).collect();
    let mut resp = data.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(ctype),
    );
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
        "inline; filename=\"{}\"; filename*=UTF-8''{}",
        a.name.replace('"', ""),
        encoded
    )) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

async fn delete_attachment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = state.db_for(&user.book_key)?;
    let a = findb::attach::get(&db, id)?
        .ok_or_else(|| AppError::NotFound("附件不存在".to_string()))?;
    let v = vouchers::get(&db, a.voucher_id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权删除该附件"));
    }
    findb::attach::delete(&db, id)?;
    db.log(
        user.username(),
        "凭证",
        "删除附件",
        &format!("{} 附件#{} {}", v.voucher_no(), id, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
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

fn invoice_from_req(r: &InvoiceReq) -> Result<findb::invoices::Invoice, AppError> {
    Ok(findb::invoices::Invoice {
        id: r.id,
        kind: if r.kind.is_empty() { "in".to_string() } else { r.kind.clone() },
        code: r.code.clone(),
        number: r.number.clone(),
        date: r.date.clone(),
        buyer: r.buyer.clone(),
        seller: r.seller.clone(),
        amount_tax: parse_money_checked(&r.amount_tax)?,
        amount: parse_money_checked(&r.amount)?,
        tax: parse_money_checked(&r.tax)?,
        tax_rate: r.tax_rate.clone(),
        status: if r.status.is_empty() { "pending".to_string() } else { r.status.clone() },
        memo: r.memo.clone(),
        attach_id: 0,
        created_by: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
    })
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
    let inv = invoice_from_req(&req)?;
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
    // 归属校验 + 保留服务端字段：普通用户不能改他人发票，
    // 且表单未提交的 attach_id/created_by/status 不能被覆盖清零
    let existing = findb::invoices::get(&db, id)?
        .ok_or_else(|| AppError::not_found("发票不存在"))?;
    if existing.created_by != user.username() && !user.user.is_admin() {
        return Err(AppError::forbidden("无权修改他人录入的发票"));
    }
    let mut inv = invoice_from_req(&req)?;
    inv.id = id;
    inv.attach_id = existing.attach_id;
    inv.created_by = existing.created_by.clone();
    inv.created_at = existing.created_at.clone();
    if inv.status != existing.status {
        // 状态变更走专用接口（带状态机校验），此处不接受表单覆盖
        inv.status = existing.status.clone();
    }
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
    let existing = findb::invoices::get(&db, id)?
        .ok_or_else(|| AppError::not_found("发票不存在"))?;
    if existing.created_by != user.username() && !user.user.is_admin() {
        return Err(AppError::forbidden("无权删除他人录入的发票"));
    }
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

/// 账簿查询参数（明细/总账/日记账共用）
fn ledger_query_from(
    state: &WebState,
    user: &CurrentUser,
    q: &HashMap<String, String>,
) -> Result<(findb::Db, fincore::Chart, LedgerQuery), AppError> {
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码参数 code"));
    }
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(state, user));
    let to = q
        .get("to")
        .and_then(|s| parse_period(s))
        .unwrap_or(from);
    let include_children = q
        .get("include_children")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    // H-3 定案：缺省即"仅已记账"，与余额表/报表口径一致
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let db = state.db_for(&user.book_key)?;
    let chart = accounts::chart(&db)?;
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
        prepared_by: None,
        code_from: None,
        code_to: None,
    }
    .with_user_scope(&user.user);
    Ok((db, chart, lq))
}

async fn get_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::balance::LedgerRow>>, AppError> {
    user.require(Perm::Report)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    let rows = balances::ledger(&db, &chart, &lq)?;
    Ok(Json(rows))
}

/// 总账（按期间汇总）
async fn get_general_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::GeneralLedgerRow>>, AppError> {
    user.require(Perm::Report)?;
    let (db, _chart, lq) = ledger_query_from(&state, &user, &q)?;
    Ok(Json(balances::general_ledger(&db, &lq)?))
}

/// 日记账（现金/银行）
async fn get_journal(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::JournalRow>>, AppError> {
    user.require(Perm::Report)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    Ok(Json(balances::journal(&db, &chart, &lq)?))
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
        .map(|v| v.clamp(1, 1000))
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
        prepared_by: None,
        code_from: None,
        code_to: None,
    }
    .with_user_scope(&user.user);
    let acct = chart
        .get(&code)
        .map(|a| format!("{} {}", code, a.name))
        .unwrap_or(code.clone());

    // 期初余额
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::range(from, from).with_user_scope(&user.user))?;
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
    let bq = BalanceQuery::range(from, to).with_user_scope(&user.user);
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
// 数据导出（CSV，需 Export 权限；带 BOM 便于 Excel 直接打开中文）
// ---------------------------------------------------------------------------

fn csv_response(filename: &str, rows: Vec<Vec<String>>) -> Response {
    fn esc(s: &str) -> String {
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    }
    let mut out = String::from("\u{feff}");
    for r in &rows {
        out.push_str(&r.iter().map(|c| esc(c)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    let mut resp = out.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
        "attachment; filename=\"{filename}\""
    )) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

async fn export_vouchers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = state.db_for(&user.book_key)?;
    let mut query = VoucherQuery::default().with_data_scope(&user.user);
    if let Some(p) = q.get("period").and_then(|s| parse_period(s)) {
        query.from = Some(p);
        query.to = Some(p);
    }
    if let Some(f) = q.get("from").and_then(|s| parse_period(s)) {
        query.from = Some(f);
    }
    if let Some(t) = q.get("to").and_then(|s| parse_period(s)) {
        query.to = Some(t);
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
    query.asc = true;
    query.limit = Some(5000);
    let mut list = vouchers::list(&db, &query)?;
    vouchers::fill_entries(&db, &mut list)?;
    let chart = accounts::chart(&db)?;
    let mut rows: Vec<Vec<String>> = vec![[
        "日期", "凭证号", "状态", "摘要", "科目编码", "科目名称", "借方", "贷方", "制单人",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()];
    let mut n = 0usize;
    for v in &list {
        if !user.user.can_see_voucher(v) {
            continue;
        }
        for e in &v.entries {
            if e.is_blank() {
                continue;
            }
            let name = chart
                .get(&e.account_code)
                .map(|a| a.name.clone())
                .unwrap_or_default();
            rows.push(vec![
                v.date.format("%Y-%m-%d").to_string(),
                v.voucher_no(),
                v.status.label().to_string(),
                e.summary.clone(),
                e.account_code.clone(),
                name,
                e.debit.fmt_plain(),
                e.credit.fmt_plain(),
                v.prepared_by.clone(),
            ]);
            n += 1;
        }
    }
    db.log(user.username(), "导出", "导出凭证", &format!("{n} 行"))?;
    Ok(csv_response("vouchers.csv", rows))
}

async fn export_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    let acct = chart
        .get(&lq.code)
        .map(|a| format!("{} {}", a.code, a.name))
        .unwrap_or_else(|| lq.code.clone());
    let list = balances::ledger(&db, &chart, &lq)?;
    let has_qty = list.iter().any(|r| r.qty_balance.is_some());
    let mut header: Vec<String> = ["日期", "凭证号", "摘要", "借方", "贷方", "方向", "余额"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if has_qty {
        header.push("数量余额".to_string());
    }
    let mut rows = vec![header];
    for r in &list {
        let mut row = vec![
            r.date.format("%Y-%m-%d").to_string(),
            r.voucher_no.clone(),
            r.summary.clone(),
            r.debit.fmt_plain(),
            r.credit.fmt_plain(),
            r.dir.label().to_string(),
            r.balance.fmt_plain(),
        ];
        if has_qty {
            row.push(r.qty_balance.map(|q| q.fmt_qty()).unwrap_or_default());
        }
        rows.push(row);
    }
    db.log(
        user.username(),
        "导出",
        "导出明细账",
        &format!("{acct} {} 行", rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("ledger.csv", rows))
}

async fn export_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = state.db_for(&user.book_key)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let list = business::payroll_list(&db, period)?;
    let mut rows = vec![[
        "员工", "部门", "应发", "社保(个人)", "公积金(个人)", "专项附加", "个税", "实发",
        "社保(单位)", "公积金(单位)",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>()];
    for p in &list {
        rows.push(vec![
            p.employee.clone(),
            p.dept.clone(),
            p.gross.fmt_plain(),
            p.social.fmt_plain(),
            p.housing.fmt_plain(),
            p.additional.fmt_plain(),
            p.tax.fmt_plain(),
            p.net.fmt_plain(),
            p.social_co.fmt_plain(),
            p.housing_co.fmt_plain(),
        ]);
    }
    db.log(
        user.username(),
        "导出",
        "导出工资表",
        &format!("{} {} 行", period.label(), rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("payroll.csv", rows))
}

async fn export_claims(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = state.db_for(&user.book_key)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    // 空串 = 全部状态：UI 下拉默认值为空，直接 parse 会落到 Draft 只显示草稿
    let status = q
        .get("status")
        .filter(|s| !s.is_empty())
        .map(|s| business::ClaimStatus::parse(s));
    let list = business::claim_list(&db, period, status)?;
    let mut rows = vec![[
        "单号", "业务日期", "申请人", "部门", "事由", "金额", "状态", "审批人", "付款人",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>()];
    for c in &list {
        rows.push(vec![
            c.no.clone(),
            c.biz_date.format("%Y-%m-%d").to_string(),
            c.applicant.clone(),
            c.dept.clone(),
            c.reason.clone(),
            c.amount.fmt_plain(),
            c.status.label().to_string(),
            c.approver.clone(),
            c.payer.clone(),
        ]);
    }
    db.log(
        user.username(),
        "导出",
        "导出报销单",
        &format!("{} {} 行", period.label(), rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("claims.csv", rows))
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
    let rows = advanced::multi_column_table(&db, &main, &cols, from, to, Some(&user.user))?;
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
    // H-3：科目日报默认只统计已记账，与账簿「只含已记账」开关同参
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let rows =
        findb::reports::account_daily_report(&db, &code, from, to, Some(&user.user), posted_only)?;
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

/// 辅助账：按辅助核算维度汇总各单位的期初/发生/期末
async fn get_aux_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let kind_code = q.get("kind").map(String::as_str).unwrap_or("customer");
    let kind = fincore::AuxKind::from_code(kind_code)
        .ok_or_else(|| AppError::bad_request("非法辅助核算维度 kind"))?;
    if kind == fincore::AuxKind::CashFlow {
        return Err(AppError::bad_request("现金流量项目不是余额维度"));
    }
    let (from, to) = report_range(&state, &user, &q);
    let db = state.db_for(&user.book_key)?;
    let rows = balances::aux_balance(&db, kind, from, to, Some(&user.user))?;
    Ok(Json(json!({
        "kind": kind.code(),
        "kind_label": kind.label(),
        "from": period_to_str(from),
        "to": period_to_str(to),
        "rows": rows,
    })))
}

/// 数量金额账：数量核算科目的数量与金额对照
async fn get_qty_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let (from, to) = report_range(&state, &user, &q);
    let db = state.db_for(&user.book_key)?;
    let rows = balances::qty_balance_sheet(&db, from, to, Some(&user.user))?;
    Ok(Json(json!({
        "from": period_to_str(from),
        "to": period_to_str(to),
        "rows": rows,
    })))
}

// ---------------------------------------------------------------------------
// 自定义报表（UFO 公式）
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct CustomLineReq {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub indent: u8,
    #[serde(default)]
    pub formulas: Vec<String>,
    #[serde(default)]
    pub bold: bool,
}

#[derive(Deserialize, Default)]
struct CustomReportReq {
    #[serde(default)]
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub lines: Vec<CustomLineReq>,
}

fn custom_json(r: &findb::mgmt::CustomReport) -> serde_json::Value {
    json!({
        "key": r.key,
        "name": r.name,
        "columns": r.columns,
        "lines": r.lines.iter().map(|l| json!({
            "name": l.name, "indent": l.indent, "formulas": l.formulas, "bold": l.bold,
        })).collect::<Vec<_>>(),
    })
}

async fn list_custom_reports(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let rows: Vec<serde_json::Value> = findb::mgmt::custom_list(&db)?
        .iter()
        .map(custom_json)
        .collect();
    Ok(Json(rows))
}

async fn get_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let r = findb::mgmt::custom_get(&db, &key)?
        .ok_or_else(|| AppError::not_found("自定义报表不存在"))?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let values = findb::mgmt::custom_report_values(&db, &r, period)?;
    let matrix: Vec<Vec<String>> = values
        .iter()
        .map(|row| row.iter().map(|m| m.fmt_money()).collect())
        .collect();
    Ok(Json(json!({
        "report": custom_json(&r),
        "period": period_to_str(period),
        "values": matrix,
    })))
}

async fn save_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CustomReportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    if req.name.trim().is_empty() {
        return Err(AppError::bad_request("报表名称不能为空"));
    }
    let key = if req.key.trim().is_empty() {
        findb::mgmt::custom_next_key(&db)?
    } else {
        req.key.trim().to_string()
    };
    let r = findb::mgmt::CustomReport {
        key: key.clone(),
        name: req.name.trim().to_string(),
        columns: req.columns.iter().map(|c| c.trim().to_string()).collect(),
        lines: req
            .lines
            .iter()
            .map(|l| findb::mgmt::CustomLine {
                name: l.name.trim().to_string(),
                indent: l.indent,
                formulas: l.formulas.clone(),
                bold: l.bold,
            })
            .collect(),
    };
    let errors: Vec<serde_json::Value> = findb::mgmt::custom_check(&r)
        .into_iter()
        .map(|(li, ci, e)| json!({ "line": li, "column": ci, "error": e }))
        .collect();
    findb::mgmt::custom_save(&db, &r)?;
    db.log(
        user.username(),
        "报表",
        "保存自定义报表",
        &format!("{} {}（{} 行）", key, r.name, r.lines.len()),
    )?;
    Ok(Json(json!({ "key": key, "errors": errors })))
}

async fn delete_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    findb::mgmt::custom_delete(&db, &key)?;
    db.log(user.username(), "报表", "删除自定义报表", &key)?;
    Ok(Json(json!({ "ok": true })))
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
    user.require(Perm::Warehouse)?;
    let db = state.db_for(&user.book_key)?;
    if req.item.trim().is_empty() {
        return Err(AppError::bad_request("缺少存货 item"));
    }
    let delta = parse_money_checked(&req.delta)?;
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
    user.require(Perm::Warehouse)?;
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
    user.require(Perm::Warehouse)?;
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
    user.require(Perm::Warehouse)?;
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
    user.require(Perm::Warehouse)?;
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
    user.require(Perm::Warehouse)?;
    let db = state.db_for(&user.book_key)?;
    findb::inventory2::unit_set(&db, &findb::inventory2::ItemUnit {
        item: req.item,
        base_unit: req.base_unit,
        alt_unit: req.alt_unit,
        factor: parse_money_checked(&req.factor)?,
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
    user.require(Perm::Warehouse)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let mut children: Vec<(String, Money)> = Vec::with_capacity(req.children.len());
    for (i, q) in req.children {
        children.push((i, parse_money_checked(&q)?));
    }
    findb::inventory2::assemble(&db, period, date, &req.parent, &children, &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn disassemble_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssembleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = state.db_for(&user.book_key)?;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let mut children: Vec<(String, Money)> = Vec::with_capacity(req.children.len());
    for (i, q) in req.children {
        children.push((i, parse_money_checked(&q)?));
    }
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
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    // 登记暂估即同事务生成暂估凭证（借 存货 / 贷 应付-订单供应商）
    let (est_id, vid) = findb::scm2::po_estimate_add(&db, req.po_id, period, &req.item, parse_money_checked(&req.est_amount)?, user.username())?;
    db.log(
        user.username(),
        "采购",
        "登记暂估",
        &format!("PO#{} {} {} 凭证 #{vid}", req.po_id, req.item, req.est_amount),
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "id": est_id, "voucher_id": vid })))
}

async fn list_estimates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
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
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let today = chrono::Local::now().date_naive();
    let date = if body.is_empty() {
        today
    } else {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        match v.get("date").and_then(|d| d.as_str()) {
            Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            None => today,
        }
    };
    // 冲回同事务生成反向凭证（借 应付 / 贷 存货）
    let vid = findb::scm2::po_estimate_settle(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "采购",
        "暂估冲回",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，冲回凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "voucher_id": vid })))
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
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    findb::scm2::quota_set(&db, period, &req.supplier, &req.item, parse_money_checked(&req.quota_qty)?)?;
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
    user.require(Perm::OrderOps)?;
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
    user.require(Perm::OrderOps)?;
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
    user.require(Perm::OrderOps)?;
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
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::procurement::po_receipt_add(&db, &findb::procurement::PoReceipt {
        id: 0, po_id: req.po_id, period, date, qty: parse_money_checked(&req.qty)?, memo: req.memo,
    })?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn add_po_return(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::procurement::po_return_add(&db, req.po_id, period, date, parse_money_checked(&req.qty)?, &req.memo)?;
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
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let amount = parse_money_checked(&req.amount)?;
    // 对标金蝶：付款动作即出台账凭证并按供应商自动核销（默认资金账户取账套配置）
    let po = findb::scm::po_get(&db, req.po_id)?
        .ok_or_else(|| AppError::not_found("采购订单不存在"))?;
    let (_doc, vid, settled) = findb::receipt::receipt_create(
        &db,
        "payment",
        date,
        "",
        &po.supplier_code,
        amount,
        &req.memo,
        user.username(),
    )?;
    let id = findb::procurement::po_payment_add(
        &db,
        &findb::procurement::PoPayment {
            id: 0,
            po_id: req.po_id,
            period,
            date,
            amount,
            memo: req.memo,
        },
    )?;
    db.log(
        user.username(),
        "采购",
        "采购付款",
        &format!(
            "#{} {} 凭证 #{vid}（自动核销 {settled} 笔）",
            req.po_id,
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "voucher_id": vid, "settled": settled })))
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
    user.require(Perm::OrderOps)?;
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
    user.require(Perm::OrderOps)?;
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
    user.require(Perm::OrderOps)?;
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
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let qty = parse_money_checked(&req.qty)?;
    if qty.is_negative() || qty.is_zero() {
        return Err(AppError::bad_request("发货数量必须为正数"));
    }
    // 对标金蝶：未确认订单不能出库（草稿/作废订单先确认）
    let so = findb::scm::so_get(&db, req.so_id)?
        .ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    if matches!(
        so.status,
        findb::scm::SoStatus::Draft | findb::scm::SoStatus::Cancelled
    ) {
        return Err(AppError::bad_request("订单未确认，不能发货（请先「确认」订单）"));
    }
    // 确认收入与应收（比例法；先出凭证再落发货流水，金额为零时无凭证）
    let ivid = findb::sales::so_income_voucher(&db, req.so_id, qty, date, user.username())?;
    let id = findb::sales::so_shipment_add(&db, req.so_id, period, date, qty, &req.memo)?;
    findb::scm::so_progress_update(&db, req.so_id)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id, "voucher_id": ivid })))
}

async fn add_so_return(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ShipmentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let qty = parse_money_checked(&req.qty)?;
    if qty.is_negative() || qty.is_zero() {
        return Err(AppError::bad_request("退货数量必须为正数"));
    }
    // 对标金蝶：未确认订单不能退货
    let so = findb::scm::so_get(&db, req.so_id)?
        .ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    if matches!(
        so.status,
        findb::scm::SoStatus::Draft | findb::scm::SoStatus::Cancelled
    ) {
        return Err(AppError::bad_request("订单未确认，不能退货（请先「确认」订单）"));
    }
    // 冲回收入与应收（负向比例；封顶已发货量）
    let ivid = findb::sales::so_income_voucher(&db, req.so_id, qty.negated(), date, user.username())?;
    let id = findb::sales::so_return_add(&db, req.so_id, period, date, qty, &req.memo)?;
    findb::scm::so_progress_update(&db, req.so_id)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id, "voucher_id": ivid })))
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
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 { period_checked(req.period)? } else { current_period(&state, &user) };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let amount = parse_money_checked(&req.amount)?;
    // 对标金蝶：收款动作即出台账凭证并按客户自动核销（默认资金账户取账套配置）
    let so = findb::scm::so_get(&db, req.so_id)?
        .ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    let (_doc, vid, settled) = findb::receipt::receipt_create(
        &db,
        "receipt",
        date,
        "",
        &so.customer_code,
        amount,
        &req.memo,
        user.username(),
    )?;
    let id =
        findb::sales::so_payment_add(&db, req.so_id, period, date, amount, &req.memo)?;
    db.log(
        user.username(),
        "销售",
        "销售收款",
        &format!(
            "#{} {} 凭证 #{vid}（自动核销 {settled} 笔）",
            req.so_id,
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "voucher_id": vid, "settled": settled })))
}

// ---------------- 单据套打（订单 / 收付款单）：字段白名单 + 批量紧凑分页 ----------------

/// 从查询串取打印字段：`fields=no,date,...` 白名单（空=全显）；`pack=0` 一单一页
fn doc_fields_from_q(q: &HashMap<String, String>) -> findb::printform::DocPrintFields {
    let mut f = findb::printform::fields_from_tokens(
        q.get("fields").map(|s| s.as_str()).unwrap_or(""),
    );
    if q.get("pack").map(|s| s.as_str()) == Some("0") {
        f.pack = false;
    }
    f
}

/// 打印纸张：`a4` / `a5` / `third` / 自定义 `宽x高`（mm，50..=600），非法回退 a4
fn doc_size_from_q(q: &HashMap<String, String>) -> String {
    let s = q
        .get("size")
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if s == "a4" || s == "a5" || s == "third" {
        return s;
    }
    if let Some((w, h)) = s
        .split_once('x')
        .and_then(|(a, b)| Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?)))
    {
        if (50..=600).contains(&w) && (50..=600).contains(&h) {
            return format!("{w}x{h}");
        }
    }
    "a4".to_string()
}

/// `ids=1,2,3`（去重保序）；缺省返回空 = 由调用方按期间/全部取数
fn print_ids(q: &HashMap<String, String>) -> Vec<i64> {
    let mut ids: Vec<i64> = q
        .get("ids")
        .map(|s| s.split(',').filter_map(|x| x.trim().parse::<i64>().ok()).collect())
        .unwrap_or_default();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn so_to_print(o: findb::scm::SalesOrder) -> findb::printform::OrderPrint {
    findb::printform::OrderPrint {
        title: "销售订单".to_string(),
        no: o.no,
        date: o.date.format("%Y-%m-%d").to_string(),
        status: o.status.label().to_string(),
        party_label: "客户".to_string(),
        party: format!("{} {}", o.customer_code, o.customer_name),
        prepared_by: o.prepared_by,
        memo: o.memo,
        rows: o
            .lines
            .into_iter()
            .map(|l| findb::printform::OrderPrintRow {
                code: l.item_code,
                name: l.item_name,
                qty: l.qty_ordered,
                price: l.unit_price,
                rate: l.tax_rate,
                amount: l.amount,
                tax: l.tax_amount,
                memo: l.memo,
            })
            .collect(),
        amount_total: o.total_amount,
        tax_total: o.total_tax,
    }
}

fn po_to_print(o: findb::scm::PurchaseOrder) -> findb::printform::OrderPrint {
    findb::printform::OrderPrint {
        title: "采购订单".to_string(),
        no: o.no,
        date: o.date.format("%Y-%m-%d").to_string(),
        status: o.status.label().to_string(),
        party_label: "供应商".to_string(),
        party: format!("{} {}", o.supplier_code, o.supplier_name),
        prepared_by: o.prepared_by,
        memo: o.memo,
        rows: o
            .lines
            .into_iter()
            .map(|l| findb::printform::OrderPrintRow {
                code: l.item_code,
                name: l.item_name,
                qty: l.qty_ordered,
                price: l.unit_price,
                rate: l.tax_rate,
                amount: l.amount,
                tax: l.tax_amount,
                memo: l.memo,
            })
            .collect(),
        amount_total: o.total_amount,
        tax_total: o.total_tax,
    }
}

fn receipt_to_print(d: findb::receipt::ReceiptDoc) -> findb::printform::ReceiptPrint {
    findb::printform::ReceiptPrint {
        no: d.no,
        date: d.date.format("%Y-%m-%d").to_string(),
        kind_label: if d.kind == "receipt" { "收款" } else { "付款" }.to_string(),
        fund: d.fund_account,
        party: d.party,
        amount: d.amount,
        memo: d.memo,
        voucher_no: d.voucher_id.map(|v| format!("记-{v:04}")).unwrap_or_default(),
    }
}

/// 销售订单套打 HTML：`ids` 逗号分隔（缺省 = 期间内全部）；`fields` 选字段；`pack=0` 一单一页
async fn print_so_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let company = user.company.clone();
    let ids = print_ids(&q);
    let mut orders = Vec::new();
    if ids.is_empty() {
        let period = q
            .get("period")
            .and_then(|s| parse_period(s))
            .unwrap_or_else(|| current_period(&state, &user));
        for o in findb::scm::so_list(&db, period, None)? {
            orders.push(so_to_print(o));
        }
    } else {
        for id in ids {
            let o = findb::scm::so_get(&db, id)?
                .ok_or_else(|| AppError::bad_request(format!("销售订单 #{id} 不存在")))?;
            orders.push(so_to_print(o));
        }
    }
    if orders.is_empty() {
        return Err(AppError::bad_request("没有可打印的订单"));
    }
    let f = doc_fields_from_q(&q);
    let html = findb::printform::order_forms_sized_html(&company, &orders, &f, &doc_size_from_q(&q));
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 采购订单套打 HTML（同销售订单：ids / fields / pack）
async fn print_po_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let company = user.company.clone();
    let ids = print_ids(&q);
    let mut orders = Vec::new();
    if ids.is_empty() {
        let period = q
            .get("period")
            .and_then(|s| parse_period(s))
            .unwrap_or_else(|| current_period(&state, &user));
        for o in findb::scm::po_list(&db, period, None)? {
            orders.push(po_to_print(o));
        }
    } else {
        for id in ids {
            let o = findb::scm::po_get(&db, id)?
                .ok_or_else(|| AppError::bad_request(format!("采购订单 #{id} 不存在")))?;
            orders.push(po_to_print(o));
        }
    }
    if orders.is_empty() {
        return Err(AppError::bad_request("没有可打印的订单"));
    }
    let f = doc_fields_from_q(&q);
    let html = findb::printform::order_forms_sized_html(&company, &orders, &f, &doc_size_from_q(&q));
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 收付款单套打 HTML：`ids` 缺省 = 全部（可配 `period` 过滤）
async fn print_receipt_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let company = user.company.clone();
    let ids = print_ids(&q);
    let period = q.get("period").and_then(|s| parse_period(s));
    let all = findb::receipt::receipt_list(&db)?;
    let mut prints = Vec::new();
    for d in all.into_iter() {
        if !ids.is_empty() {
            if ids.contains(&d.id) {
                prints.push(receipt_to_print(d));
            }
        } else if period.map(|p| d.period == p).unwrap_or(true) {
            prints.push(receipt_to_print(d));
        }
    }
    if prints.is_empty() {
        return Err(AppError::bad_request("没有可打印的收付款单"));
    }
    let f = doc_fields_from_q(&q);
    let html = findb::printform::receipt_forms_sized_html(&company, &prints, &f, &doc_size_from_q(&q));
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 报价单转销售订单（approved → converted）：生成草稿订单，税率 0 可在订单中再调整
async fn convert_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let so_id = findb::sales::quo_to_order(&db, id, user.username())?;
    db.log(
        user.username(),
        "销售",
        "报价转订单",
        &format!("报价 #{id} → 销售订单 #{so_id}"),
    )?;
    Ok(Json(json!({ "ok": true, "so_id": so_id })))
}

// ---------------- 订单 CRUD（销售/采购，对标金蝶订单流程） ----------------

#[derive(Deserialize)]
struct OrderLineInput {
    #[serde(default)]
    item_code: String,
    #[serde(default)]
    item_name: String,
    #[serde(default)]
    qty_ordered: String,
    #[serde(default)]
    unit_price: String,
    #[serde(default)]
    tax_rate: String,
    #[serde(default)]
    qty_shipped: String,
    #[serde(default)]
    qty_received: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
struct SoInput {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    customer_code: String,
    #[serde(default)]
    customer_name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<OrderLineInput>,
}

#[derive(Deserialize)]
struct PoInput {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    supplier_code: String,
    #[serde(default)]
    supplier_name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<OrderLineInput>,
}

#[derive(Deserialize)]
struct OrderTransitionReq {
    status: String,
}

/// 订单日期：缺省今天（非法值回退今天，不让单据失败）
fn order_date(s: &str) -> NaiveDate {
    if s.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| chrono::Local::now().date_naive())
    }
}

fn so_status_parse(s: &str) -> Result<findb::scm::SoStatus, AppError> {
    findb::scm::status_from::<findb::scm::SoStatus>(s.trim()).ok_or_else(|| {
        AppError::bad_request("订单状态取值：Draft/Confirmed/PartialShip/Completed/Cancelled")
    })
}

fn po_status_parse(s: &str) -> Result<findb::scm::PoStatus, AppError> {
    findb::scm::status_from::<findb::scm::PoStatus>(s.trim()).ok_or_else(|| {
        AppError::bad_request("订单状态取值：Draft/Confirmed/PartialIn/Completed/Cancelled")
    })
}

async fn list_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::scm::so_list(&db, period, None)?;
    Ok(Json(json!({ "rows": rows })))
}

/// 保存销售订单：服务端计算行金额（数量×单价）与税额（金额×税率）；
/// 非草稿状态保存时 so_save 内做客户信用检查（对标金蝶卡控）。
async fn save_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SoInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let code = req.customer_code.trim();
    if code.is_empty() {
        return Err(AppError::bad_request("客户编码必填"));
    }
    let name = if req.customer_name.trim().is_empty() {
        code.to_string()
    } else {
        req.customer_name.trim().to_string()
    };
    let mut so = findb::scm::SalesOrder::new(period, order_date(&req.date), code, &name, user.username());
    so.id = req.id;
    so.memo = req.memo;
    so.status = so_status_parse(&req.status)?;
    if so.id > 0 {
        let old = findb::scm::so_get(&db, so.id)?
            .ok_or_else(|| AppError::not_found("销售订单不存在"))?;
        so.no = old.no;
        so.shipped_amount = old.shipped_amount;
    } else {
        so.no = findb::scm::so_next_no(&db, period)?;
    }
    for l in &req.lines {
        if l.item_code.trim().is_empty() {
            continue;
        }
        let qty = parse_money_checked(&l.qty_ordered)?;
        let price = parse_money_checked(&l.unit_price)?;
        let rate = if l.tax_rate.trim().is_empty() {
            Money::ZERO
        } else {
            parse_money_checked(&l.tax_rate)?
        };
        let amount = (qty * price).round2();
        let tax = (amount * rate).round2();
        so.lines.push(findb::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: l.item_code.trim().to_string(),
            item_name: if l.item_name.trim().is_empty() {
                l.item_code.trim().to_string()
            } else {
                l.item_name.trim().to_string()
            },
            qty_ordered: qty,
            qty_shipped: if l.qty_shipped.trim().is_empty() {
                Money::ZERO
            } else {
                parse_money_checked(&l.qty_shipped)?
            },
            unit_price: price,
            tax_rate: rate,
            amount,
            tax_amount: tax,
            memo: l.memo.clone(),
        });
    }
    if so.lines.is_empty() {
        return Err(AppError::bad_request("订单至少一行明细"));
    }
    let id = findb::scm::so_save(&db, &mut so)?;
    db.log(
        user.username(),
        "销售",
        "保存销售订单",
        &format!("#{} {} {} 不含税 {}", id, so.no, code, so.total_amount.fmt_money()),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "total_amount": so.total_amount,
        "total_tax": so.total_tax
    })))
}

async fn transition_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<OrderTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let to = so_status_parse(&req.status)?;
    findb::scm::so_set_status(&db, id, to)?;
    db.log(user.username(), "销售", "销售订单状态", &format!("#{} → {}", id, to.label()))?;
    Ok(Json(json!({ "ok": true, "status": to.label() })))
}

async fn delete_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    findb::scm::so_delete(&db, id)?;
    db.log(user.username(), "销售", "删除销售订单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::scm::po_list(&db, period, None)?;
    Ok(Json(json!({ "rows": rows })))
}

/// 保存采购订单（行金额/税额服务端计算，同销售订单）
async fn save_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PoInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let code = req.supplier_code.trim();
    if code.is_empty() {
        return Err(AppError::bad_request("供应商编码必填"));
    }
    let name = if req.supplier_name.trim().is_empty() {
        code.to_string()
    } else {
        req.supplier_name.trim().to_string()
    };
    let mut po = findb::scm::PurchaseOrder::new(period, order_date(&req.date), code, &name, user.username());
    po.id = req.id;
    po.memo = req.memo;
    po.status = po_status_parse(&req.status)?;
    if po.id > 0 {
        let old = findb::scm::po_get(&db, po.id)?
            .ok_or_else(|| AppError::not_found("采购订单不存在"))?;
        po.no = old.no;
        po.received_amount = old.received_amount;
    } else {
        po.no = findb::scm::po_next_no(&db, period)?;
    }
    for l in &req.lines {
        if l.item_code.trim().is_empty() {
            continue;
        }
        let qty = parse_money_checked(&l.qty_ordered)?;
        let price = parse_money_checked(&l.unit_price)?;
        let rate = if l.tax_rate.trim().is_empty() {
            Money::ZERO
        } else {
            parse_money_checked(&l.tax_rate)?
        };
        let amount = (qty * price).round2();
        let tax = (amount * rate).round2();
        po.lines.push(findb::scm::PoLine {
            id: 0,
            po_id: 0,
            item_code: l.item_code.trim().to_string(),
            item_name: if l.item_name.trim().is_empty() {
                l.item_code.trim().to_string()
            } else {
                l.item_name.trim().to_string()
            },
            qty_ordered: qty,
            qty_received: if l.qty_received.trim().is_empty() {
                Money::ZERO
            } else {
                parse_money_checked(&l.qty_received)?
            },
            unit_price: price,
            tax_rate: rate,
            amount,
            tax_amount: tax,
            memo: l.memo.clone(),
        });
    }
    if po.lines.is_empty() {
        return Err(AppError::bad_request("订单至少一行明细"));
    }
    let id = findb::scm::po_save(&db, &mut po)?;
    db.log(
        user.username(),
        "采购",
        "保存采购订单",
        &format!("#{} {} {} 不含税 {}", id, po.no, code, po.total_amount.fmt_money()),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "total_amount": po.total_amount,
        "total_tax": po.total_tax
    })))
}

async fn transition_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<OrderTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    let to = po_status_parse(&req.status)?;
    findb::scm::po_set_status(&db, id, to)?;
    db.log(user.username(), "采购", "采购订单状态", &format!("#{} → {}", id, to.label()))?;
    Ok(Json(json!({ "ok": true, "status": to.label() })))
}

async fn delete_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = state.db_for(&user.book_key)?;
    findb::scm::po_delete(&db, id)?;
    db.log(user.username(), "采购", "删除采购订单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
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
    let mut ops: Vec<advanced::RoutingOp> = Vec::with_capacity(req.len());
    for d in req {
        ops.push(advanced::RoutingOp {
            id: 0,
            item_code: item.clone(),
            version: String::new(),
            seq: d.seq,
            op_code: d.op_code,
            op_name: d.op_name,
            work_center: d.work_center,
            std_hours: parse_money_checked(&d.std_hours)?,
            rate: parse_money_checked(&d.rate)?,
        });
    }
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
    advanced::prod_op_report(&db, req.op_id, parse_money_checked(&req.qty)?, parse_money_checked(&req.hours)?)?;
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
    let mut demands: Vec<(String, Money, String)> = Vec::with_capacity(req.demands.len());
    for d in req.demands {
        demands.push((d.item_code, parse_money_checked(&d.qty)?, d.source));
    }
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
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
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
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let rows = findb::funds::bill_list(&db, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_bill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BillReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
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
        amount: parse_money_checked(&req.amount)?,
        status: if req.status.is_empty() {
            "in_hand".to_string()
        } else {
            req.status
        },
        handled_date: None,
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
        voucher_id: None,
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
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let date = chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    let to = findb::funds::BillStatus::parse(&req.status);
    // 资金动作（背书/贴现/兑付）在 findb 同事务自动生成台账凭证草稿
    let vid = findb::funds::bill_transition(&db, id, to, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "票据流转",
        &format!(
            "#{} → {}{}",
            id,
            to.label(),
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

async fn delete_bill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
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
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let rows = findb::funds::loan_list(&db, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_loan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<LoanReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
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
        principal: parse_money_checked(&req.principal)?,
        rate_pct: parse_money_checked(&req.rate_pct)?,
        start_date: parse_date(&req.start_date)?,
        end_date: parse_date(&req.end_date)?,
        status: if req.status.is_empty() {
            "active".to_string()
        } else {
            req.status
        },
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
        voucher_id: None,
        settle_voucher_id: None,
        settle_date: None,
    };
    let id = findb::funds::loan_save(&db, &mut l)?;
    db.log(user.username(), "资金", "保存融资", &format!("#{id} {}", l.no))?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct LoanSettleReq {
    #[serde(default)]
    date: Option<String>,
}

/// 结清融资（body 可选指定结清日期；findb 同事务自动生成还本凭证）
async fn loan_settle(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let today = chrono::Local::now().date_naive();
    let date = if body.is_empty() {
        today
    } else {
        let req: LoanSettleReq = serde_json::from_slice(&body)
            .map_err(|_| AppError::bad_request("请求体应为 {\"date\":\"YYYY-MM-DD\"}"))?;
        match req.date {
            Some(s) => chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            None => today,
        }
    };
    let vid = findb::funds::loan_settle(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "结清融资",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 票据补出台账凭证（流转已自动生成；本端点用于存量台账回填）
async fn bill_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let vid = findb::funds::bill_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "票据生成凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
}

/// 融资台账凭证（存续=到账凭证；已结清=还本凭证回填）
async fn loan_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let vid = findb::funds::loan_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "融资生成凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
}

#[derive(Deserialize)]
struct CashCountReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    date: String,
    #[serde(default)]
    account_code: String,
    #[serde(default)]
    counted: String,
    #[serde(default)]
    memo: String,
}

async fn list_cash_counts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    Ok(Json(serde_json::json!({ "rows": findb::funds::cash_count_list(&db)? })))
}

/// 新增/修改现金盘点：账面余额与差异由服务端按资金日报（按日）口径重算
async fn save_cash_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CashCountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let account = if req.account_code.trim().is_empty() {
        "1001".to_string()
    } else {
        req.account_code.trim().to_string()
    };
    let mut c = findb::funds::CashCount {
        id: req.id,
        period: fincore::Period::from_date(date),
        date,
        account_code: account,
        book_amount: Money::ZERO,
        counted: parse_money_checked(&req.counted)?,
        diff: Money::ZERO,
        memo: req.memo,
        voucher_id: None,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::cash_count_save(&db, &mut c)?;
    db.log(
        user.username(),
        "资金",
        "现金盘点",
        &format!(
            "{} {} 实盘 {}（账面 {} 差异 {}）",
            c.account_code,
            c.date.format("%Y-%m-%d"),
            c.counted.fmt_money(),
            c.book_amount.fmt_money(),
            c.diff.fmt_money()
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "book_amount": c.book_amount,
        "diff": c.diff
    })))
}

async fn delete_cash_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    findb::funds::cash_count_delete(&db, id)?;
    db.log(user.username(), "资金", "删除盘点记录", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 盘盈盘亏差异生成凭证
async fn cash_count_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let vid = findb::funds::cash_count_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "盘点差异出凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
}

#[derive(Deserialize)]
struct DayClearReq {
    #[serde(default)]
    account_code: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    clear: bool,
}

/// 日清日期查询：?account=&from=YYYY-MM-DD&to=YYYY-MM-DD（缺省 today~today）
async fn list_day_clear(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let account = q
        .get("account")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1001".to_string());
    let parse = |k: &str| -> Result<Option<chrono::NaiveDate>, AppError> {
        match q.get(k) {
            Some(s) => Ok(Some(
                chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            )),
            None => Ok(None),
        }
    };
    let today = chrono::Local::now().date_naive();
    let from = parse("from")?.unwrap_or(today);
    let to = parse("to")?.unwrap_or(today);
    let dates = findb::funds::day_clear_dates(&db, &account, from, to)?;
    Ok(Json(json!({ "dates": dates })))
}

/// 日清标记 / 取消
async fn set_day_clear(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<DayClearReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let account = if req.account_code.trim().is_empty() {
        "1001".to_string()
    } else {
        req.account_code.trim().to_string()
    };
    let date = chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    findb::funds::day_clear_set(&db, &account, date, req.clear, user.username())?;
    db.log(
        user.username(),
        "资金",
        if req.clear { "日记账日清" } else { "取消日清" },
        &format!("{} {}", account, date.format("%Y-%m-%d")),
    )?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct CheckReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    no: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    bank_account: String,
    #[serde(default)]
    payee: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    issued_date: String,
    #[serde(default)]
    memo: String,
}

async fn list_checks(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    Ok(Json(json!({ "rows": findb::funds::check_list(&db)? })))
}

/// 支票登记簿新增/修改（备查簿，不入账）
async fn save_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CheckReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    if req.no.trim().is_empty() {
        return Err(AppError::bad_request("支票号必填"));
    }
    let issued_date = if req.issued_date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.issued_date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let kind = if req.kind == "cash" { "cash" } else { "transfer" };
    let mut c = findb::funds::CheckRow {
        id: req.id,
        no: req.no.trim().to_string(),
        kind: kind.to_string(),
        bank_account: req.bank_account,
        payee: req.payee,
        amount: parse_money_checked(&req.amount)?,
        issued_date,
        status: "issued".to_string(),
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::check_save(&db, &mut c)?;
    db.log(user.username(), "资金", "保存支票", &format!("#{id} {}", c.no))?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn check_status(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let status = req["status"].as_str().unwrap_or("");
    findb::funds::check_set_status(&db, id, status)?;
    db.log(
        user.username(),
        "资金",
        "支票状态",
        &format!("#{id} → {status}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    findb::funds::check_delete(&db, id)?;
    db.log(user.username(), "资金", "删除支票", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AdvanceReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    no: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    employee: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    pay_account: String,
    #[serde(default)]
    expense_account: String,
    #[serde(default)]
    memo: String,
}

async fn list_advances(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    Ok(Json(json!({ "rows": findb::funds::advance_list(&db)? })))
}

/// 新增/修改借支单（建单即 approved；支付/核销走独立端点）
async fn save_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AdvanceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let no = if req.no.trim().is_empty() {
        format!("JZ{}", chrono::Local::now().format("%Y%m%d%H%M%S"))
    } else {
        req.no.trim().to_string()
    };
    let mut a = findb::funds::Advance {
        id: req.id,
        no,
        period: fincore::Period::from_date(date),
        date,
        employee: req.employee,
        purpose: req.purpose,
        amount: parse_money_checked(&req.amount)?,
        pay_account: if req.pay_account.trim().is_empty() {
            "1001".to_string()
        } else {
            req.pay_account.trim().to_string()
        },
        status: "approved".to_string(),
        paid_date: None,
        paid_voucher_id: None,
        settle_date: None,
        settle_voucher_id: None,
        expense_account: if req.expense_account.trim().is_empty() {
            "660201".to_string()
        } else {
            req.expense_account.trim().to_string()
        },
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::advance_save(&db, &mut a)?;
    db.log(
        user.username(),
        "资金",
        "借支建单",
        &format!("#{} {} {} {}", id, a.no, a.employee, a.amount.fmt_money()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct AdvanceSettleReq {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    expense_account: String,
    #[serde(default)]
    expense_amount: String,
}

fn opt_body_date(body: &axum::body::Bytes) -> Result<Option<chrono::NaiveDate>, AppError> {
    if body.is_empty() {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct D {
        #[serde(default)]
        date: Option<String>,
    }
    let d: D = serde_json::from_slice(body)
        .map_err(|_| AppError::bad_request("请求体日期格式应为 {\"date\":\"YYYY-MM-DD\"}"))?;
    match d.date {
        Some(s) => Ok(Some(
            chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
        )),
        None => Ok(None),
    }
}

/// 支付借支（body 可选 {date}，默认今天）；同事务生成支付凭证
async fn pay_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let date = opt_body_date(&body)?.unwrap_or_else(|| chrono::Local::now().date_naive());
    let vid = findb::funds::advance_pay(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "借支支付",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 核销借支（body: {expense_account, expense_amount, date?}）；同事务生成核销凭证
async fn settle_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    if body.is_empty() {
        return Err(AppError::bad_request(
            "请求体应为 {\"expense_account\":\"660201\",\"expense_amount\":\"1500\",\"date\":\"YYYY-MM-DD\"?}",
        ));
    }
    let req: AdvanceSettleReq = serde_json::from_slice(&body)
        .map_err(|_| AppError::bad_request("请求体格式错误（见接口约定）"))?;
    let date = match req.date {
        Some(s) => chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
        None => chrono::Local::now().date_naive(),
    };
    let expense_account = if req.expense_account.trim().is_empty() {
        "660201".to_string()
    } else {
        req.expense_account.trim().to_string()
    };
    let expense = parse_money_checked(&req.expense_amount)?;
    let vid = findb::funds::advance_settle(
        &db,
        id,
        &expense_account,
        expense,
        date,
        user.username(),
    )?;
    db.log(
        user.username(),
        "资金",
        "借支核销",
        &format!(
            "#{id} 冲账 {expense}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

async fn delete_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    findb::funds::advance_delete(&db, id)?;
    db.log(user.username(), "资金", "删除借支单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ReceiptCreateReq {
    #[serde(default)]
    date: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    fund_account: String,
    #[serde(default)]
    party: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    memo: String,
}

async fn list_receipts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    Ok(Json(json!({ "rows": findb::receipt::receipt_list(&db)? })))
}

/// 新增收付款单：同事务生成资金凭证 + FIFO 自动核销（对标金蝶收款单/付款单）
async fn create_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptCreateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let kind = if req.kind == "payment" { "payment" } else { "receipt" };
    let amount = parse_money_checked(&req.amount)?;
    let (id, vid, settled) = findb::receipt::receipt_create(
        &db,
        kind,
        date,
        &req.fund_account,
        &req.party,
        amount,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "资金",
        "新增收付款单",
        &format!(
            "#{id} {} {} 凭证 #{vid}（自动核销 {settled} 笔）",
            if kind == "receipt" { "收款" } else { "付款" },
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "voucher_id": vid, "settled": settled })))
}

async fn delete_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    findb::receipt::receipt_delete(&db, id)?;
    db.log(user.username(), "资金", "删除收付款单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_loan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    findb::funds::loan_delete(&db, id)?;
    Ok(Json(json!({ "ok": true })))
}

/// 资金预算 vs 执行（当期，仅现金/银行科目；实际=当期已记账净额）
async fn get_funds_budget(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::funds::funds_budget(&db, period)?;
    Ok(Json(json!({ "rows": rows })))
}

/// 资金日报（按日）：?date=YYYY-MM-DD（默认今天）→ 上日结余/本日收支/日末结存
async fn get_funds_daily_date(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = state.db_for(&user.book_key)?;
    let date = match q.get("date") {
        Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
        None => chrono::Local::now().date_naive(),
    };
    let rows = findb::funds::funds_daily_by_date(&db, date)?;
    Ok(Json(json!({
        "date": date.format("%Y-%m-%d").to_string(),
        "rows": rows
    })))
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
    let sc = parse_money_checked(&req.standard_cost)?;
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
// 固定资产（与桌面端对齐：卡片 / 折旧计划 / 计提 / 清理）
// ---------------------------------------------------------------------------

fn asset_json(a: &findb::assets::Asset) -> serde_json::Value {
    json!({
        "id": a.id,
        "code": a.code,
        "name": a.name,
        "category": a.category,
        "spec": a.spec,
        "dept": a.dept,
        "asset_account": a.asset_account,
        "dep_account": a.dep_account,
        "expense_account": a.expense_account,
        "original_value": a.original_value.fmt_money(),
        "residual_rate": (a.residual_rate * Money::from_i64(100)).fmt_plain(),
        "life_months": a.life_months,
        "method": a.method.code(),
        "method_label": a.method.label(),
        "start_period": period_to_str(a.start_period),
        "disposed_period": a.disposed_period.map(period_to_str),
        "dispose_amount": a.dispose_amount.map(|m| m.fmt_money()),
        "status": a.status.code(),
        "status_label": a.status.label(),
        "voucher_id": a.voucher_id,
        "memo": a.memo,
    })
}

#[derive(Deserialize)]
struct AssetReq {
    #[serde(default)]
    pub id: i64,
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default)]
    pub dept: String,
    pub asset_account: String,
    pub dep_account: String,
    pub expense_account: String,
    pub original_value: String,
    /// 残值率按百分数录入（5 = 5%）
    #[serde(default)]
    pub residual_rate: String,
    pub life_months: i32,
    #[serde(default)]
    pub method: String,
    /// 启用期间 YYYYMM
    pub start_period: i32,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize, Default)]
struct AssetDisposeReq {
    pub ymm: i32,
    #[serde(default)]
    pub amount: String,
}

fn asset_from_req(r: &AssetReq) -> Result<findb::assets::Asset, AppError> {
    if r.code.trim().is_empty() {
        return Err(AppError::bad_request("资产编码不能为空"));
    }
    if r.name.trim().is_empty() {
        return Err(AppError::bad_request("资产名称不能为空"));
    }
    let start = period_checked(r.start_period)?;
    let original = parse_money_checked(&r.original_value)?;
    if !original.is_positive() {
        return Err(AppError::bad_request("资产原值必须大于 0"));
    }
    let rate = if r.residual_rate.trim().is_empty() {
        Money::parse("0.05").unwrap_or(Money::ZERO)
    } else {
        parse_money_checked(&r.residual_rate)?
            .checked_div(rust_decimal::Decimal::from(100))
            .expect("字面量 100 非零")
    };
    if rate.is_negative() || rate > Money::ONE {
        return Err(AppError::bad_request("残值率必须在 0% ~ 100% 之间"));
    }
    Ok(findb::assets::Asset {
        id: r.id,
        code: r.code.trim().to_string(),
        name: r.name.trim().to_string(),
        category: r.category.trim().to_string(),
        spec: r.spec.trim().to_string(),
        dept: r.dept.trim().to_string(),
        asset_account: r.asset_account.trim().to_string(),
        dep_account: r.dep_account.trim().to_string(),
        expense_account: r.expense_account.trim().to_string(),
        original_value: original,
        residual_rate: rate,
        life_months: r.life_months,
        method: fincore::engine::depreciation::DepMethod::parse(&r.method),
        start_period: start,
        disposed_period: None,
        dispose_amount: None,
        status: findb::assets::AssetStatus::InUse,
        voucher_id: None,
        memo: r.memo.trim().to_string(),
    })
}

async fn list_assets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let cards: Vec<serde_json::Value> = findb::assets::list(&db)?.iter().map(asset_json).collect();
    let ledger: Vec<serde_json::Value> = findb::assets::ledger(&db, period)?
        .iter()
        .map(|r| {
            json!({
                "asset": asset_json(&r.asset),
                "accum": r.accum.fmt_money(),
                "net": r.net.fmt_money(),
                "months": r.months,
            })
        })
        .collect();
    let plan: Vec<serde_json::Value> = findb::assets::dep_plan(&db, period)?
        .iter()
        .map(|p| {
            json!({
                "asset_id": p.asset_id, "code": p.code, "name": p.name, "dept": p.dept,
                "expense_account": p.expense_account, "dep_account": p.dep_account,
                "amount": p.amount.fmt_money(), "accum": p.accum.fmt_money(), "net": p.net.fmt_money(),
            })
        })
        .collect();
    let deps: Vec<serde_json::Value> = findb::assets::dep_list_period(&db, period)?
        .iter()
        .map(|d| {
            json!({
                "asset_id": d.asset_id, "period": period_to_str(d.period),
                "amount": d.amount.fmt_money(), "accum": d.accum.fmt_money(),
                "net_value": d.net_value.fmt_money(), "voucher_id": d.voucher_id,
            })
        })
        .collect();
    Ok(Json(json!({
        "period": period_to_str(period),
        "cards": cards,
        "ledger": ledger,
        "plan": plan,
        "deps": deps,
    })))
}

async fn create_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssetReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let a = asset_from_req(&req)?;
    if findb::assets::get_by_code(&db, &a.code)?.is_some() {
        return Err(AppError::bad_request("资产编码已存在"));
    }
    let id = findb::assets::insert(&db, &a)?;
    db.log(
        user.username(),
        "固定资产",
        "新增卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "id": id })))
}

async fn update_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AssetReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let existing = findb::assets::get(&db, id)?
        .ok_or_else(|| AppError::not_found("资产卡片不存在"))?;
    if existing.status == findb::assets::AssetStatus::Disposed {
        return Err(AppError::bad_request("已清理的卡片不能修改"));
    }
    let mut a = asset_from_req(&req)?;
    a.id = id;
    a.status = existing.status;
    a.disposed_period = existing.disposed_period;
    a.dispose_amount = existing.dispose_amount;
    a.voucher_id = existing.voucher_id;
    // 已计提过折旧：影响计价的字段冻结，避免历史折旧与台账对不上
    if !findb::assets::dep_list(&db, id)?.is_empty() {
        let frozen = a.original_value != existing.original_value
            || a.residual_rate != existing.residual_rate
            || a.life_months != existing.life_months
            || a.start_period != existing.start_period
            || a.method != existing.method;
        if frozen {
            return Err(AppError::bad_request(
                "已计提折旧的卡片不能修改原值/残值率/年限/启用期间/折旧方法",
            ));
        }
    }
    findb::assets::update(&db, &a)?;
    db.log(
        user.username(),
        "固定资产",
        "修改卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let a = findb::assets::get(&db, id)?
        .ok_or_else(|| AppError::not_found("资产卡片不存在"))?;
    findb::assets::delete(&db, id)?;
    db.log(
        user.username(),
        "固定资产",
        "删除卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_asset_deps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let rows: Vec<serde_json::Value> = findb::assets::dep_list(&db, id)?
        .iter()
        .map(|d| {
            json!({
                "period": period_to_str(d.period),
                "amount": d.amount.fmt_money(),
                "accum": d.accum.fmt_money(),
                "net_value": d.net_value.fmt_money(),
                "voucher_id": d.voucher_id,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

async fn dispose_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AssetDisposeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = period_checked(req.ymm)?;
    let amount = if req.amount.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.amount)?
    };
    findb::assets::dispose(&db, id, period, amount)?;
    db.log(
        user.username(),
        "固定资产",
        "资产清理",
        &format!("#{id} {} 金额 {}", period_to_str(period), amount.fmt_money()),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn depreciate_assets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = period_checked(req.ymm)?;
    let res = findb::assets::depreciate_period(&db, period, user.username())?;
    Ok(Json(json!({
        "already": res.already,
        "count": res.count,
        "total": res.total.fmt_money(),
        "voucher_id": res.voucher_id,
        "voucher_no": res.voucher_no,
    })))
}

async fn delete_asset_deps_period(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = state.db_for(&user.book_key)?;
    let period = period_checked(req.ymm)?;
    let removed = findb::assets::dep_delete_period(&db, period)?;
    db.log(
        user.username(),
        "固定资产",
        "删除本期折旧",
        &format!("{} 共 {removed} 条", period_to_str(period)),
    )?;
    Ok(Json(json!({ "removed": removed })))
}

// ---------------------------------------------------------------------------
// 银行对账（与桌面端对齐）
// ---------------------------------------------------------------------------

fn stmt_json(s: &findb::bank::Statement) -> serde_json::Value {
    json!({
        "id": s.id,
        "period": period_to_str(s.period),
        "account": s.account_code,
        "date": s.biz_date.format("%Y-%m-%d").to_string(),
        "summary": s.summary,
        "settle_no": s.settle_no,
        "debit": s.debit.fmt_money(),
        "credit": s.credit.fmt_money(),
        "balance": s.balance.fmt_money(),
        "entry_id": s.entry_id,
        "matched_by": s.matched_by,
    })
}

fn book_entry_json(b: &findb::bank::BookEntry) -> serde_json::Value {
    json!({
        "entry_id": b.entry_id,
        "voucher_id": b.voucher_id,
        "date": b.date.format("%Y-%m-%d").to_string(),
        "voucher_no": b.voucher_label(),
        "summary": b.summary,
        "settle_no": b.settle_no,
        "debit": b.debit.fmt_money(),
        "credit": b.credit.fmt_money(),
    })
}

fn reconcile_json(r: &findb::bank::Reconciliation) -> serde_json::Value {
    json!({
        "period": period_to_str(r.period),
        "account": r.account_code,
        "bank_balance": r.bank_balance.fmt_money(),
        "book_balance": r.book_balance.fmt_money(),
        "bank_adjusted": r.bank_adjusted.fmt_money(),
        "book_adjusted": r.book_adjusted.fmt_money(),
        "balanced": r.balanced(),
        "diff": r.diff().fmt_money(),
        "book_only_in": r.book_only_in.iter().map(book_entry_json).collect::<Vec<_>>(),
        "book_only_out": r.book_only_out.iter().map(book_entry_json).collect::<Vec<_>>(),
        "bank_only_in": r.bank_only_in.iter().map(stmt_json).collect::<Vec<_>>(),
        "bank_only_out": r.bank_only_out.iter().map(stmt_json).collect::<Vec<_>>(),
    })
}

#[derive(Deserialize, Default)]
struct BankImportReq {
    pub ymm: i32,
    pub account: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Deserialize)]
struct BankAutoReq {
    pub ymm: i32,
    pub account: String,
    #[serde(default)]
    pub tolerance: i64,
}

#[derive(Deserialize)]
struct BankLinkReq {
    pub stmt_id: i64,
    pub entry_id: i64,
}

#[derive(Deserialize)]
struct BankStmtIdReq {
    pub stmt_id: i64,
}

async fn get_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let db = state.db_for(&user.book_key)?;
    let statements = findb::bank::list(&db, period, account.trim())?;
    let book = findb::bank::book_side(&db, period, account.trim())?;
    let recon = findb::bank::reconcile(&db, period, account.trim())?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "account": account.trim(),
        "statements": statements.iter().map(stmt_json).collect::<Vec<_>>(),
        "book": book.iter().map(book_entry_json).collect::<Vec<_>>(),
        "reconcile": reconcile_json(&recon),
    })))
}

async fn import_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankImportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = state.db_for(&user.book_key)?;
    let (n, warnings) = findb::bank::import_csv(&db, period, &account, &req.text)?;
    db.log(
        user.username(),
        "银行对账",
        "导入对账单",
        &format!("{account} {n} 条"),
    )?;
    Ok(Json(json!({ "imported": n, "warnings": warnings })))
}

async fn auto_match_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = state.db_for(&user.book_key)?;
    let r = findb::bank::auto_match(&db, period, &account, req.tolerance, user.username())?;
    db.log(
        user.username(),
        "银行对账",
        "自动勾对",
        &format!("{account} 成功 {} 对", r.matched),
    )?;
    Ok(Json(json!({
        "matched": r.matched,
        "by_no": r.by_no,
        "by_amount_date": r.by_amount_date,
        "by_amount": r.by_amount,
        "ambiguous": r.ambiguous,
    })))
}

async fn link_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankLinkReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    findb::bank::link(&db, req.stmt_id, req.entry_id, user.username())?;
    db.log(
        user.username(),
        "银行对账",
        "手工勾对",
        &format!("流水#{} ↔ 分录#{}", req.stmt_id, req.entry_id),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn unlink_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankStmtIdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    findb::bank::unlink(&db, req.stmt_id)?;
    db.log(
        user.username(),
        "银行对账",
        "取消勾对",
        &format!("流水#{}", req.stmt_id),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn clear_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = state.db_for(&user.book_key)?;
    let n = findb::bank::clear(&db, period, &account)?;
    db.log(
        user.username(),
        "银行对账",
        "清空对账单",
        &format!("{account} {} 条", period_to_str(period)),
    )?;
    Ok(Json(json!({ "removed": n })))
}

// ---------------------------------------------------------------------------
// 往来核销（与桌面端对齐）
// ---------------------------------------------------------------------------

fn open_entry_json(e: &findb::settle::OpenEntry) -> serde_json::Value {
    json!({
        "entry_id": e.entry_id,
        "voucher_id": e.voucher_id,
        "period": period_to_str(e.period),
        "date": e.date.format("%Y-%m-%d").to_string(),
        "voucher_no": format!("{}-{:04}", e.word, e.no),
        "line": e.line,
        "summary": e.summary,
        "account_code": e.account_code,
        "aux_key": e.aux_key,
        "settle_no": e.settle_no,
        "debit": e.debit.fmt_money(),
        "credit": e.credit.fmt_money(),
        "settled": e.settled.fmt_money(),
        "open": e.open().fmt_money(),
        "dir": e.dir_label(),
    })
}

#[derive(Deserialize, Default)]
struct SettleAutoReq {
    pub account: String,
    #[serde(default)]
    pub ymm: i32,
    #[serde(default)]
    pub tolerance: String,
}

#[derive(Deserialize)]
struct SettleRunReq {
    pub from_entry: i64,
    pub to_entry: i64,
    pub amount: String,
}

#[derive(Deserialize)]
struct UnsettleReq {
    pub id: i64,
}

async fn get_settle_open(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = q
        .get("upto")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let include_all = q.get("all").map(|s| s == "1" || s == "true").unwrap_or(false);
    let db = state.db_for(&user.book_key)?;
    let mut rows = findb::settle::open_entries(&db, account.trim(), upto, include_all)?;
    if !include_all {
        rows.retain(|e| e.is_open());
    }
    Ok(Json(json!({
        "account": account.trim(),
        "upto": period_to_str(upto),
        "rows": rows.iter().map(open_entry_json).collect::<Vec<_>>(),
    })))
}

async fn auto_settle_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SettleAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = if req.ymm > 0 {
        period_checked(req.ymm)?
    } else {
        current_period(&state, &user)
    };
    let tolerance = if req.tolerance.trim().is_empty() {
        Money::parse("0.01").unwrap_or(Money::ZERO)
    } else {
        parse_money_checked(&req.tolerance)?
    };
    let db = state.db_for(&user.book_key)?;
    let r = findb::settle::auto_settle(&db, &account, upto, tolerance, user.username())?;
    db.log(
        user.username(),
        "往来",
        "自动核销",
        &format!("{account} {} 对 {}", r.pairs, r.amount.fmt_money()),
    )?;
    Ok(Json(json!({
        "pairs": r.pairs,
        "amount": r.amount.fmt_money(),
        "exact": r.exact,
        "written_off": r.written_off,
    })))
}

async fn manual_settle_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SettleRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let amount = parse_money_checked(&req.amount)?;
    let db = state.db_for(&user.book_key)?;
    let id = findb::settle::settle(&db, req.from_entry, req.to_entry, amount, user.username())?;
    db.log(
        user.username(),
        "往来",
        "手工核销",
        &format!("分录#{} ↔ 分录#{} {}", req.from_entry, req.to_entry, amount.fmt_money()),
    )?;
    Ok(Json(json!({ "id": id })))
}

async fn list_settle_records(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let db = state.db_for(&user.book_key)?;
    let rows: Vec<serde_json::Value> = findb::settle::list(&db, account.trim())?
        .iter()
        .map(|r| {
            json!({
                "id": r.id, "period": period_to_str(r.period), "account": r.account_code,
                "aux_key": r.aux_key, "from_entry": r.from_entry, "to_entry": r.to_entry,
                "amount": r.amount.fmt_money(), "settled_by": r.settled_by, "settled_at": r.settled_at,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

async fn unsettle_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<UnsettleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    findb::settle::unsettle(&db, req.id)?;
    db.log(user.username(), "往来", "取消核销", &format!("记录#{}", req.id))?;
    Ok(Json(json!({ "ok": true })))
}

async fn get_settle_aging(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = q
        .get("upto")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let as_of = q
        .get("as_of")
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    let by_year = q.get("scheme").map(|s| s == "year").unwrap_or(false);
    let buckets = if by_year {
        fincore::engine::aging::buckets_by_year()
    } else {
        fincore::engine::aging::buckets_by_days()
    };
    let db = state.db_for(&user.book_key)?;
    let lines = findb::settle::aging(&db, account.trim(), upto, as_of, &buckets)?;
    let labels: Vec<&str> = buckets.iter().map(|b| b.label).collect();
    let rows: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| {
            json!({
                "key": l.key,
                "amounts": l.amounts.iter().map(|m| m.fmt_money()).collect::<Vec<_>>(),
                "total": l.total.fmt_money(),
                "credit_total": l.credit_total.fmt_money(),
                "net": l.net().fmt_money(),
                "max_days": l.max_days,
            })
        })
        .collect();
    Ok(Json(json!({
        "account": account.trim(),
        "as_of": as_of.format("%Y-%m-%d").to_string(),
        "buckets": labels,
        "rows": rows,
    })))
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
    // 资产负债表第二列是"年初余额"：取数基准必须从会计年度 1 月起，
    // 不能用请求里的 from（否则 5 月查表会把 5 月初当成"年初"）。
    let (bq_from, subtitle) = if key == "balance_sheet" {
        (Period::new(to.year(), 1).unwrap_or(from), format!("{} 期末", to.label()))
    } else {
        (from, format!("{} 至 {}", from.label(), to.label()))
    };
    let mut bq = BalanceQuery::range(bq_from, to);
    bq = bq.with_user_scope(&user.user);
    let snap = BalanceSnapshot::load(db, &bq)?;
    let company = db.options().company;
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
    let limit: i64 = q
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
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
    let period = match req.period.filter(|ym| *ym > 0) {
        Some(ym) => period_checked(ym)?,
        None => current_period(&state, &user),
    };
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
    let period = match q.get("period").and_then(|s| s.parse::<i32>().ok()) {
        Some(ym) => period_checked(ym)?,
        None => db.options().start_period,
    };
    Ok(Json(template::due_list(&db, period)?))
}

// ---------------- 辅助核算档案 ----------------
async fn list_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AuxEntity>>, AppError> {
    // 档案列表是**只读参照**（凭证/收付款单都要选客户、供应商、职员……），
    // 任何可记账角色都需要；增删改仍由 AuxEdit 把关。
    user.require(Perm::Report)?;
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
    if !doc_in_scope(&user, employee) {
        return Err(AppError::forbidden("数据范围受限，不能录入他人的工资行"));
    }
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    let p = business::payroll_calc(
        &db,
        period,
        employee,
        r.dept.trim(),
        parse_money_checked(&r.gross)?,
        parse_money_checked(&r.social)?,
        parse_money_checked(&r.housing)?,
        parse_money_checked(&r.deduction)?,
        parse_money_checked(&r.additional)?,
        parse_money_checked(&r.social_co)?,
        parse_money_checked(&r.housing_co)?,
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
    if user.user.data_scope.own_doc_only {
        return Err(AppError::forbidden("数据范围受限的账号不能批量生成工资表"));
    }
    let period = req
        .period
        .as_deref()
        .and_then(parse_period)
        .unwrap_or_else(|| current_period(&state, &user));
    let mut rows: Vec<(String, String, Money, Money, Money, Money, Money, Money, Money)> =
        Vec::with_capacity(req.rows.len());
    for r in &req.rows {
        if r.employee.trim().is_empty() {
            continue;
        }
        rows.push((
            r.employee.trim().to_string(),
            r.dept.trim().to_string(),
            parse_money_checked(&r.gross)?,
            parse_money_checked(&r.social)?,
            parse_money_checked(&r.housing)?,
            parse_money_checked(&r.deduction)?,
            parse_money_checked(&r.additional)?,
            parse_money_checked(&r.social_co)?,
            parse_money_checked(&r.housing_co)?,
        ));
    }
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
    let p = business::payroll_get_by_id(&db, id)?
        .ok_or_else(|| AppError::not_found("工资行不存在"))?;
    if !doc_in_scope(&user, &p.employee) {
        return Err(AppError::forbidden("无权操作他人的工资行"));
    }
    if p.voucher_id.is_some() {
        return Err(AppError::bad_request("该工资行已生成凭证，不能删除"));
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
    if !doc_in_scope(&user, employee.trim()) {
        return Err(AppError::forbidden("数据范围受限，不能查看他人的年度工资累计"));
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

fn claim_items(items: &[ClaimItemInput]) -> Result<Vec<business::ClaimItem>, AppError> {
    let mut out = Vec::with_capacity(items.len());
    for i in items {
        out.push(business::ClaimItem {
            expense_account: i.expense_account.trim().to_string(),
            amount: parse_money_checked(&i.amount)?,
            memo: i.memo.trim().to_string(),
        });
    }
    Ok(out)
}

async fn list_claims(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<business::Claim>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = state.db_for(&user.book_key)?;
    let period = query_period(&state, &user, &q);
    // 空串 = 全部状态：UI 下拉默认值为空，直接 parse 会落到 Draft 只显示草稿
    let status = q
        .get("status")
        .filter(|s| !s.is_empty())
        .map(|s| business::ClaimStatus::parse(s));
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
    if !doc_in_scope(&user, applicant) {
        return Err(AppError::forbidden("数据范围受限，不能为他人填报销单"));
    }
    let db = state.db_for(&user.book_key)?;
    let period = match r.period.filter(|ym| *ym > 0) {
        Some(ym) => period_checked(ym)?,
        None => current_period(&state, &user),
    };
    let date = req_date(&r.biz_date, period.last_day())?;
    let c = business::Claim {
        id: 0,
        period,
        no: business::claim_next_no(&db, period)?,
        biz_date: date,
        applicant: applicant.to_string(),
        dept: r.dept.trim().to_string(),
        reason: r.reason.trim().to_string(),
        amount: parse_money_checked(&r.amount)?,
        status: business::ClaimStatus::Draft,
        items: claim_items(&r.items)?,
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
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能修改他人的报销单"));
    }
    if !c.status.editable() {
        return Err(AppError::bad_request("当前状态不允许修改内容"));
    }
    if c.voucher_id.is_some() {
        return Err(AppError::bad_request("已生成凭证的报销单不能修改"));
    }
    let date = req_date(&r.biz_date, c.period.last_day())?;
    c.biz_date = date;
    c.applicant = r.applicant.trim().to_string();
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能改由他人作为申请人"));
    }
    c.dept = r.dept.trim().to_string();
    c.reason = r.reason.trim().to_string();
    c.amount = parse_money_checked(&r.amount)?;
    c.items = claim_items(&r.items)?;
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
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能删除他人的报销单"));
    }
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
    let c = business::claim_get(&db, id)?
        .ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能操作他人的报销单"));
    }
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
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能为他人报销单生成凭证"));
    }
    if c.voucher_id.is_some() {
        // 幂等：支付时已自动出过凭证 → 返回同一张（不新增）
        let existing = c.voucher_id.unwrap();
        return Ok(Json(json!({ "id": existing, "already": true })));
    }
    let vid = business::claim_voucher(&db, id, pay, user.username())?;
    db.log(user.username(), "报销", "生成凭证", &format!("#{id} 凭证 #{vid}"))?;
    Ok(Json(json!({ "id": vid })))
}
