//! 用户与权限

use egui::{Color32, RichText, Ui};
use fincore::{Perm, Role, User};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

pub struct UsersView {
    pub rows: Vec<User>,
    pub dirty: bool,
    pub editing: Option<User>,
    pub editing_new: bool,
    pub pwd: String,
    pub err: String,
    /// 查看某角色的权限矩阵
    pub show_perms: Option<Role>,
}

impl Default for UsersView {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            dirty: true,
            editing: None,
            editing_new: false,
            pwd: String::new(),
            err: String::new(),
            show_perms: None,
        }
    }
}

impl UsersView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.rows = findb::users::list(ctx.db()).unwrap_or_default();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "用户与权限", |ui| {
            ui.label(RichText::new(format!("共 {} 个用户", self.rows.len())).weak());
        });

        widgets::toolbar(ui, |ui| {
            if ui.button("新增用户").clicked() {
                self.editing = Some(User::new("", "", Role::Accountant));
                self.editing_new = true;
                self.pwd.clear();
                self.err.clear();
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            ui.label("角色权限：");
            for r in Role::all() {
                if ui.button(r.label()).clicked() {
                    self.show_perms = Some(*r);
                }
            }
        });

        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("用户名", 120.0).fixed(),
            widgets::TCol::new("姓名", 120.0).fixed(),
            widgets::TCol::new("角色", 100.0).fixed(),
            widgets::TCol::new("状态", 60.0).fixed(),
            widgets::TCol::new("备注", 260.0),
            widgets::TCol::new("操作", 130.0).fixed(),
        ];
        let me = ctx.user().username.clone();
        let mut del: Option<i64> = None;
        widgets::grid(ui, "user_list", &cols, rows.len(), 24.0, |i, c, ui| {
            let u = &rows[i];
            match c {
                0 => {
                    let mut t = RichText::new(&u.username).monospace();
                    if u.username == me {
                        t = t.strong();
                    }
                    ui.label(t);
                }
                1 => { ui.label(&u.display_name); }
                2 => { ui.label(u.role.label()); }
                3 => {
                    ui.label(if u.disabled {
                        RichText::new("停用").color(palette::CREDIT)
                    } else {
                        RichText::new("启用").color(palette::OK)
                    });
                }
                4 => { ui.label(RichText::new(&u.memo).weak()); }
                5 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改").clicked() {
                            self.editing = Some(u.clone());
                            self.editing_new = false;
                            self.pwd.clear();
                            self.err.clear();
                        }
                        if ui.small_button("删").clicked() {
                            del = Some(u.id);
                        }
                    });
                }
                _ => {}
            }
        });
        if let Some(id) = del {
            ctx.confirm_dangerous(
                "删除用户",
                "删除后该用户无法登录，其历史操作日志保留。确定删除吗？",
                ConfirmAction::DeleteUser(id),
                true,
            );
        }

        self.edit_window(ctx, ui);
        self.perm_window(ui);
    }

    fn edit_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(u) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;

        egui::Window::new(if is_new { "新增用户" } else { "修改用户" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.err);
                }
                egui::Grid::new("user_edit")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("用户名：");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut u.username));
                        ui.end_row();
                        ui.label("姓名：");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut u.display_name));
                        ui.end_row();
                        ui.label("角色：");
                        egui::ComboBox::from_id_salt("user_role")
                            .selected_text(u.role.label())
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for r in Role::all() {
                                    ui.selectable_value(&mut u.role, *r, r.label());
                                }
                            });
                        ui.end_row();
                        ui.label(if is_new { "初始密码：" } else { "重置密码：" });
                        ui.add_sized(
                            [220.0, 22.0],
                            egui::TextEdit::singleline(&mut self.pwd)
                                .password(true)
                                .hint_text("留空表示不修改"),
                        );
                        ui.end_row();
                        ui.label("备注：");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut u.memo));
                        ui.end_row();
                    });
                ui.checkbox(&mut u.disabled, "停用该用户");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui.button("保存").clicked() {
                            save = true;
                        }
                    });
                });
            });

        if close || !open {
            self.editing = None;
            self.err.clear();
            return;
        }
        if save {
            let mut u = self.editing.clone().unwrap();
            self.err.clear();
            if u.username.trim().is_empty() {
                self.err = "用户名不能为空".to_string();
                return;
            }
            if is_new {
                let pwd = if self.pwd.is_empty() {
                    "123456".to_string()
                } else {
                    self.pwd.clone()
                };
                u.set_password(&pwd);
                // 普通账户默认只能看自己填制的凭证；管理员可看全部
                if !u.is_admin() {
                    u.data_scope.own_voucher_only = true;
                }
                match findb::users::insert(ctx.db(), &u) {
                    Ok(_) => {
                        ctx.log("用户", "新增用户", &u.username);
                        ctx.info("已新增用户");
                        self.dirty = true;
                        self.editing = None;
                    }
                    Err(e) => self.err = e.to_string(),
                }
            } else {
                match findb::users::update(ctx.db(), &u) {
                    Ok(()) => {
                        if !self.pwd.is_empty() {
                            if let Err(e) =
                                findb::users::reset_password(ctx.db(), &u.username, &self.pwd)
                            {
                                self.err = e.to_string();
                                return;
                            }
                        }
                        ctx.log("用户", "修改用户", &u.username);
                        ctx.info("已保存");
                        self.dirty = true;
                        self.editing = None;
                    }
                    Err(e) => self.err = e.to_string(),
                }
            }
        }
    }

    fn perm_window(&mut self, ui: &mut Ui) {
        let Some(r) = self.show_perms else { return };
        let mut open = true;
        egui::Window::new(format!("角色权限 — {}", r.label()))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                let owned: Vec<Perm> = r.perms().to_vec();
                ui.horizontal_wrapped(|ui| {
                    for p in Perm::all() {
                        let on = owned.contains(p);
                        let txt = if on {
                            format!("✔ {}", p.label())
                        } else {
                            format!("✖ {}", p.label())
                        };
                        ui.label(
                            RichText::new(txt).color(if on {
                                Color32::BLACK
                            } else {
                                ui.visuals().weak_text_color()
                            }),
                        );
                        ui.add_space(6.0);
                    }
                });
            });
        if !open {
            self.show_perms = None;
        }
    }
}
