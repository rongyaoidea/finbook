//! 采购/销售深度：采购暂估 / 采购对账 / 销售对账 / 供应商配额 / 订单变更
//!
//! 数据层 `findb::scm2` 已就绪，这里只做展示与录入。

use egui::{RichText, Ui};
use findb::scm2::{self, PoEstimate, PoRecon, SoRecon};
use findb::{auxs, business, scm};
use fincore::{AuxKind, Money, Perm, Period};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Estimate,
    PoRecon,
    SoRecon,
    Quota,
    ChangeLog,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Estimate => "采购暂估",
            Tab::PoRecon => "采购对账",
            Tab::SoRecon => "销售对账",
            Tab::Quota => "供应商配额",
            Tab::ChangeLog => "订单变更",
        }
    }
    const ALL: &'static [Tab] = &[
        Tab::Estimate,
        Tab::PoRecon,
        Tab::SoRecon,
        Tab::Quota,
        Tab::ChangeLog,
    ];
}

pub struct ScmDeepView {
    pub tab: Tab,
    pub period_text: String,

    // 采购暂估
    pub pos: Vec<(i64, String, String)>, // (id, no, supplier)
    pub est_po_id: i64,
    pub est_item: String,
    pub est_amount: String,
    pub est_rows: Vec<PoEstimate>,

    // 对账
    pub po_recon: Vec<PoRecon>,
    pub so_recon: Vec<SoRecon>,

    // 配额
    pub quota_supplier: String,
    pub quota_item: String,
    pub quota_qty: String,
    pub quota_remaining: Option<Money>,
    pub suppliers: Vec<String>,
    pub items: Vec<String>,

    // 订单变更
    pub log_type: String,
    pub log_id: String,
    pub log_rows: Vec<(String, String, String, String, String)>,

    pub err: String,
    pub dirty: bool,
    key: String,
}

impl Default for ScmDeepView {
    fn default() -> Self {
        Self {
            tab: Tab::Estimate,
            period_text: String::new(),
            pos: Vec::new(),
            est_po_id: 0,
            est_item: String::new(),
            est_amount: String::new(),
            est_rows: Vec::new(),
            po_recon: Vec::new(),
            so_recon: Vec::new(),
            quota_supplier: String::new(),
            quota_item: String::new(),
            quota_qty: String::new(),
            quota_remaining: None,
            suppliers: Vec::new(),
            items: Vec::new(),
            log_type: "po".to_string(),
            log_id: String::new(),
            log_rows: Vec::new(),
            err: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl ScmDeepView {
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
        let key = format!("{}|{}", p.ymm(), self.tab.name());
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        self.suppliers = auxs::codes(ctx.db(), AuxKind::Supplier).unwrap_or_default();
        self.items = business::stock_items(ctx.db()).unwrap_or_default();

        match self.tab {
            Tab::Estimate => {
                let list = scm::po_list(ctx.db(), p, None).unwrap_or_default();
                self.pos = list
                    .iter()
                    .map(|o| (o.id, o.no.clone(), o.supplier_name.clone()))
                    .collect();
                if self.est_po_id == 0 {
                    if let Some(first) = self.pos.first() {
                        self.est_po_id = first.0;
                    }
                }
                self.reload_estimates(ctx);
            }
            Tab::PoRecon => {
                self.po_recon = scm2::po_reconcile(ctx.db(), p).unwrap_or_default();
            }
            Tab::SoRecon => {
                self.so_recon = scm2::so_reconcile(ctx.db(), p).unwrap_or_default();
            }
            Tab::Quota => {
                if !self.quota_supplier.is_empty() && !self.quota_item.is_empty() {
                    self.quota_remaining = scm2::quota_remaining(
                        ctx.db(),
                        p,
                        &self.quota_supplier,
                        &self.quota_item,
                    )
                    .ok()
                    .flatten();
                }
            }
            Tab::ChangeLog => {}
        }
    }

    fn reload_estimates(&mut self, ctx: &mut AppCtx<'_>) {
        if self.est_po_id == 0 {
            self.est_rows.clear();
            return;
        }
        self.est_rows = scm2::po_estimate_list(ctx.db(), self.est_po_id).unwrap_or_default();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "采购 / 销售深度", |ui| {
            ui.label(RichText::new("采购暂估 / 采购对账 / 销售对账 / 供应商配额 / 订单变更").weak());
        });

        widgets::toolbar(ui, |ui| {
            for t in Tab::ALL {
                ui.selectable_value(&mut self.tab, *t, t.name());
            }
            ui.separator();
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, &self.err);
        }

        match self.tab {
            Tab::Estimate => self.show_estimate(ctx, ui),
            Tab::PoRecon => self.show_po_recon(ui),
            Tab::SoRecon => self.show_so_recon(ui),
            Tab::Quota => self.show_quota(ctx, ui),
            Tab::ChangeLog => self.show_change_log(ctx, ui),
        }
    }

    // ------------------------- 采购暂估 -------------------------
    fn show_estimate(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("采购订单");
            let opts: Vec<String> = self
                .pos
                .iter()
                .map(|(id, no, sup)| format!("#{id} {no} {sup}"))
                .collect();
            let idx = self
                .pos
                .iter()
                .position(|(id, _, _)| *id == self.est_po_id)
                .unwrap_or(0);
            let mut sel = if opts.is_empty() { String::new() } else { opts[idx].clone() };
            if widgets::combo(ui, "scmdeep_po", &mut sel, &opts, 240.0).changed() {
                if let Some(i) = opts.iter().position(|o| *o == sel) {
                    self.est_po_id = self.pos[i].0;
                    self.reload_estimates(ctx);
                }
            }
        });

