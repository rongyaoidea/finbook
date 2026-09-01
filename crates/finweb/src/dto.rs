//! 请求 / 响应数据结构（与前端 JSON 契约）

use fincore::user::{Perm, Role, User};
use fincore::Voucher;
use serde::{Deserialize, Serialize};

use crate::state::period_to_str;

/// 对外暴露的用户信息（不含口令等敏感字段）
#[derive(Clone, Serialize)]
pub struct PublicUser {
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub role_label: String,
    pub is_admin: bool,
    pub device_name: String,
    /// 是否已停用（前端据此显示账号状态）
    pub disabled: bool,
    /// 该用户拥有的权限（snake_case 名称）
    pub perms: Vec<String>,
}

impl PublicUser {
    pub fn from_user(u: &User) -> Self {
        let perms = Perm::all()
            .iter()
            .filter(|p| u.can(**p))
            .map(|p| serde_json::to_value(p).ok())
            .flatten()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        PublicUser {
            username: u.username.clone(),
            display_name: u.display_name.clone(),
            role: u.role,
            role_label: u.role.label().to_string(),
            is_admin: u.is_admin(),
            device_name: u.device_name.clone(),
            disabled: u.disabled,
            perms,
        }
    }
}

#[derive(Serialize)]
pub struct SetupStatus {
    /// 管理员账号是否已设定（不暴露用户名，避免登录页被枚举）
    pub admin_set: bool,
    /// 账套是否已初始化建账（公司名/启用期间是否已设定）
    pub needs_setup: bool,
    pub company: String,
    pub version: String,
    pub book: String,
}

impl VoucherDetail {
    pub fn from_voucher(v: Voucher) -> Self {
        let period_label = period_to_str(v.period);
        let voucher_no = v.voucher_no();
        VoucherDetail {
            voucher: v,
            voucher_no,
            period_label,
        }
    }
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
    /// 浏览器/设备指纹（前端生成并持久化在 localStorage）
    pub device_id: String,
    /// 设备展示名（浏览器 UA / 主机名）
    pub device_name: String,
    /// 账套 key（空 = 默认账套）
    #[serde(default)]
    pub book_key: String,
}

#[derive(Serialize)]
pub struct LoginResp {
    pub user: PublicUser,
    /// 是否强制要求改密
    pub must_change_pwd: bool,
    /// 本次登录是否触发了「首次登录即管理员」初始化
    pub setup: bool,
}

#[derive(Deserialize)]
pub struct ChangePwdReq {
    pub old: String,
    pub new: String,
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    pub username: String,
    pub display_name: String,
    pub password: String,
    #[serde(default)]
    pub role: Role,
}

#[derive(Deserialize)]
pub struct UpdateUserReq {
    pub display_name: Option<String>,
    pub role: Option<Role>,
    pub disabled: Option<bool>,
    pub must_change_pwd: Option<bool>,
}

#[derive(Deserialize)]
pub struct ResetPwdReq {
    pub new: String,
}

#[derive(Deserialize)]
pub struct PeriodReq {
    pub ymm: i32,
}

/// 凭证分录（前端提交的最小字段）
#[derive(Deserialize)]
pub struct VoucherEntryDto {
    pub line: i32,
    pub summary: String,
    pub account_code: String,
    /// 借方金额（十进制字符串）
    pub debit: String,
    /// 贷方金额（十进制字符串）
    pub credit: String,
}

/// 保存凭证请求
#[derive(Deserialize)]
pub struct SaveVoucherReq {
    pub id: i64,
    /// 期间 ymm
    pub period: i32,
    /// 日期 YYYY-MM-DD
    pub date: String,
    pub word: String,
    pub no: i32,
    pub attachments: i32,
    pub memo: String,
    pub entries: Vec<VoucherEntryDto>,
}

/// 发票新增/更新请求
#[derive(Deserialize, Default)]
pub struct InvoiceReq {
    pub id: i64,
    /// in / out
    pub kind: String,
    pub code: String,
    pub number: String,
    pub date: String,
    pub buyer: String,
    pub seller: String,
    /// 价税合计（十进制字符串）
    pub amount_tax: String,
    /// 不含税金额
    pub amount: String,
    /// 税额
    pub tax: String,
    pub tax_rate: String,
    pub status: String,
    pub memo: String,
}

