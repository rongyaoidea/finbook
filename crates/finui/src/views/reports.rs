//! 会计报表：资产负债表 / 利润表 / 现金流量表

use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use findb::balances::{BalanceQuery, BalanceSnapshot};
use fincore::report::cashflow::CashFlowStatement;
use fincore::report::{AmountKind, LineStyle, ReportTable};
use fincore::{Money, Period};

use crate::state::AppCtx;
use crate::theme;
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Balance,
    Income,
    CashFlow,
}

pub struct ReportsView {
    pub tab: Tab,
    pub from: String,
    pub to: String,
    pub table: Option<ReportTable>,
    pub cf: Option<CashFlowStatement>,
    pub err: Option<String>,
    pub dirty: bool,
    key: String,
}

impl Default for ReportsView {
    fn default() -> Self {
        Self {
            tab: Tab::Balance,
            from: String::new(),
            to: String::new(),
            table: None,
            cf: None,
            err: None,
            dirty: true,
            key: String::new(),
        }
    }
}

impl ReportsView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        let p = ctx.period();
        if self.from.is_empty() {
            self.from = p.code();
        }
        if self.to.is_empty() {
            self.to = p.code();
        }
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let key = format!("{}|{}|{:?}", self.from, self.to, self.tab);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        self.err = None;

        let from = Period::parse(&self.from).unwrap_or_else(|_| ctx.period());
        let to = Period::parse(&self.to).unwrap_or_else(|_| ctx.period());

        match self.tab {
            Tab::CashFlow => {
                self.table = None;
                match findb::reports::cash_flow_statement(ctx.db(), from, to) {
                    Ok(s) => self.cf = Some(s),
                    Err(e) => self.err = Some(e.to_string()),
                }
            }
            _ => {
                self.cf = None;
                let key_def = if self.tab == Tab::Balance {
                    "balance_sheet"
                } else {
                    "income_statement"
                };
                let def = match findb::reports::get_def(ctx.db(), key_def) {
                    Ok(Some(d)) => d,
                    Ok(None) => {
                        if self.tab == Tab::Balance {
                            fincore::report::balance_sheet::balance_sheet_def()
                        } else {
                            fincore::report::income::income_statement_def()
                        }
                    }
                    Err(e) => {
                        self.err = Some(e.to_string());
                        return;
                    }
                };
                let snap = match BalanceSnapshot::load(ctx.db(), &BalanceQuery::range(from, to)) {
                    Ok(s) => s,
                    Err(e) => {
                        self.err = Some(e.to_string());
                        return;
                    }
                };
                let company = ctx.db().options().company;
                let subtitle = format!("{} 至 {}", from.label(), to.label());
                let table = if self.tab == Tab::Balance {
                    fincore::report::render(
                        &def,
                        &snap,
                        &company,
                        &subtitle,
                        vec![
                            Box::new(fincore::report::identity),
                            Box::new(fincore::report::to_begin),
                        ],
                    )
                } else {
                    fincore::report::render(
                        &def,
                        &snap,
                        &company,
                        &subtitle,
                        vec![
                            Box::new(fincore::report::identity),
                            Box::new(to_ytd),
                        ],
                    )
                };
                self.table = Some(table);
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "会计报表", |ui| {
            ui.label(RichText::new(ctx.db().options().company).weak());
        });

        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, Tab::Balance, "资产负债表");
            ui.selectable_value(&mut self.tab, Tab::Income, "利润表");
            ui.selectable_value(&mut self.tab, Tab::CashFlow, "现金流量表");
            ui.separator();
            ui.label("期间");
            ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.from));
            ui.label("—");
            ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.to));
            if ui.button("查询").clicked() {
                self.dirty = true;
            }
            if ui.button("本期").clicked() {
                let p = ctx.period();
                self.from = p.code();
                self.to = p.code();
                self.dirty = true;
            }
            if ui.button("本年").clicked() {
                let p = ctx.period();
                self.from = Period::new(p.year(), 1).map(|x| x.code()).unwrap_or(p.code());
                self.to = p.code();
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
            if self.tab != Tab::CashFlow && ui.button("恢复内置模板").clicked() {
                let key = if self.tab == Tab::Balance {
                    "balance_sheet"
                } else {
                    "income_statement"
                };
                let r = findb::reports::reset_def(ctx.db(), key);
                if ctx.handle(r).is_some() {
                    ctx.info("已恢复内置模板");
                    self.dirty = true;
                }
            }
        });

        if let Some(e) = &self.err {
            ui.colored_label(palette::CREDIT, e);
            return;
        }

        egui::ScrollArea::both().show(ui, |ui| {
            match self.tab {
                Tab::CashFlow => {
                    if let Some(cf) = &self.cf {
                        cash_flow_table(ui, cf, &ctx.db().options().company, &self.subtitle());
                    }
                }
                _ => {
                    if let Some(t) = &self.table {
                        report_table(ui, t);
                    }
                }
            }
        });
    }

    fn subtitle(&self) -> String {
        format!("{} 至 {}", self.from, self.to)
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let sh = match self.tab {
            Tab::CashFlow => match &self.cf {
                None => {
                    ctx.error("没有可导出的数据");
                    return;
                }
                Some(cf) => cash_flow_sheet(cf),
            },
            _ => match &self.table {
                None => {
                    ctx.error("没有可导出的数据");
                    return;
                }
                Some(t) => {
                    let mut headers = vec!["项目".to_string(), "行次".to_string()];
                    headers.extend(t.columns.iter().cloned());
                    let mut sh = crate::views::export::Sheet::new(&t.title, headers);
                    for r in &t.rows {
                        let mut row = vec![
                            format!("{}{}", "　".repeat(r.indent as usize), r.name),
                            r.no.clone(),
                        ];
                        row.extend(r.values.iter().map(|v| v.fmt_plain()));
                        sh.push(row);
                    }
                    sh
                }
            },
        };
        let name = match self.tab {
            Tab::Balance => "资产负债表",
            Tab::Income => "利润表",
            Tab::CashFlow => "现金流量表",
        };
        let file_name = format!("{name}_{}", self.to);
        let title = format!("{name}（{}）", self.to);
        match crate::views::export::run_export(&sh, &file_name, &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}

fn to_ytd(k: AmountKind) -> AmountKind {
    match k {
        AmountKind::PeriodDebit => AmountKind::YearDebit,
        AmountKind::PeriodCredit => AmountKind::YearCredit,
        other => other,
    }
}

/// 报表表格渲染
pub fn report_table(ui: &mut Ui, t: &ReportTable) {
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(&t.title).size(19.0).strong());
    });
    ui.horizontal(|ui| {
        ui.label(format!("编制单位：{}", t.company));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(&t.subtitle).weak());
            ui.label(RichText::new("单位：元").weak());
        });
    });
    ui.separator();

    let n = t.rows.len();
    let mut tb = TableBuilder::new(ui)
        .id_salt("report_table")
        .striped(false)
        .resizable(true)
        .min_scrolled_height(200.0)
        .column(Column::remainder().at_least(260.0))
        .column(Column::initial(46.0).resizable(false));
    for _ in 0..t.columns.len() {
        tb = tb.column(Column::initial(150.0).at_least(110.0));
    }

    tb.header(26.0, |mut h| {
        h.col(|ui| {
            ui.label(RichText::new("项　目").strong());
        });
        h.col(|ui| {
            ui.label(RichText::new("行次").strong());
        });
        for c in &t.columns {
            h.col(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(c).strong());
                });
            });
        }
    })
    .body(|body| {
        body.rows(24.0, n, |mut row| {
            let i = row.index();
            let r = &t.rows[i];
            let strong = matches!(r.style, LineStyle::Total | LineStyle::Subtotal);

            row.col(|ui| {
                let indent = "　".repeat(r.indent as usize);
                let txt = if strong { RichText::new(format!("{indent}{}", r.name)).strong() } else { RichText::new(format!("{indent}{}", r.name)) };
                if matches!(r.style, LineStyle::Header) {
                    ui.label(txt.color(palette::PRIMARY));
                } else {
                    ui.label(txt);
                }
            });
            row.col(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&r.no).weak());
                });
            });
            for ci in 0..t.columns.len() {
                row.col(|ui| {
                    let v = r.values.get(ci).copied().unwrap_or(Money::ZERO);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if r.style == LineStyle::Blank || r.style == LineStyle::Header {
                            return;
                        }
                        if v.is_zero() {
                            ui.label(RichText::new("").weak());
                        } else {
                            let red = v.is_negative() && r.show_negative_red;
                            let mut txt = RichText::new(v.fmt_money());
                            if strong {
                                txt = txt.strong();
                            }
                            ui.label(txt.color(if red {
                                palette::CREDIT
                            } else {
                                theme::amount_color(v)
                            }));
                        }
                    });
                });
            }
        });
    });
}

