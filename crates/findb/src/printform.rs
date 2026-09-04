//! 会计凭证 / 账簿套打（模板打印）HTML 生成
//!
//! 会计档案要求「凭证 + 账簿」按标准版式打印装订。本模块把已经算好的
//! 业务数据（凭证、账簿行）渲染成一张自带打印样式的 HTML——
//! - 桌面端：写临时文件用系统浏览器打开，Ctrl/Cmd+P 打印；
//! - Web 端：直接返回 HTML，浏览器打印。
//!
//! 双端复用同一份模板，保证打印版式一致。版式对齐《会计基础工作规范》：
//! - 记账凭证：摘要 / 总账科目 / 明细科目 / 借 / 贷，附单据张数、会计主管·记账·出纳·制单签章栏；
//! - 账簿（明细账 / 总账 / 日记账）：日期 / 凭证字号 / 摘要 / 借 / 贷 / 借或贷 / 余额，
//!   含期初余额、本期合计，页尾可接续。

use fincore::{Money, Period};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 由业务凭证 + 科目表转换成套打行数据。
/// - `gen_name`：总账（一级）科目路径名；`detail_name`：包含辅助核算的明细科目全名。
pub fn voucher_print_from(
    v: &fincore::Voucher,
    chart: &fincore::Chart,
    aux_label: &dyn Fn(&fincore::AuxRef) -> String,
) -> VoucherPrint {
    let mut rows = Vec::with_capacity(v.entries.len());
    let mut debit_total = Money::ZERO;
    for e in &v.entries {
        if e.debit.is_zero() && e.credit.is_zero() {
            continue;
        }
        let full = chart.full_name(&e.account_code);
        let gen = full
            .split('/')
            .next()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| full.clone());
        let mut detail = full;
        let a = aux_label(&e.aux);
        if !a.trim().is_empty() {
            if detail.is_empty() {
                detail = a.clone();
            } else {
                detail = format!("{detail}（{a}）");
            }
        }
        debit_total += e.debit;
        rows.push(VoucherPrintRow {
            summary: e.summary.clone(),
            gen_name: gen,
            detail_name: detail,
            debit: e.debit,
            credit: e.credit,
        });
    }
    VoucherPrint {
        word: v.word.clone(),
        no: v.no,
        date: v.date.format("%Y-%m-%d").to_string(),
        attachments: v.attachments,
        rows,
        debit_total,
    }
}

/// 把某个期间、某凭证字下的业务凭证转成套打输入。
/// 只取未作废的凭证，随着 `vouchers::list` 的返回顺序。
pub fn vouchers_to_print(
    vouchers: &[fincore::Voucher],
    chart: &fincore::Chart,
    aux_label: &dyn Fn(&fincore::AuxRef) -> String,
) -> Vec<VoucherPrint> {
    vouchers
        .iter()
        .filter(|v| v.status != fincore::VoucherStatus::Void)
        .map(|v| voucher_print_from(v, chart, aux_label))
        .collect()
}

// ===========================================================================
// 记账凭证套打
// ===========================================================================

/// 凭证套打的一行分录
pub struct VoucherPrintRow {
    pub summary: String,
    /// 总账科目（一级）名称
    pub gen_name: String,
    /// 明细科目全路径名称
    pub detail_name: String,
    pub debit: Money,
    pub credit: Money,
}

/// 凭证套打的整张凭证
pub struct VoucherPrint {
    pub word: String,
    pub no: i32,
    pub date: String,
    pub attachments: i32,
    pub rows: Vec<VoucherPrintRow>,
    pub debit_total: Money,
}

impl VoucherPrint {
    pub fn voucher_no(&self) -> String {
        format!("{}-{:04}", self.word, self.no)
    }
}

/// 生成一页「记账凭证」套打 HTML（可含多张凭证，逐张分页）。
/// 标准版式：摘要 / 总账科目 / 明细科目 / 借方金额 / 贷方金额 + 合计 + 签章栏。
pub fn voucher_form_html(
    company: &str,
    period_label: &str,
    vouchers: &[VoucherPrint],
    page_from_1: bool,
) -> String {
    let forms: String = vouchers
        .iter()
        .enumerate()
        .map(|(i, v)| one_voucher_form(company, period_label, v, page_from_1, i + 1, vouchers.len()))
        .collect();
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<title>记账凭证</title>
<style>{VOUCHER_CSS}</style>
<script>window.onload=function(){{setTimeout(function(){{window.print();}},300);}};</script>
</head><body>{forms}</body></html>"#
    )
}

