//! 资金 / 预算分析 / 成本核算 三个拓展视图
//!
//! - 资金管理：现金/银行日记账、票据、融资、资金预测
//! - 预算分析：年度逐月预算 vs 实际（含部门维度）
//! - 成本核算：计价方式配置、期末结价
//!
//! 数据层都在 findb，这里只负责展示与交互。

use egui::{RichText, Ui};
use fincore::{Money, Period};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

// ===========================================================================
// 资金管理
// ===========================================================================

pub struct FundsView {
    pub tab: u8, // 0=资金日报 1=票据 2=融资 3=资金预测
    pub bill_kind: String,
    pub loan_kind: String,
    pub dirty: bool,
}

impl Default for FundsView {
    fn default() -> Self {
        Self {
            tab: 0,
            bill_kind: String::new(),
            loan_kind: String::new(),
            dirty: true,
        }
    }
}

impl FundsView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::page_header(ui, "资金管理", |ui| {
            ui.label(RichText::new("现金/银行资金日报 · 票据 · 融资 · 资金预测").weak());
        });

        widgets::toolbar(ui, |ui| {
            for (i, label) in ["资金日报", "票据", "融资", "资金预测"].iter().enumerate() {
                if ui.selectable_label(self.tab == i as u8, *label).clicked() {
                    self.tab = i as u8;
                    self.dirty = true;
                }
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        match self.tab {
            0 => self.show_daily(ctx, ui),
            1 => self.show_bills(ctx, ui),
            2 => self.show_loans(ctx, ui),
            _ => self.show_forecast(ctx, ui),
        }
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let (name, sh) = match self.tab {
            0 => {
                let rows = findb::funds::funds_daily(ctx.db(), ctx.period()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "资金日报",
                    vec!["科目".to_string(), "科目名称".to_string(), "期初".to_string(), "收入".to_string(), "支出".to_string(), "期末结存".to_string()],
                );
                for r in rows {
                    sh.push(vec![r.account_code.clone(), r.account_name.clone(), r.begin.fmt_plain(), r.income.fmt_plain(), r.expense.fmt_plain(), r.end.fmt_plain()]);
                }
                ("资金日报", sh)
            }
            1 => {
                let kind = if self.bill_kind.is_empty() { None } else { Some(self.bill_kind.as_str()) };
                let rows = findb::funds::bill_list(ctx.db(), kind).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "票据",
                    vec!["类型".to_string(), "票据号".to_string(), "出票日".to_string(), "到期日".to_string(), "对方单位".to_string(), "金额".to_string(), "状态".to_string()],
                );
                for b in rows {
                    sh.push(vec![
                        if b.kind == "receivable" { "应收".to_string() } else { "应付".to_string() },
                        b.no.clone(),
                        b.issue_date.format("%Y-%m-%d").to_string(),
                        b.due_date.format("%Y-%m-%d").to_string(),
                        b.counterpart.clone(),
                        b.amount.fmt_plain(),
                        findb::funds::BillStatus::parse(&b.status).label().to_string(),
                    ]);
                }
                ("票据", sh)
            }
            2 => {
                let kind = if self.loan_kind.is_empty() { None } else { Some(self.loan_kind.as_str()) };
                let rows = findb::funds::loan_list(ctx.db(), kind).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "融资",
                    vec!["类型".to_string(), "编号".to_string(), "机构".to_string(), "本金".to_string(), "年利率%".to_string(), "起息日".to_string(), "到期日".to_string(), "状态".to_string()],
                );
                for l in rows {
                    sh.push(vec![
                        if l.kind == "borrow" { "借款".to_string() } else { "放款".to_string() },
                        l.no.clone(),
                        l.bank.clone(),
                        l.principal.fmt_plain(),
                        l.rate_pct.fmt_qty(),
                        l.start_date.format("%Y-%m-%d").to_string(),
                        l.end_date.format("%Y-%m-%d").to_string(),
                        if l.status == "active" { "存续".to_string() } else { "已结清".to_string() },
                    ]);
                }
                ("融资", sh)
            }
            _ => {
                let fc = findb::funds::funds_forecast(ctx.db(), ctx.period()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "资金预测",
                    vec!["项目".to_string(), "金额".to_string()],
                );
                sh.push(vec!["现金/银行结存".to_string(), fc.cash_balance.fmt_plain()]);
                sh.push(vec!["在库应收票据".to_string(), fc.receivable_bills.fmt_plain()]);
                sh.push(vec!["应付票据".to_string(), fc.payable_bills.fmt_plain()]);
                sh.push(vec!["放款可收回".to_string(), fc.lend.fmt_plain()]);
                sh.push(vec!["借款需偿还".to_string(), fc.borrow.fmt_plain()]);
                sh.push(vec!["预计资金头寸".to_string(), fc.position.fmt_plain()]);
                ("资金预测", sh)
            }
        };
        let title = format!("{name}（{}）", ctx.period().label());
        match crate::views::export::run_export(&sh, name, &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }

    fn show_daily(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.dirty {
            self.dirty = false;
        }
        let rows = findb::funds::funds_daily(ctx.db(), ctx.period()).unwrap_or_default();
        let cols = [
            widgets::TCol::new("科目", 110.0).fixed(),
            widgets::TCol::new("科目名称", 160.0),
            widgets::TCol::new("期初", 130.0).right(),
            widgets::TCol::new("收入", 130.0).right(),
            widgets::TCol::new("支出", 130.0).right(),
            widgets::TCol::new("期末结存", 130.0).right(),
        ];
        widgets::grid(ui, "funds_daily", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.account_code).monospace()); }
                1 => { ui.label(&r.account_name); }
                2 => widgets::amount_label(ui, r.begin),
                3 => widgets::amount_label(ui, r.income),
                4 => widgets::amount_label(ui, r.expense),
                5 => widgets::amount_label(ui, r.end),
                _ => {}
            }
        });
    }

    fn show_bills(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("类型");
            egui::ComboBox::from_id_salt("bill_kind")
                .selected_text(if self.bill_kind.is_empty() {
                    "全部".to_string()
                } else {
                    self.bill_kind.clone()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.bill_kind, String::new(), "全部");
                    ui.selectable_value(&mut self.bill_kind, "receivable".to_string(), "应收票据");
                    ui.selectable_value(&mut self.bill_kind, "payable".to_string(), "应付票据");
                });
            if ui.button("新增票据").clicked() {
                let db = ctx.db();
                let who = ctx.user().username.clone();
                let mut b = findb::funds::Bill {
                    id: 0,
                    kind: "receivable".to_string(),
                    no: format!("PJ-{}", chrono::Local::now().format("%Y%m%d%H%M%S")),
                    period: ctx.period(),
                    issue_date: chrono::Local::now().date_naive(),
                    due_date: chrono::Local::now().date_naive(),
                    counterpart: String::new(),
                    bank: String::new(),
                    amount: Money::ZERO,
                    status: "in_hand".to_string(),
                    handled_date: None,
                    memo: String::new(),
                    created_by: who,
                    created_at: String::new(),
                };
                match findb::funds::bill_save(db, &mut b) {
                    Ok(id) => {
                        ctx.log("资金", "新增票据", &format!("#{id} {}", b.no));
                        ctx.info("已新增票据");
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });

        let rows = findb::funds::bill_list(ctx.db(), if self.bill_kind.is_empty() { None } else { Some(&self.bill_kind) })
            .unwrap_or_default();
        let cols = [
            widgets::TCol::new("类型", 70.0).fixed(),
            widgets::TCol::new("票据号", 130.0).fixed(),
            widgets::TCol::new("出票日", 100.0).fixed(),
            widgets::TCol::new("到期日", 100.0).fixed(),
            widgets::TCol::new("对方单位", 140.0),
            widgets::TCol::new("金额", 130.0).right(),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("操作", 120.0).fixed(),
        ];
        let mut act: Option<(i64, String)> = None;
        widgets::grid(ui, "bills", &cols, rows.len(), 24.0, |i, c, ui| {
            let b = &rows[i];
            match c {
                0 => { ui.label(if b.kind == "receivable" { "应收" } else { "应付" }); }
                1 => { ui.label(&b.no); }
                2 => { ui.label(b.issue_date.format("%Y-%m-%d").to_string()); }
                3 => { ui.label(b.due_date.format("%Y-%m-%d").to_string()); }
                4 => { ui.label(if b.counterpart.is_empty() { "—".to_string() } else { b.counterpart.clone() }); }
                5 => widgets::amount_label(ui, b.amount),
                6 => {
                    let s = findb::funds::BillStatus::parse(&b.status);
                    ui.label(RichText::new(s.label()).color(match s {
                        findb::funds::BillStatus::Settled => palette::OK,
                        findb::funds::BillStatus::InHand => palette::WARN,
                        _ => palette::CREDIT,
                    }));
                }
                7 => {
                    if b.status == "in_hand" {
                        ui.horizontal(|ui| {
                            if ui.small_button("背书").clicked() {
                                act = Some((b.id, "endorsed".to_string()));
                            }
                            if ui.small_button("贴现").clicked() {
                                act = Some((b.id, "discounted".to_string()));
                            }
                            if ui.small_button("兑付").clicked() {
                                act = Some((b.id, "settled".to_string()));
                            }
                        });
                    }
                }
                _ => {}
            }
        });
        if let Some((id, to)) = act {
            let st = findb::funds::BillStatus::parse(&to);
            match findb::funds::bill_transition(ctx.db(), id, st, chrono::Local::now().date_naive()) {
                Ok(()) => {
                    ctx.log("资金", "票据流转", &format!("#{id} → {}", st.label()));
                    ctx.info("已更新票据状态");
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn show_loans(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("类型");
            egui::ComboBox::from_id_salt("loan_kind")
                .selected_text(if self.loan_kind.is_empty() {
                    "全部".to_string()
                } else {
                    self.loan_kind.clone()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.loan_kind, String::new(), "全部");
                    ui.selectable_value(&mut self.loan_kind, "borrow".to_string(), "借款");
                    ui.selectable_value(&mut self.loan_kind, "lend".to_string(), "放款");
                });
            if ui.button("新增融资").clicked() {
                let db = ctx.db();
                let who = ctx.user().username.clone();
                let mut l = findb::funds::Loan {
                    id: 0,
                    kind: "borrow".to_string(),
                    no: format!("DK-{}", chrono::Local::now().format("%Y%m%d%H%M%S")),
                    bank: String::new(),
                    principal: Money::ZERO,
                    rate_pct: Money::ZERO,
                    start_date: chrono::Local::now().date_naive(),
                    end_date: chrono::Local::now().date_naive(),
                    status: "active".to_string(),
                    memo: String::new(),
                    created_by: who,
                    created_at: String::new(),
                };
                match findb::funds::loan_save(db, &mut l) {
                    Ok(id) => {
                        ctx.log("资金", "新增融资", &format!("#{id} {}", l.no));
                        ctx.info("已新增融资");
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });

        let rows = findb::funds::loan_list(ctx.db(), if self.loan_kind.is_empty() { None } else { Some(&self.loan_kind) })
            .unwrap_or_default();
        let cols = [
            widgets::TCol::new("类型", 70.0).fixed(),
            widgets::TCol::new("编号", 130.0).fixed(),
            widgets::TCol::new("机构", 140.0),
            widgets::TCol::new("本金", 130.0).right(),
            widgets::TCol::new("年利率%", 90.0).right(),
            widgets::TCol::new("起息日", 100.0).fixed(),
            widgets::TCol::new("到期日", 100.0).fixed(),
            widgets::TCol::new("状态", 80.0).fixed(),
        ];
        let mut settle: Option<i64> = None;
        widgets::grid(ui, "loans", &cols, rows.len(), 24.0, |i, c, ui| {
            let l = &rows[i];
            match c {
                0 => { ui.label(if l.kind == "borrow" { "借款" } else { "放款" }); }
                1 => { ui.label(&l.no); }
                2 => { ui.label(if l.bank.is_empty() { "—".to_string() } else { l.bank.clone() }); }
                3 => widgets::amount_label(ui, l.principal),
                4 => { ui.label(l.rate_pct.fmt_qty()); }
                5 => { ui.label(l.start_date.format("%Y-%m-%d").to_string()); }
                6 => { ui.label(l.end_date.format("%Y-%m-%d").to_string()); }
                7 => {
                    if l.status == "active" {
                        if ui.small_button("结清").clicked() {
                            settle = Some(l.id);
                        }
                    } else {
                        ui.label(RichText::new("已结清").color(palette::OK));
                    }
                }
                _ => {}
            }
        });
        if let Some(id) = settle {
            match findb::funds::loan_settle(ctx.db(), id) {
                Ok(()) => {
                    ctx.log("资金", "结清融资", &format!("#{id}"));
                    ctx.info("已结清");
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn show_forecast(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let fc = findb::funds::funds_forecast(ctx.db(), ctx.period()).unwrap_or_default();
        let items = [
            ("现金/银行结存", fc.cash_balance),
            ("在库应收票据", fc.receivable_bills),
            ("应付票据", fc.payable_bills),
            ("放款可收回", fc.lend),
            ("借款需偿还", fc.borrow),
            ("预计资金头寸", fc.position),
        ];
        widgets::grid(ui, "forecast", &[], items.len(), 30.0, |i, c, ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(items[i].0).strong());
                ui.label("：");
                widgets::amount_label(ui, items[i].1);
            });
            let _ = c;
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("头寸 = 结存 + 应收票据 − 应付票据 + 放款 − 借款（在库/存续口径）")
                .weak(),
        );
    }
}

// ===========================================================================
// 预算分析
// ===========================================================================

pub struct BudgetAnalysisView {
    pub year_text: String,
    pub version: String,
    pub dirty: bool,
}

impl Default for BudgetAnalysisView {
    fn default() -> Self {
        Self {
            year_text: String::new(),
            version: String::new(),
            dirty: true,
        }
    }
}

impl BudgetAnalysisView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.year_text.is_empty() {
            self.year_text = ctx.period().year().to_string();
        }
        widgets::page_header(ui, "预算分析", |ui| {
            ui.label(RichText::new("年度逐月预算 vs 实际，按科目 × 部门展开").weak());
        });
        widgets::toolbar(ui, |ui| {
            ui.label("年度");
            let r = ui.add_sized([60.0, 22.0], egui::TextEdit::singleline(&mut self.year_text));
            if r.changed() {
                self.dirty = true;
            }
            ui.label("版本");
            let r2 = ui.add_sized([100.0, 22.0], egui::TextEdit::singleline(&mut self.version).hint_text("留空=当前"));
            if r2.changed() {
                self.dirty = true;
            }
            if ui.button("查询").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                let year: i32 = self.year_text.trim().parse().unwrap_or_else(|_| ctx.period().year());
                let rows = findb::mgmt::budget_analysis_summary(ctx.db(), year, &self.version).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "预算分析",
                    vec!["科目".to_string(), "部门".to_string(), "预算".to_string(), "实际".to_string(), "执行率%".to_string()],
                );
                for r in rows {
                    sh.push(vec![
                        format!("{} {}", r.account_code, r.account_name),
                        if r.dept.is_empty() { "—".to_string() } else { r.dept.clone() },
                        r.budget.fmt_plain(),
                        r.actual.fmt_plain(),
                        r.rate.fmt_qty(),
                    ]);
                }
                let title = format!("预算分析（{} 年度）", year);
                match crate::views::export::run_export(&sh, "预算分析", &title, mode) {
                    Ok(m) => ctx.info(m),
                    Err(e) => ctx.error(e),
                }
            }
        });

        let year: i32 = self.year_text.trim().parse().unwrap_or_else(|_| ctx.period().year());
        let rows = findb::mgmt::budget_analysis(ctx.db(), year, &self.version, None).unwrap_or_default();
        let summary = findb::mgmt::budget_analysis_summary(ctx.db(), year, &self.version).unwrap_or_default();

        ui.separator();
        ui.label(RichText::new("年度汇总（科目 × 部门）").strong());
        let cols = [
            widgets::TCol::new("科目", 200.0),
            widgets::TCol::new("部门", 120.0),
            widgets::TCol::new("预算", 130.0).right(),
            widgets::TCol::new("实际", 130.0).right(),
            widgets::TCol::new("执行率%", 100.0).right(),
        ];
        widgets::grid(ui, "budget_ana_sum", &cols, summary.len(), 24.0, |i, c, ui| {
            let r = &summary[i];
            match c {
                0 => { ui.label(format!("{} {}", r.account_code, r.account_name)); }
                1 => { ui.label(if r.dept.is_empty() { "—".to_string() } else { r.dept.clone() }); }
                2 => widgets::amount_label(ui, r.budget),
                3 => widgets::amount_label(ui, r.actual),
                4 => {
                    let over = r.rate.to_f64() >= 100.0;
                    ui.label(RichText::new(r.rate.fmt_qty()).color(if over { palette::CREDIT } else { palette::OK }));
                }
                _ => {}
            }
        });

        if !rows.is_empty() {
            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("逐月明细").strong());
            let cols2 = [
                widgets::TCol::new("期间", 90.0).fixed(),
                widgets::TCol::new("科目", 200.0),
                widgets::TCol::new("部门", 120.0),
                widgets::TCol::new("预算", 130.0).right(),
                widgets::TCol::new("实际", 130.0).right(),
                widgets::TCol::new("执行率%", 100.0).right(),
            ];
            widgets::grid(ui, "budget_ana_detail", &cols2, rows.len(), 24.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => { ui.label(r.period.label()); }
                    1 => { ui.label(format!("{} {}", r.account_code, r.account_name)); }
                    2 => { ui.label(if r.dept.is_empty() { "—".to_string() } else { r.dept.clone() }); }
                    3 => widgets::amount_label(ui, r.budget),
                    4 => widgets::amount_label(ui, r.actual),
                    5 => { ui.label(RichText::new(r.rate.fmt_qty()).color(if r.rate.to_f64() >= 100.0 { palette::CREDIT } else { palette::OK })); }
                    _ => {}
                }
            });
        }
    }
}