/// 现金流量表渲染
pub fn cash_flow_table(ui: &mut Ui, cf: &CashFlowStatement, company: &str, subtitle: &str) {
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("现金流量表").size(19.0).strong());
    });
    ui.horizontal(|ui| {
        ui.label(format!("编制单位：{company}"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(subtitle).weak());
            ui.label(RichText::new("单位：元").weak());
        });
    });
    ui.separator();

    let cols = [
        crate::widgets::TCol::new("项　目", 380.0),
        crate::widgets::TCol::new("行次", 50.0).fixed(),
        crate::widgets::TCol::new("流入金额", 150.0).right(),
        crate::widgets::TCol::new("流出金额", 150.0).right(),
        crate::widgets::TCol::new("净额", 150.0).right(),
    ];

    // 组装成行
    struct Line {
        name: String,
        indent: u8,
        inflow: Money,
        outflow: Money,
        net: Money,
        strong: bool,
    }
    let mut lines: Vec<Line> = Vec::new();
    let push = |lines: &mut Vec<Line>,
                name: &str,
                indent: u8,
                inflow: Money,
                outflow: Money,
                net: Money,
                strong: bool| {
        lines.push(Line {
            name: name.to_string(),
            indent,
            inflow,
            outflow,
            net,
            strong,
        });
    };

    push(&mut lines, "一、经营活动产生的现金流量", 0, Money::ZERO, Money::ZERO, Money::ZERO, true);
    for l in &cf.operating {
        push(&mut lines, &l.name, 1, l.inflow, l.outflow, l.net, false);
    }
    push(
        &mut lines,
        "　　经营活动产生的现金流量净额",
        0,
        Money::ZERO,
        Money::ZERO,
        cf.operating_net,
        true,
    );
    push(&mut lines, "二、投资活动产生的现金流量", 0, Money::ZERO, Money::ZERO, Money::ZERO, true);
    for l in &cf.investing {
        push(&mut lines, &l.name, 1, l.inflow, l.outflow, l.net, false);
    }
    push(
        &mut lines,
        "　　投资活动产生的现金流量净额",
        0,
        Money::ZERO,
        Money::ZERO,
        cf.investing_net,
        true,
    );
    push(&mut lines, "三、筹资活动产生的现金流量", 0, Money::ZERO, Money::ZERO, Money::ZERO, true);
    for l in &cf.financing {
        push(&mut lines, &l.name, 1, l.inflow, l.outflow, l.net, false);
    }
    push(
        &mut lines,
        "　　筹资活动产生的现金流量净额",
        0,
        Money::ZERO,
        Money::ZERO,
        cf.financing_net,
        true,
    );
    push(
        &mut lines,
        "四、现金及现金等价物净增加额",
        0,
        Money::ZERO,
        Money::ZERO,
        cf.net_increase,
        true,
    );
    push(&mut lines, "　　加：期初现金及现金等价物余额", 0, Money::ZERO, Money::ZERO, cf.begin_cash, false);
    push(&mut lines, "五、期末现金及现金等价物余额", 0, Money::ZERO, Money::ZERO, cf.end_cash, true);

    let n = lines.len();
    crate::widgets::grid(ui, "cash_flow", &cols, n, 24.0, |i, c, ui| {
        let l = &lines[i];
        match c {
            0 => {
                let indent = "　".repeat(l.indent as usize);
                ui.label(if l.strong { RichText::new(format!("{indent}{}", l.name)).strong() } else { RichText::new(format!("{indent}{}", l.name)) });
            }
            1 => {
                ui.label(RichText::new(format!("{}", i + 1)).weak());
            }
            2 => { crate::widgets::amount_label(ui, l.inflow); }
            3 => { crate::widgets::amount_label(ui, l.outflow); }
            4 => {
                let v = l.net;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut txt = RichText::new(v.fmt_money());
                    if l.strong {
                        txt = txt.strong();
                    }
                    ui.label(txt.color(if v.is_negative() {
                        palette::CREDIT
                    } else {
                        Color32::BLACK
                    }));
                });
            }
            _ => {}
        }
    });

    ui.separator();
    if cf.unassigned.is_zero() {
        ui.label(RichText::new("✔ 现金收支已全部标注现金流量项目").color(palette::OK));
    } else {
        ui.colored_label(
            palette::WARN,
            format!(
                "⚠ 尚有 {} 的现金收支未标注现金流量项目，请在凭证中补录",
                cf.unassigned.fmt_money()
            ),
        );
    }
    if cf.ties() {
        ui.label(
            RichText::new("✔ 净增加额与货币资金期末减期初一致，勾稽通过")
                .color(palette::OK),
        );
    } else {
        ui.colored_label(
            palette::CREDIT,
            format!(
                "✖ 勾稽不符：净增加额 {} ≠ 期末 {} − 期初 {}（差 {}）",
                cf.net_increase.fmt_money(),
                cf.end_cash.fmt_money(),
                cf.begin_cash.fmt_money(),
                (cf.net_increase - (cf.end_cash - cf.begin_cash)).fmt_money()
            ),
        );
    }
}