        widgets::card(ui, "新增暂估", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("存货");
                widgets::combo(ui, "scmdeep_est_item", &mut self.est_item, &self.items, 160.0);
                ui.label("暂估金额");
                widgets::money_input(ui, &mut self.est_amount, 120.0);
                if ui.button("登记暂估").clicked() && ctx.can(Perm::AccountEdit) {
                    self.do_add_estimate(ctx);
                }
            });
        });

        ui.add_space(6.0);
        let open_sum = self
            .est_rows
            .iter()
            .filter(|e| !e.settled)
            .map(|e| e.est_amount)
            .sum::<Money>();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("未冲回暂估合计：{}", open_sum.fmt_money())).strong());
        });

        let rows = self.est_rows.clone();
        let cols = [
            widgets::TCol::new("#", 60.0).fixed(),
            widgets::TCol::new("存货", 160.0),
            widgets::TCol::new("暂估金额", 130.0).right(),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("操作", 100.0).fixed(),
        ];
        let mut settle: Option<i64> = None;
        widgets::grid(ui, "scmdeep_est", &cols, rows.len(), 24.0, |i, c, ui| {
            let e = &rows[i];
            match c {
                0 => { ui.label(e.id.to_string()); }
                1 => { ui.label(&e.item); }
                2 => widgets::amount_label(ui, e.est_amount),
                3 => {
                    ui.label(
                        RichText::new(if e.settled { "已冲回" } else { "未冲回" })
                            .color(if e.settled { palette::OK } else { palette::WARN }),
                    );
                }
                4 => {
                    if !e.settled && ui.small_button("冲回").clicked() {
                        settle = Some(e.id);
                    }
                }
                _ => {}
            }
        });
        if let Some(id) = settle {
            match scm2::po_estimate_settle(ctx.db(), id) {
                Ok(()) => {
                    ctx.log("采购深度", "暂估冲回", &format!("#{id}"));
                    ctx.info("已冲回暂估");
                    self.reload_estimates(ctx);
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn do_add_estimate(&mut self, ctx: &mut AppCtx<'_>) {
        self.err.clear();
        if self.est_po_id == 0 {
            self.err = "请先选择采购订单".to_string();
            return;
        }
        if self.est_item.trim().is_empty() {
            self.err = "请选择存货".to_string();
            return;
        }
        let amount = Money::parse_or_zero(&self.est_amount);
        if amount.is_zero() {
            self.err = "请填写暂估金额".to_string();
            return;
        }
        let p = self.period(ctx);
        match scm2::po_estimate_add(ctx.db(), self.est_po_id, p, &self.est_item, amount) {
            Ok(id) => {
                ctx.log("采购深度", "登记暂估", &format!("#{id} {amount}"));
                ctx.info("已登记暂估");
                self.est_amount.clear();
                self.reload_estimates(ctx);
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    // ------------------------- 对账 -------------------------
    fn show_po_recon(&self, ui: &mut Ui) {
        let rows = self.po_recon.clone();
        let cols = [
            widgets::TCol::new("订单号", 130.0),
            widgets::TCol::new("供应商", 160.0),
            widgets::TCol::new("订单金额", 120.0).right(),
            widgets::TCol::new("已付款", 120.0).right(),
            widgets::TCol::new("未付款", 120.0).right(),
            widgets::TCol::new("未冲回暂估", 120.0).right(),
        ];
        widgets::grid(ui, "scmdeep_po_recon", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.no).monospace()); }
                1 => { ui.label(&r.supplier); }
                2 => widgets::amount_label(ui, r.order_amount),
                3 => widgets::amount_label(ui, r.paid),
                4 => widgets::amount_label(ui, r.unpaid),
                5 => widgets::amount_label(ui, r.open_estimate),
                _ => {}
            }
        });
    }

    fn show_so_recon(&self, ui: &mut Ui) {
        let rows = self.so_recon.clone();
        let cols = [
            widgets::TCol::new("订单号", 130.0),
            widgets::TCol::new("客户", 160.0),
            widgets::TCol::new("订单金额", 120.0).right(),
            widgets::TCol::new("已收款", 120.0).right(),
            widgets::TCol::new("未收款", 120.0).right(),
        ];
        widgets::grid(ui, "scmdeep_so_recon", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.no).monospace()); }
                1 => { ui.label(&r.customer); }
                2 => widgets::amount_label(ui, r.order_amount),
                3 => widgets::amount_label(ui, r.received),
                4 => widgets::amount_label(ui, r.unreceived),
                _ => {}
            }
        });
    }

    // ------------------------- 配额 -------------------------
    fn show_quota(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::card(ui, "供应商配额设置", |ui| {
            egui::Grid::new("scmdeep_quota_form")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("供应商");
                    widgets::combo(ui, "scmdeep_quota_sup", &mut self.quota_supplier, &self.suppliers, 180.0);
                    ui.end_row();
                    ui.label("物料");
                    widgets::combo(ui, "scmdeep_quota_item", &mut self.quota_item, &self.items, 180.0);
                    ui.end_row();
                    ui.label("配额数量");
                    widgets::money_input(ui, &mut self.quota_qty, 120.0);
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("保存配额").clicked() && ctx.can(Perm::AccountEdit) {
                    self.do_set_quota(ctx);
                }
                if ui.button("查询剩余配额").clicked() {
                    self.dirty = true;
                }
            });
            if let Some(rem) = self.quota_remaining {
                ui.label(RichText::new(format!("剩余配额：{}", rem.fmt_qty())).strong());
            }
        });
    }

    fn do_set_quota(&mut self, ctx: &mut AppCtx<'_>) {
        self.err.clear();
        if self.quota_supplier.trim().is_empty() || self.quota_item.trim().is_empty() {
            self.err = "请选择供应商与物料".to_string();
            return;
        }
        let qty = Money::parse_or_zero(&self.quota_qty);
        if qty.is_negative() {
            self.err = "配额不能为负".to_string();
            return;
        }
        let p = self.period(ctx);
        match scm2::quota_set(ctx.db(), p, &self.quota_supplier, &self.quota_item, qty) {
            Ok(()) => {
                ctx.log("采购深度", "供应商配额", &format!("{} {} {}", self.quota_supplier, self.quota_item, qty.fmt_qty()));
                ctx.info("已保存配额");
                self.quota_remaining = Some(qty);
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    // ------------------------- 订单变更 -------------------------
    fn show_change_log(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("订单类型");
            let types = vec!["po".to_string(), "so".to_string()];
            let labels = vec!["采购订单".to_string(), "销售订单".to_string()];
            let idx = types.iter().position(|t| *t == self.log_type).unwrap_or(0);
            let mut sel = labels[idx].clone();
            if widgets::combo(ui, "scmdeep_log_type", &mut sel, &labels, 120.0).changed() {
                if let Some(i) = labels.iter().position(|l| *l == sel) {
                    self.log_type = types[i].clone();
                }
            }
            ui.label("订单ID");
            widgets::text_input(ui, &mut self.log_id, 90.0, "如 1");
            if ui.button("查询").clicked() {
                self.log_rows = match self.log_id.trim().parse::<i64>() {
                    Ok(id) => scm2::change_log_list(ctx.db(), &self.log_type, id).unwrap_or_default(),
                    Err(_) => {
                        self.err = "订单ID必须是数字".to_string();
                        Vec::new()
                    }
                };
            }
        });

        let rows = self.log_rows.clone();
        let cols = [
            widgets::TCol::new("字段", 120.0),
            widgets::TCol::new("旧值", 160.0),
            widgets::TCol::new("新值", 160.0),
            widgets::TCol::new("操作人", 120.0),
            widgets::TCol::new("时间", 170.0),
        ];
        widgets::grid(ui, "scmdeep_change_log", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(&r.0); }
                1 => { ui.label(&r.1); }
                2 => { ui.label(&r.2); }
                3 => { ui.label(&r.3); }
                4 => { ui.label(&r.4); }
                _ => {}
            }
        });
    }
}