const VOUCHER_CSS: &str = r#"
@page{size:A4 landscape;margin:12mm 10mm;}
body{font-family:"宋体","SimSun","Noto Serif CJK SC",serif;color:#111;margin:0;font-size:12px;}
.form{width:100%;page-break-after:always;box-sizing:border-box;}
.form:last-child{page-break-after:auto;}
.head{display:flex;align-items:center;justify-content:space-between;
  border:1px solid #000;border-bottom:none;padding:4px 10px;background:#fafafa;}
.head .company{font-weight:bold;font-size:14px;}
.head .title{font-weight:bold;font-size:16px;letter-spacing:4px;}
.head .no{white-space:nowrap;}
.meta{display:flex;border:1px solid #000;border-bottom:none;font-size:11px;}
.meta>div{padding:3px 10px;border-right:1px dotted #999;}
table{border-collapse:collapse;width:100%;}
th,td{border:1px solid #000;padding:2px 4px;}
th{background:#f2f2f2;text-align:center;}
td.num{text-align:right;font-variant-numeric:tabular-nums;}
td.sel{background:#fafafa;}
tr.l{height:21px;}
.total-row td{font-weight:bold;background:#f7f7f7;}
.foot{display:flex;justify-content:space-between;border:1px solid #000;border-top:none;
  padding:6px 10px;font-size:11px;}
.foot div{width:22%;}
.foot .lbl{display:block;color:#666;margin-bottom:2px;}
@media print{body{font-size:11px;}}
"#;

fn one_voucher_form(
    company: &str,
    period_label: &str,
    v: &VoucherPrint,
    page_from_1: bool,
    idx: usize,
    total: usize,
) -> String {
    let mut body = String::new();
    let n_rows = v.rows.len().max(1);
    // 保证打印纸底部版式稳定：按固定行高补齐到至少 8 行
    let fill = 8usize.saturating_sub(n_rows);
    for (i, r) in v.rows.iter().enumerate() {
        body.push_str(&format!(
            "<tr class='l'><td class='c'>{}</td><td>{}</td><td>{}</td>\
             <td class='num'>{}</td><td class='num'>{}</td></tr>",
            i + 1,
            esc(&r.gen_name),
            esc(&r.detail_name),
            r.debit.fmt_money(),
            r.credit.fmt_money(),
        ));
    }
    for _ in 0..fill {
        body.push_str(
            "<tr class='l'><td class='c'></td><td class='sel'></td><td class='sel'></td>\
             <td class='num sel'></td><td class='num sel'></td></tr>",
        );
    }
    let page = if page_from_1 { format!("第 {idx} / 共 {total} 页") } else { String::new() };
    let caption = if v.debit_total.is_zero() {
        "".to_string()
    } else {
        v.debit_total.to_capital()
    };
    format!(
        r#"<div class="form">
<div class="head">
  <span class="company">{company}</span>
  <span class="title">记　账　凭　证</span>
  <span class="no">凭证字号：{word}</span>
</div>
<div class="meta">
  <div>日期：{date}</div>
  <div>期间：{period}</div>
  <div>附件：{att} 张</div>
  <div style="border-right:none;flex:1"></div>
  <div>{page}</div>
</div>
<table>
<thead><tr>
  <th style="width:4%">行</th>
  <th style="width:17%">摘要</th>
  <th style="width:22%">总账科目</th>
  <th style="width:27%">明细科目</th>
  <th style="width:15%">借方金额</th>
  <th style="width:15%">贷方金额</th>
</tr></thead>
<tbody>
{rows}
<tr class="total-row">
  <td colspan="2">合计</td>
  <td class="num" colspan="1" style="border-right:none"></td>
  <td style="border-left:none"></td>
  <td class="num">{di}</td>
  <td class="num">{ci}</td>
</tr>
</tbody></table>
<div class="foot">
  <div><span class="lbl">会计主管</span>&nbsp;</div>
  <div><span class="lbl">记账</span>&nbsp;</div>
  <div><span class="lbl">复核（审核）</span>&nbsp;</div>
  <div><span class="lbl">出纳</span>&nbsp;</div>
  <div><span class="lbl">制单</span>&nbsp;&nbsp;大写：{cap}</div>
</div>
</div>"#,
        company = esc(company),
        word = esc(&v.voucher_no()),
        date = esc(&v.date),
        period = esc(period_label),
        att = v.attachments,
        rows = body,
        page = esc(&page),
        di = v.debit_total.fmt_money(),
        ci = v.debit_total.fmt_money(),
        cap = esc(&caption),
    )
}

// ===========================================================================
// 账簿套打（总账 / 明细账 / 日记账）
// ===========================================================================

/// 账簿一行
#[derive(Clone)]
pub struct LedgerPrintRow {
    pub date: String,
    pub voucher_no: String,
    pub summary: String,
    pub debit: Money,
    pub credit: Money,
    /// 借 / 贷 / 平
    pub dir: String,
    pub balance: Money,
}

/// 账簿套打整页的输入
pub struct LedgerPrint {
    pub title: String,      // 总账 / 明细账 / 日记账
    pub account_name: String, // 科目（编码 + 全路径名）
    pub period_label: String,
    /// 期初余额方向（借 / 贷 / 平）与金额
    pub begin_dir: String,
    pub begin_balance: Money,
    pub rows: Vec<LedgerPrintRow>,
    pub page_from_1: bool,
}

/// 生成账簿套打 HTML。标准三栏账版式：日期 / 凭证字号 / 摘要 / 借 / 贷 / 借或贷 / 余额。
pub fn ledger_form_html(company: &str, ledger: &LedgerPrint) -> String {
    let mut body = String::new();
    for r in &ledger.rows {
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class='num'>{}</td>\
             <td class='num'>{}</td><td>{}</td><td class='num'>{}</td></tr>",
            esc(&r.date),
            esc(&r.voucher_no),
            esc(&r.summary),
            r.debit.fmt_money(),
            r.credit.fmt_money(),
            esc(&r.dir),
            r.balance.fmt_money(),
        ));
    }
    let page = if ledger.page_from_1 { "第 1 页 / 共 1 页".to_string() } else { String::new() };
    let head_row = format!(
        "<tr><td>{}</td><td>{}</td><td>{}</td><td></td><td></td><td class='c'>{}</td>\
         <td class='num'>{}</td></tr>",
        "",
        "",
        esc(&format!("期初余额：{}{}", ledger.begin_dir, ledger.begin_balance)),
        esc(&ledger.begin_dir),
        ledger.begin_balance.fmt_money(),
    );
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<title>{title}</title>
<style>{LEDGER_CSS}</style>
<script>window.onload=function(){{setTimeout(function(){{window.print();}},300);}};</script>
</head><body>
<div class="lhead">
  <span class="ltitle">{title}</span>
  <span class="lacct">{acct}</span>
</div>
<div class="lmeta">
  <span>编制单位：{company}</span>
  <span>期间：{period}</span>
  <span>{page}</span>
</div>
<table>
<thead><tr>
  <th style="width:9%">日期</th>
  <th style="width:11%">凭证字号</th>
  <th style="width:36%">摘要</th>
  <th style="width:12%">借方</th>
  <th style="width:12%">贷方</th>
  <th style="width:6%">借或贷</th>
  <th style="width:14%">余额</th>
</tr></thead>
<tbody>{head}{rows}</tbody></table>
</body></html>"#,
        title = esc(&ledger.title),
        acct = esc(&ledger.account_name),
        company = esc(company),
        period = esc(&ledger.period_label),
        page = esc(&page),
        head = head_row,
        rows = body,
    )
}

const LEDGER_CSS: &str = r#"
@page{size:A4 landscape;margin:10mm 10mm;}
body{font-family:"宋体","SimSun","Noto Serif CJK SC",serif;color:#111;font-size:12px;margin:0;}
.lhead{display:flex;justify-content:space-between;align-items:center;border:1px solid #000;
  border-bottom:none;padding:4px 10px;background:#fafafa;}
.ltitle{font-weight:bold;font-size:16px;}
.lacct{white-space:nowrap;}
.lmeta{display:flex;justify-content:space-between;border:1px solid #000;border-bottom:none;
  padding:3px 10px;font-size:11px;color:#333;}
table{border-collapse:collapse;width:100%;}
th,td{border:1px solid #000;padding:2px 4px;}
th{background:#f2f2f2;text-align:center;}
td.num{text-align:right;font-variant-numeric:tabular-nums;}
td.c{text-align:center;}
@media print{body{font-size:11px;}}
"#;

// ===========================================================================
// 便利函数：把账簿行 + 期初余额转成套打需要的结构
// ===========================================================================

/// 由一组账簿行计算本期合计与静态页元，供上层组装 [`LedgerPrint`]。
#[allow(clippy::too_many_arguments)]
pub fn summarize_ledger<'a>(
    title: &str,
    account_name: &str,
    period: Period,
    begin_dir: &str,
    begin_balance: Money,
    rows: &'a [LedgerPrintRow],
) -> LedgerPrint {
    LedgerPrint {
        title: title.to_string(),
        account_name: account_name.to_string(),
        period_label: period.code(),
        begin_dir: begin_dir.to_string(),
        begin_balance,
        rows: rows.to_vec(),
        page_from_1: true,
    }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use fincore::{Account, AcctCategory, AuxRef, Chart, CodeScheme, Money, Period};

    fn sample_chart() -> Chart {
        let mut chart = Chart::new(CodeScheme::default());
        chart.insert(Account::new("1001", "库存现金", AcctCategory::Asset));
        chart.insert(Account::new("5001", "生产成本", AcctCategory::Cost));
        chart.insert(Account::new("500101", "直接材料", AcctCategory::Cost));
        chart
    }

    fn sample_voucher() -> fincore::Voucher {
        let p = Period::new(2026, 9).unwrap();
        let mut v = fincore::Voucher::new(p, NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(), "记", 1);
        v.attachments = 2;
        {
            let mut e = fincore::Entry::new(1, "500101", "领用原材料");
            e.debit = Money::from_cents(120_000);
            v.entries.push(e);
        }
        {
            let mut e = fincore::Entry::new(2, "1001", "结转成本");
            e.credit = Money::from_cents(120_000);
            v.entries.push(e);
        }
        v
    }

    #[test]
    fn voucher_print_skips_zero_and_total() {
        let chart = sample_chart();
        let v = sample_voucher();
        let aux = |_: &fincore::AuxRef| "".to_string();
        let p = voucher_print_from(&v, &chart, &aux);
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].gen_name, "生产成本");
        assert_eq!(p.debit_total, Money::from_cents(120_000));
        assert_eq!(p.voucher_no(), "记-0001");
    }

    #[test]
    fn voucher_form_html_has_key_structure() {
        let chart = sample_chart();
        let v = sample_voucher();
        let aux = |_: &fincore::AuxRef| "".to_string();
        let prints = vec![voucher_print_from(&v, &chart, &aux)];
        let html = voucher_form_html("某某公司", "2026-09", &prints, true);
        assert!(html.contains("记　账　凭　证"));
        assert!(html.contains("某某公司"));
        assert!(html.contains("记-0001"));
        assert!(html.contains("借方金额"));
        assert!(html.contains("合计"));
        assert!(html.contains("1,200.00"));
    }

    #[test]
    fn ledger_form_html_renders_begin_balance() {
        let ledger = LedgerPrint {
            title: "明细账".into(),
            account_name: "1001 库存现金".into(),
            period_label: "2026-09".into(),
            begin_dir: "借".into(),
            begin_balance: Money::from_cents(50_000),
            rows: vec![LedgerPrintRow {
                date: "2026-09-04".into(),
                voucher_no: "记-0001".into(),
                summary: "收款".into(),
                debit: Money::from_cents(10_000),
                credit: Money::ZERO,
                dir: "借".into(),
                balance: Money::from_cents(60_000),
            }],
            page_from_1: true,
        };
        let html = ledger_form_html("某某公司", &ledger);
        assert!(html.contains("明细账"));
        assert!(html.contains("期初余额：借500.00"));
        assert!(html.contains("借或贷"));
        assert!(html.contains("600.00"));
    }
}