pub fn cash_flow_sheet(cf: &CashFlowStatement) -> crate::views::export::Sheet {
    let mut sh = crate::views::export::Sheet::new(
        "现金流量表",
        vec![
            "项目".into(),
            "流入金额".into(),
            "流出金额".into(),
            "净额".into(),
        ],
    );
    let add = |sh: &mut crate::views::export::Sheet,
                   name: &str,
                   i: Money,
                   o: Money,
                   n: Money| {
        sh.push(vec![name.to_string(), i.fmt_plain(), o.fmt_plain(), n.fmt_plain()]);
    };
    add(&mut sh, "一、经营活动产生的现金流量", Money::ZERO, Money::ZERO, Money::ZERO);
    for l in &cf.operating {
        add(&mut sh, &l.name, l.inflow, l.outflow, l.net);
    }
    add(
        &mut sh,
        "经营活动产生的现金流量净额",
        Money::ZERO,
        Money::ZERO,
        cf.operating_net,
    );
    add(&mut sh, "二、投资活动产生的现金流量", Money::ZERO, Money::ZERO, Money::ZERO);
    for l in &cf.investing {
        add(&mut sh, &l.name, l.inflow, l.outflow, l.net);
    }
    add(
        &mut sh,
        "投资活动产生的现金流量净额",
        Money::ZERO,
        Money::ZERO,
        cf.investing_net,
    );
    add(&mut sh, "三、筹资活动产生的现金流量", Money::ZERO, Money::ZERO, Money::ZERO);
    for l in &cf.financing {
        add(&mut sh, &l.name, l.inflow, l.outflow, l.net);
    }
    add(
        &mut sh,
        "筹资活动产生的现金流量净额",
        Money::ZERO,
        Money::ZERO,
        cf.financing_net,
    );
    add(
        &mut sh,
        "四、现金及现金等价物净增加额",
        Money::ZERO,
        Money::ZERO,
        cf.net_increase,
    );
    add(&mut sh, "加：期初现金及现金等价物余额", Money::ZERO, Money::ZERO, cf.begin_cash);
    add(&mut sh, "五、期末现金及现金等价物余额", Money::ZERO, Money::ZERO, cf.end_cash);
    sh
}