// ===========================================================================
// 成本核算
// ===========================================================================

pub struct CostView {
    pub tab: u8, // 0=计价配置 1=期末结价
    pub period_text: String,
    pub configs: Vec<findb::business::CostConfigRow>,
    pub close_rows: Vec<findb::business::PeriodEndCostRow>,
    pub dirty: bool,
    pub key: String,
}

impl Default for CostView {
    fn default() -> Self {
        Self {
            tab: 0,
            period_text: String::new(),
            configs: Vec::new(),
            close_rows: Vec::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl CostView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        widgets::page_header(ui, "成本核算", |ui| {
            ui.label(RichText::new("存货计价方式配置 · 期末结价").weak());
        });
        widgets::toolbar(ui, |ui| {
            for (i, label) in ["计价方式", "期末结价"].iter().enumerate() {
                if ui.selectable_label(self.tab == i as u8, *label).clicked() {
                    self.tab = i as u8;
                    self.dirty = true;
                }
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        match self.tab {
            0 => self.show_configs(ctx, ui),
            _ => self.show_period_end(ctx, ui),
        }
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let (name, sh) = match self.tab {
            0 => {
                let rows = findb::business::cost_configs(ctx.db()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "计价方式配置",
                    vec!["存货".to_string(), "计价方式".to_string(), "标准成本".to_string()],
                );
                for r in rows {
                    sh.push(vec![r.item.clone(), r.method_label.clone(), r.standard_cost.fmt_plain()]);
                }
                ("计价方式配置", sh)
            }
            _ => {
                let p = Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period());
                let rows = findb::business::period_end_cost(ctx.db(), p, false).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "期末结价",
                    vec!["存货".to_string(), "计价方式".to_string(), "结存数量".to_string(), "结存金额".to_string(), "单价".to_string(), "调整额".to_string()],
                );
                for r in rows {
                    sh.push(vec![r.item.clone(), r.method.clone(), r.end_qty.fmt_qty(), r.end_amount.fmt_plain(), r.unit_cost.fmt_plain(), r.adjust.fmt_plain()]);
                }
                ("期末结价", sh)
            }
        };
        let title = format!("{name}（{}）", ctx.period().label());
        match crate::views::export::run_export(&sh, name, &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }

    fn show_configs(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.dirty {
            self.dirty = false;
            self.configs = findb::business::cost_configs(ctx.db()).unwrap_or_default();
        }
        widgets::toolbar(ui, |ui| {
            if ui.button("新增配置").clicked() {
                let item = "ITEM".to_string();
                let method = "moving_average".to_string();
                match findb::business::item_cost_method_set(ctx.db(), &item, Some(&method), Money::ZERO) {
                    Ok(()) => {
                        ctx.log("成本", "设置计价方式", &format!("{item} → {method}"));
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });
        let rows = self.configs.clone();
        let cols = [
            widgets::TCol::new("存货", 140.0).fixed(),
            widgets::TCol::new("计价方式", 160.0),
            widgets::TCol::new("标准成本", 130.0).right(),
            widgets::TCol::new("操作", 100.0).fixed(),
        ];
        let mut clear: Option<String> = None;
        widgets::grid(ui, "cost_configs", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.item).monospace()); }
                1 => { ui.label(&r.method_label); }
                2 => widgets::amount_label(ui, r.standard_cost),
                3 => {
                    if ui.small_button("清除").clicked() {
                        clear = Some(r.item.clone());
                    }
                }
                _ => {}
            }
        });
        if let Some(item) = clear {
            match findb::business::item_cost_method_clear(ctx.db(), &item) {
                Ok(()) => {
                    ctx.log("成本", "清除计价配置", &item);
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn show_period_end(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("试算").clicked() {
                self.reload_close(ctx);
            }
            if ui.button("结价（写入调整）").clicked() {
                let p = Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period());
                match findb::business::period_end_cost(ctx.db(), p, true) {
                    Ok(rows) => {
                        self.close_rows = rows;
                        ctx.log("成本", "期末结价", &format!("{}", p.label()));
                        ctx.info("已写入成本调整");
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });

        if self.dirty {
            self.reload_close(ctx);
        }
        let rows = self.close_rows.clone();
        let cols = [
            widgets::TCol::new("存货", 140.0).fixed(),
            widgets::TCol::new("计价方式", 150.0),
            widgets::TCol::new("结存数量", 110.0).right(),
            widgets::TCol::new("结存金额", 130.0).right(),
            widgets::TCol::new("单价", 120.0).right(),
            widgets::TCol::new("调整额", 130.0).right(),
        ];
        widgets::grid(ui, "period_end", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.item).monospace()); }
                1 => { ui.label(&r.method); }
                2 => { ui.label(r.end_qty.fmt_qty()); }
                3 => widgets::amount_label(ui, r.end_amount),
                4 => widgets::amount_label(ui, r.unit_cost),
                5 => {
                    let neg = r.adjust.is_negative();
                    ui.label(
                        RichText::new(r.adjust.fmt_money())
                            .color(if neg { palette::CREDIT } else { palette::OK }),
                    );
                }
                _ => {}
            }
        });
        if !rows.is_empty() {
            let sum: Money = rows.iter().map(|r| r.adjust).sum();
            ui.separator();
            ui.label(RichText::new(format!("调整合计：{}", sum.fmt_money())).strong());
        }
    }

    fn reload_close(&mut self, ctx: &mut AppCtx<'_>) {
        let p = Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period());
        self.close_rows = findb::business::period_end_cost(ctx.db(), p, false).unwrap_or_default();
        self.dirty = false;
    }
}
