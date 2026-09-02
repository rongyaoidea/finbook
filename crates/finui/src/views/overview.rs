//! 管理员 · 账目总览（只读视角）
//!
//! 管理员的职责是查看账目全貌而不是记账：本页把资产 / 负债 / 权益 / 损益、
//! 凭证与发票概况、最近凭证集中在一页，只读展示，不做任何录入。

use egui::{RichText, Ui};
use findb::reports::Overview;
use fincore::{Money, Period};

use crate::state::AppCtx;
use crate::theme;
use crate::widgets;

pub struct OverviewView {
    pub period_text: String,
    data: Option<Overview>,
    dirty: bool,
    key: String,
}

impl Default for OverviewView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            data: None,
            dirty: true,
            key: String::new(),
        }
    }
}

impl OverviewView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        self.dirty = true;
    }

    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}|{}", p.ymm(), ctx.db().path().display());
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        match findb::reports::overview(ctx.db(), p) {
            Ok(o) => self.data = Some(o),
            Err(e) => {
                ctx.error(e.to_string());
                self.data = None;
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        widgets::page_header(ui, "账目总览", |ui| {
            ui.label(RichText::new("管理员只读视角 · 账目全貌").weak());
        });
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        let Some(o) = self.data.clone() else {
            return;
        };
        let t = &o.totals;

        // ---------------- 财务概况 ----------------
        widgets::card(ui, "财务概况（年初至今）", |ui| {
            ui.columns(4, |cols| {
                stat(&mut cols[0], "资产总额", &t.total_asset);
                stat(&mut cols[1], "负债总额", &t.total_liab);
                stat(&mut cols[2], "所有者权益", &t.equity);
                stat(&mut cols[3], "净利润", &t.net_profit);
            });
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                stat(&mut cols[0], "营业收入", &t.revenue);
                stat(&mut cols[1], "营业成本", &t.cost);
            });
        });

        // ---------------- 凭证 / 发票概况 ----------------
        widgets::card(ui, "业务概况", |ui| {
            ui.horizontal_wrapped(|ui| {
                kv(ui, "公司", &o.company);
                kv(ui, "已结账至", &o
                    .closed_upto
                    .map(|p| p.label())
                    .unwrap_or_else(|| "未结账".to_string()));
            });
            ui.horizontal_wrapped(|ui| {
                kv(ui, "凭证总数（全账套）", &o.vouchers.to_string());
                kv(ui, "分录总数（全账套）", &o.entries.to_string());
                kv(ui, "科目数（全账套）", &o.accounts.to_string());
            });
            ui.horizontal_wrapped(|ui| {
                kv(ui, "当期未记账", &o.unposted.to_string());
                kv(ui, "当期已记账", &o.posted.to_string());
                kv(
                    ui,
                    "进项发票价税合计",
                    &format!("{}（{} 张）", o.invoice_in.0.fmt_money(), o.invoice_in.1),
                );
                kv(
                    ui,
                    "销项发票价税合计",
                    &format!("{}（{} 张）", o.invoice_out.0.fmt_money(), o.invoice_out.1),
                );
            });
        });

        // ---------------- 最近凭证 ----------------
        ui.add_space(6.0);
        ui.label(RichText::new("最近凭证（全账套，只读）").strong());
        let rows = o.recent.len();
        let cols = [
            widgets::TCol::new("期间", 76.0).fixed(),
            widgets::TCol::new("日期", 88.0).fixed(),
            widgets::TCol::new("凭证号", 88.0).fixed(),
            widgets::TCol::new("摘要", 280.0),
            widgets::TCol::new("借方", 110.0).right(),
            widgets::TCol::new("贷方", 110.0).right(),
            widgets::TCol::new("状态", 70.0).fixed(),
            widgets::TCol::new("制单", 80.0).fixed(),
        ];
        widgets::grid(ui, "overview_recent", &cols, rows, 24.0, |i, c, ui| {
            let v = &o.recent[i];
            match c {
                0 => { ui.label(p_of(v).code()); }
                1 => { ui.label(v.date.format("%Y-%m-%d").to_string()); }
                2 => { ui.label(RichText::new(v.voucher_no()).monospace()); }
                3 => { ui.label(v.first_summary()); }
                4 => { widgets::amount_label(ui, v.debit_total()); }
                5 => { widgets::amount_label(ui, v.credit_total()); }
                6 => {
                    ui.label(
                        RichText::new(v.status.label())
                            .color(theme::status_color(v.status.counts())),
                    );
                }
                7 => { ui.label(&v.prepared_by); }
                _ => {}
            }
        });
    }
}

fn p_of(v: &fincore::Voucher) -> Period {
    v.period
}

fn stat(ui: &mut Ui, label: &str, v: &Money) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).weak().size(12.0));
        ui.label(RichText::new(v.fmt_money()).size(17.0).strong());
    });
}

fn kv(ui: &mut Ui, label: &str, v: &str) {
    ui.label(RichText::new(format!("{label}：")).weak());
    ui.label(RichText::new(v).strong());
    ui.add_space(10.0);
}