/// 发票状态流转请求
#[derive(Deserialize)]
pub struct InvoiceStatusReq {
    pub status: String,
}

/// 凭证列表项（轻量，不含分录明细）
#[derive(Serialize)]
pub struct VoucherListItem {
    pub id: i64,
    pub period: String,
    pub date: String,
    pub word: String,
    pub no: i32,
    pub voucher_no: String,
    pub summary: String,
    pub debit_total: String,
    pub credit_total: String,
    pub status: String,
    pub status_label: String,
    pub prepared_by: String,
}

#[derive(Serialize)]
pub struct Dashboard {
    pub company: String,
    pub start_period: String,
    pub current_period: String,
    pub closed_upto: Option<String>,
    pub vouchers: i64,
    pub entries: i64,
    pub accounts: i64,
}

/// 科目余额表行（对外契约：方向已展开、金额已格式化，前端直接渲染）
#[derive(Serialize)]
pub struct TrialRow {
    pub account_code: String,
    pub account_name: String,
    /// 期初方向："借" / "贷" / "平"
    pub begin_dir: String,
    /// 期初余额（绝对值，已格式化）
    pub begin: String,
    /// 本期借方（已格式化）
    pub debit: String,
    /// 本期贷方（已格式化）
    pub credit: String,
    /// 期末方向
    pub end_dir: String,
    /// 期末余额（绝对值，已格式化）
    pub end: String,
    pub ytd_debit: String,
    pub ytd_credit: String,
}

impl TrialRow {
    pub fn from_row(r: &fincore::balance::BalanceRow) -> Self {
        let (b_dir, b_amt) = r.begin_dir_amount();
        let (e_dir, e_amt) = r.end_dir_amount();
        TrialRow {
            account_code: r.account_code.clone(),
            account_name: r.account_name.clone(),
            begin_dir: b_dir.label().to_string(),
            begin: b_amt.fmt_money(),
            debit: r.debit.fmt_money(),
            credit: r.credit.fmt_money(),
            end_dir: e_dir.label().to_string(),
            end: e_amt.fmt_money(),
            ytd_debit: r.ytd_debit.fmt_money(),
            ytd_credit: r.ytd_credit.fmt_money(),
        }
    }
}

/// 凭证详情（补上列表同款的 voucher_no，避免前端拿不到凭证号）
#[derive(Serialize)]
pub struct VoucherDetail {
    #[serde(flatten)]
    pub voucher: Voucher,
    /// 凭证号（如 记-0001）
    pub voucher_no: String,
    /// 期间 "YYYY-MM"
    pub period_label: String,
}

/// 导入预检请求
#[derive(Deserialize)]
pub struct ImportAnalyzeReq {
    /// begin = 期初余额表；voucher = 凭证
    pub kind: String,
    /// CSV 文本（选择文件上传时可为空）
    #[serde(default)]
    pub text: String,
    /// 来源模板：generic / kingdee / yonyou
    #[serde(default)]
    pub template: String,
    /// Excel 文件内容（base64，.xlsx/.xls/.ods）；与 text 二选一
    #[serde(default)]
    pub file: Option<String>,
}

/// 导入执行请求
#[derive(Deserialize)]
pub struct ImportRunReq {
    /// begin = 期初余额表；voucher = 凭证
    pub kind: String,
    /// CSV 文本（选择文件上传时可为空）
    #[serde(default)]
    pub text: String,
    /// 来源模板：generic / kingdee / yonyou
    #[serde(default)]
    pub template: String,
    /// Excel 文件内容（base64，.xlsx/.xls/.ods）；与 text 二选一
    #[serde(default)]
    pub file: Option<String>,
    /// 凭证导入时的期间（YYYYMM）
    #[serde(default)]
    pub period: i32,
    /// 缺失科目映射：源编码 → 目标编码
    #[serde(default)]
    pub mapping: std::collections::HashMap<String, String>,
}
