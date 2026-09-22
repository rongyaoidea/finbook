// FinBook Web 前端（原生 JS SPA，无框架依赖）
const API = "/api";

let session = { user: null };
let state = { view: "dashboard", periods: [], current: null, accounts: null, users: null, bookKey: "" };

// ---------- 工具 ----------
// esc / fmt / fmtMoney / ymm 抽到 util.js（无 DOM 依赖，可单测），此处经全局复用。
function $(sel, root) { return (root || document).querySelector(sel); }
function $all(sel, root) { return Array.from((root || document).querySelectorAll(sel)); }

// 请求竞态防护：每个视图一个请求序号，返回时才采纳最新一次的结果
const reqSeq = { v: 0 };
function nextReq(scope) { reqSeq[scope] = (reqSeq[scope] || 0) + 1; return reqSeq[scope]; }
function staleReq(scope, id) { return reqSeq[scope] !== id; }

async function api(path, opts = {}) {
  const r = await fetch(API + path, Object.assign({ credentials: "same-origin" }, opts));
  let data = null;
  try { data = await r.json(); } catch (e) {}
  if (r.status === 401) {
    if (path !== "/setup/status" && path !== "/login") { session.user = null; render(); }
    throw new Error((data && data.error) || "未登录");
  }
  if (!r.ok) throw new Error((data && data.error) || ("请求失败 " + r.status));
  return data;
}

const MAX_TOASTS = 5;
function toast(msg, kind) {
  const wrap = document.getElementById("toast");
  // 上限：挤掉最旧
  while (wrap.children.length >= MAX_TOASTS) wrap.removeChild(wrap.firstChild);
  const t = document.createElement("div");
  t.className = "toast " + (kind || "");
  t.textContent = msg;
  wrap.appendChild(t);
  requestAnimationFrame(() => t.classList.add("show"));
  setTimeout(() => { t.classList.remove("show"); setTimeout(() => t.remove(), 250); }, 2600);
}

let modalStack = [];
function modal(html, wide) {
  const root = document.getElementById("modal-root");
  const mask = document.createElement("div");
  mask.className = "modal-mask";
  mask.innerHTML = `<div class="modal ${wide ? "wide" : ""}">${html}</div>`;
  root.appendChild(mask);
  mask.addEventListener("click", (e) => { if (e.target === mask) closeModal(); });
  modalStack.push(mask);
  // 焦点移入弹窗首个可聚焦元素
  const first = mask.querySelector("input, select, textarea, button");
  if (first) first.focus();
  return mask;
}
function closeModal() {
  const root = document.getElementById("modal-root");
  root.innerHTML = "";
  modalStack = [];
}
// 确认对话框（Promise 化，替代浏览器原生 confirm）
// 自包含：只关闭自身遮罩，不影响下方已打开的其它弹窗（如凭证编辑器）
function confirmDialog(message, danger) {
  return new Promise((resolve) => {
    const root = document.getElementById("modal-root");
    const mask = document.createElement("div");
    mask.className = "modal-mask";
    mask.innerHTML = `<div class="modal">
      <h3>确认操作</h3>
      <p class="muted" style="margin:0 0 16px;line-height:1.6">${esc(message)}</p>
      <div class="foot">
        <button class="btn ghost" id="cf-cancel">取消</button>
        <button class="btn ${danger ? "danger" : "primary"}" id="cf-ok">确定</button>
      </div>
    </div>`;
    root.appendChild(mask);
    const done = (val) => {
      document.removeEventListener("keydown", onKey, true);
      mask.remove();
      resolve(val);
    };
    const onKey = (e) => {
      // 捕获阶段拦截并阻止冒泡，避免触发全局 Esc 关闭其它弹窗
      if (e.key === "Escape") { e.stopPropagation(); done(false); }
    };
    document.addEventListener("keydown", onKey, true);
    mask.addEventListener("click", (e) => { if (e.target === mask) done(false); });
    $("#cf-ok", mask).onclick = () => done(true);
    $("#cf-cancel", mask).onclick = () => done(false);
    // 危险操作默认聚焦「取消」，防止回车误删；普通操作聚焦「确定」
    (danger ? $("#cf-cancel", mask) : $("#cf-ok", mask)).focus();
  });
}
// Esc 关闭最近弹窗
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && modalStack.length) closeModal();
});

// ---------- 设备指纹 ----------
function deviceId() {
  let id = localStorage.getItem("finbook_device_id");
  if (!id) { id = (crypto.randomUUID ? crypto.randomUUID() : "d-" + Math.random().toString(36).slice(2) + Date.now()); localStorage.setItem("finbook_device_id", id); }
  return id;
}
function deviceName() { return (navigator.platform || "Web") + " · " + navigator.userAgent.slice(0, 40); }

// ---------- 权限 ----------
function can(p) { return session.user && session.user.perms.indexOf(p) >= 0; }

// ===========================================================================
// 登录（平台级：登录后再选择/新建账套）
// ===========================================================================
async function showLogin() {
  const app = document.getElementById("app");
  app.innerHTML = `
    <div class="login-wrap">
      <div class="login-card">
        <h1>FinBook 财务管理系统</h1>
        <div class="sub">多用户 · 多账套</div>
        <div class="banner set">请输入平台账号登录。登录后可选择或新建自己的账套。</div>
        <form id="login-form">
          <div class="field"><label>账号</label><input id="u" autocomplete="username" required /></div>
          <div class="field"><label>口令</label><input id="p" type="password" autocomplete="current-password" required /></div>
          <button class="btn block" type="submit">登录</button>
        </form>
        <div id="login-err" class="muted" style="color:var(--err);margin-top:10px;min-height:18px"></div>
      </div>
    </div>`;
  $("#login-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const username = $("#u").value.trim();
    const password = $("#p").value;
    try {
      const r = await api("/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username, password, device_id: deviceId(), device_name: deviceName(), book_key: "" }),
      });
      session.user = r.user;
      session.platformAdmin = !!r.user.is_admin;
      if (r.must_change_pwd) {
        openChangePwd(true, () => showBookPicker());
      } else {
        toast(`欢迎，${esc(r.user.display_name)}`, "ok");
        showBookPicker({ user: r.user, books: r.books });
      }
    } catch (err) {
      $("#login-err").textContent = err.message;
    }
  });
}

// ---------------------------------------------------------------------------
// 账套选择 / 新建（登录成功后、进入账套前）
// ---------------------------------------------------------------------------

async function loadMyBooks() {
  const r = await api("/books");
  return { user: r.user || null, books: (r && r.books) || [] };
}

async function showBookPicker(pre, force) {
  let data = pre || null;
  if (!data) {
    try { data = await loadMyBooks(); } catch (e) { return; }
  }
  // force：从账套内退回选择页时，用平台身份覆盖账套内身份
  if (data.user && (!session.user || force)) session.user = data.user;
  if (data.user) session.platformAdmin = !!data.user.is_admin;
  const books = data.books || [];
  const app = document.getElementById("app");
  const u = session.user || {};
  const isPlatformAdmin = !!session.platformAdmin;
  app.innerHTML = `
    <div class="login-wrap">
      <div class="login-card wide">
        <h1>选择账套</h1>
        <div class="sub">${esc(u.display_name || "")}${isPlatformAdmin ? "（平台管理员）" : ""}</div>
        <div class="muted" style="margin:6px 0 14px">普通用户可在自己创建的账套中记账；平台管理员可进入全部账套查看。选择账套进入，或新建一套。</div>
        <div id="book-list" class="book-list">${books.length ? books.map((b) => `
          <div class="book-item">
            <button class="book-enter" data-key="${esc(b.key)}" data-company="${esc(b.company || b.key)}">
              <span class="book-name">${esc(b.company || b.key)}</span>
              <span class="book-meta">${isPlatformAdmin ? `归属：${esc(b.owner)} · ` : ""}${esc(b.key)}</span>
            </button>
            ${(isPlatformAdmin || b.owner === u.username) ? `<button class="btn sm ghost book-del" data-del="${esc(b.key)}" title="删除该账套（数据不可恢复）">删除</button>` : ""}
          </div>`).join("") : `<div class="muted" style="padding:18px 0">还没有账套，点击下方「新建账套」开始记账。</div>`}</div>
        <div style="display:flex;gap:10px;margin-top:16px">
          <button class="btn primary" id="new-book">＋ 新建账套</button>
          <span class="grow"></span>
          <button class="btn ghost" id="picker-logout">退出登录</button>
        </div>
      </div>
    </div>`;
  $all(".book-enter").forEach((el) => {
    el.addEventListener("click", () => enterBook(el.dataset.key, el.dataset.company));
  });
  $all(".book-del").forEach((el) => {
    el.addEventListener("click", () => deleteBook(el.dataset.del, el.dataset.del));
  });
  $("#new-book").addEventListener("click", openCreateBook);
  $("#picker-logout").addEventListener("click", async () => {
    try { await api("/logout", { method: "POST" }); } catch (e) {}
    session.user = null;
    showLogin();
  });
}

async function enterBook(key, name) {
  try {
    await api(`/books/${encodeURIComponent(key)}/select`, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
    state.bookKey = key;
    const me = await api("/me");
    session.user = me;
    shellBuilt = false; // 身份从平台层切换为账套层，顶栏与侧边栏需重建
    state.view = "dashboard";
    toast(`已进入账套「${esc(name || key)}」`, "ok");
    await afterLogin();
  } catch (e) {
    toast(e.message, "err");
    if (String(e.message).indexOf("登录") >= 0 || String(e.message).indexOf("停用") >= 0) showLogin();
    else if (session.user) showBookPicker();
  }
}

/// 删除账套（平台管理员 或 账套归属者）；删除后回到账套选择页
async function deleteBook(key, name) {
  if (!(await confirmDialog(`确定删除账套「${name || key}」？该账套内的全部凭证、科目与设置将被永久删除，不可恢复。`, true))) return;
  try {
    await api(`/books/${encodeURIComponent(key)}`, { method: "DELETE" });
    toast("账套已删除", "ok");
    if (key === state.bookKey) {
      // 删掉的正是当前所在账套：退回选择页并用平台身份重建
      state.bookKey = "";
      shellBuilt = false;
      await showBookPicker(null, true);
    } else {
      await showBookPicker();
    }
  } catch (e) { toast(e.message, "err"); }
}

function openCreateBook() {
  const now = new Date();
  const mask = modal(`
    <h3>新建账套</h3>
    <p class="muted" style="margin:0 0 14px;line-height:1.6">
      每个账套都是独立隔离的一套账。创建者自动成为该账套的管理员，可再为同事开通账套内子账号。
    </p>
    <div class="field"><label>公司名称</label><input id="cb-company" placeholder="例如：某某贸易有限公司" /></div>
    <div class="field"><label>启用期间（YYYY-MM）</label><input id="cb-start" value="${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}" /></div>
    <div class="field"><label>账套标识（可选，留空自动生成）</label><input id="cb-key" placeholder="字母/数字/下划线" /></div>
    <div class="foot"><button class="btn" id="cb-cancel">取消</button><button class="btn primary" id="cb-save">创建</button></div>`);
  $("#cb-cancel", mask).onclick = closeModal;
  $("#cb-save", mask).onclick = async () => {
    const company = $("#cb-company", mask).value.trim();
    if (!company) { toast("请输入公司名称", "err"); return; }
    const ym = $("#cb-start", mask).value.trim().replace(/[^0-9]/g, "");
    let start_period = 0;
    if (ym.length === 6) start_period = parseInt(ym, 10);
    else if (/^\d{4}$/.test(ym)) start_period = parseInt(ym, 10) * 100 + 1;
    if (start_period <= 0) { toast("启用期间格式应为 YYYY-MM", "err"); return; }
    try {
      const r = await api("/books", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ key: $("#cb-key", mask).value.trim(), company, start_period }) });
      closeModal();
      toast(`账套「${esc(company)}」创建成功`, "ok");
      await enterBook(r.key, company);
    } catch (e) { toast(e.message, "err"); }
  };
}

async function afterLogin() {
  // 加载期间列表与当前期间
  try {
    const p = await api("/periods");
    state.periods = p.list || [];
    state.current = p.current;
  } catch (e) {}
  state.view = "dashboard";
  render();
  // 未建账（尚未设置公司名）→ 弹出建账向导
  try {
    const st = await api("/setup/status");
    if (st && st.needs_setup) showSetupWizard();
  } catch (e) {}
}

// ===========================================================================
// 建账向导（首次登录 / 未设置公司名时出现）
// ===========================================================================
async function showSetupWizard() {
  if (!session.user) return;
  let opts = {};
  try { opts = await api("/options"); } catch (e) {}
  const now = new Date();
  const mask = modal(`
    <h3>创建账套</h3>
    <p class="muted" style="margin:0 0 14px;line-height:1.6">
      为当前账套设置公司信息与启用期间。完成后即可开始填制凭证。
      此步骤可由管理员随时在「账套参数」中修改。
    </p>
    <div class="field">
      <label>公司名称</label>
      <input id="set-company" placeholder="例如：某某贸易有限公司" value="${esc(opts.company || "")}" />
    </div>
    <div class="field">
      <label>启用期间（YYYY-MM）</label>
      <input id="set-start" placeholder="2026-01" value="${opts.start_period || now.getFullYear() + "-01"}" />
    </div>
    <div class="field">
      <label>本位币</label>
      <input id="set-currency" value="${opts.base_currency || "CNY"}" />
    </div>
    <div class="foot">
      <button class="btn ghost" id="setup-later">稍后再说</button>
      <button class="btn primary" id="setup-save">创建账套</button>
    </div>
  `);
  $("#setup-save").addEventListener("click", async () => {
    const company = $("#set-company").value.trim();
    if (!company) { toast("请输入公司名称", "err"); return; }
    const startText = $("#set-start").value.trim();
    const ym = startText.replace(/[^0-9]/g, "");
    let start_period = 0;
    if (ym.length === 6) start_period = parseInt(ym, 10);
    else if (/^\d{4}$/.test(ym)) start_period = parseInt(ym, 10) * 100 + 1;
    if (start_period <= 0) { toast("启用期间格式应为 YYYY-MM", "err"); return; }
    try {
      const cur = await api("/options");
      const merged = Object.assign({}, cur, { company, start_period, base_currency: $("#set-currency").value.trim() || "CNY" });
      await api("/options", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(merged) });
      closeModal();
      toast("账套创建成功", "ok");
      state.view = "dashboard";
      render();
    } catch (err) { toast(err.message, "err"); }
  });
  const later = $("#setup-later");
  if (later) later.addEventListener("click", () => closeModal());
}

// ===========================================================================
// 应用骨架
// ===========================================================================

// 导航项配置（新增页面只改这里 + VIEWS 注册表，无需改 switch）
// admin: true 表示仅系统管理员可见（只读视角入口）
// group: 侧边栏分组（与桌面端 NavItem::group 保持一致）
const NAV_ITEMS = [
  { id: "overview", label: "账目总览", admin: true, group: "管理员" },
  { id: "platform-users", label: "平台账号", platform: true, group: "平台管理" },
  { id: "platform-books", label: "全部账套", platform: true, group: "平台管理" },
  { id: "dashboard", label: "仪表盘", perm: null, group: "开始" },
  { id: "accounts", label: "会计科目", perm: "account_edit", group: "基础资料" },
  { id: "begin", label: "期初建账", perm: "opening", group: "基础资料" },
  { id: "aux", label: "辅助档案", perm: "aux_edit", group: "基础资料" },
  { id: "vouchers", label: "记账凭证", perm: "voucher_new", group: "凭证" },
  { id: "imports", label: "数据导入", perm: "voucher_new", group: "凭证" },
  { id: "templates", label: "凭证模板", perm: "voucher_new", group: "凭证" },
  { id: "payroll", label: "工资管理", perm: "voucher_new", group: "凭证" },
  { id: "claims", label: "费用报销", perm: "voucher_new", group: "凭证" },
  { id: "invoices", label: "发票管理", perm: "report", group: "凭证" },
  { id: "ledger", label: "明细账", perm: "report", group: "账簿报表" },
  { id: "reports", label: "报表中心", perm: "report", group: "账簿报表" },
  { id: "balance-sheet", label: "资产负债表", perm: "report", group: "账簿报表" },
  { id: "income-statement", label: "利润表", perm: "report", group: "账簿报表" },
  { id: "cash-flow", label: "现金流量表", perm: "report", group: "账簿报表" },
  { id: "multi-column", label: "多栏账", perm: "report", group: "账簿报表" },
  { id: "summary-table", label: "摘要汇总表", perm: "report", group: "账簿报表" },
  { id: "ratios", label: "财务指标", perm: "report", group: "账簿报表" },
  { id: "equity", label: "权益变动表", perm: "report", group: "账簿报表" },
  { id: "compare", label: "报表对比", perm: "report", group: "账簿报表" },
  { id: "daily", label: "科目日报表", perm: "report", group: "账簿报表" },
  { id: "notes", label: "报表附注", perm: "report", group: "账簿报表" },
  { id: "custom-reports", label: "自定义报表", perm: "report", group: "账簿报表" },
  { id: "period-end", label: "期末处理", perm: "period_close", group: "期末" },
  { id: "assets", label: "固定资产", perm: "account_edit", group: "期末" },
  { id: "bank", label: "银行对账", perm: "voucher_new", group: "期末" },
  { id: "settle", label: "往来核销", perm: "voucher_new", group: "期末" },
  { id: "reconcile", label: "期末对账", perm: "report", group: "期末" },
  { id: "mrp", label: "MRP 运算", perm: "account_edit", group: "生产制造" },
  { id: "routing", label: "工艺路线", perm: "account_edit", group: "生产制造" },
  { id: "work-report", label: "工序报工", perm: "account_edit", group: "生产制造" },
  { id: "funds", label: "资金管理", perm: "report", group: "资金" },
  { id: "budget-versions", label: "预算版本", perm: "report", group: "管理会计" },
  { id: "budget-alerts", label: "预算预警", perm: "report", group: "管理会计" },
  { id: "budget-analysis", label: "预算分析", perm: "report", group: "管理会计" },
  { id: "cost", label: "成本核算", perm: "report", group: "管理会计" },
  { id: "po-doc", label: "采购单据", perm: "account_edit", group: "采购" },
  { id: "po-reconcile", label: "采购对账", perm: "report", group: "采购" },
  { id: "po-estimate", label: "采购暂估", perm: "account_edit", group: "采购" },
  { id: "procure-quota", label: "供应商配额", perm: "account_edit", group: "采购" },
  { id: "so-doc", label: "销售单据", perm: "account_edit", group: "销售" },
  { id: "so-reconcile", label: "销售对账", perm: "report", group: "销售" },
  { id: "order-change-log", label: "订单变更", perm: "report", group: "销售" },
  { id: "inv-aging", label: "库存账龄", perm: "report", group: "库存" },
  { id: "inv-abc", label: "库存ABC", perm: "report", group: "库存" },
  { id: "inv-serial", label: "序列号", perm: "account_edit", group: "库存" },
  { id: "inv-unit", label: "多单位换算", perm: "account_edit", group: "库存" },
  { id: "inv-assemble", label: "组装拆卸", perm: "account_edit", group: "库存" },
  { id: "inv-warehouse", label: "分仓库库存", perm: "report", group: "库存" },
  { id: "inv-transfer", label: "调拨报表", perm: "report", group: "库存" },
  { id: "approval", label: "审批中心", perm: "report", group: "系统" },
  { id: "archive", label: "电子档案", perm: "report", group: "系统" },
  { id: "options", label: "账套参数", perm: "sys_option", group: "系统" },
  { id: "logs", label: "操作日志", perm: "audit_log", group: "系统" },
  { id: "backup", label: "备份恢复", perm: "backup", group: "系统" },
  { id: "security", label: "安全中心", perm: "user_manage", group: "系统" },
];

// 视图注册表：id → 渲染函数（函数声明已提升，可在顶层引用）
const VIEWS = {
  "overview": viewOverview,
  "platform-users": viewPlatformUsers,
  "platform-books": viewPlatformBooks,
  "dashboard": viewDashboard,
  "vouchers": viewVouchers,
  "invoices": viewInvoices,
  "imports": viewImports,
  "ledger": viewLedger,
  "reports": viewReports,
  "balance-sheet": viewBalanceSheet,
  "income-statement": viewIncomeStatement,
  "cash-flow": viewCashFlow,
  "multi-column": viewMultiColumn,
  "summary-table": viewSummaryTable,
  "ratios": viewRatios,
  "equity": viewEquity,
  "compare": viewCompare,
  "daily": viewDaily,
  "period-end": viewPeriodEnd,
  "assets": viewAssets,
  "bank": viewBank,
  "settle": viewSettle,
  "reconcile": viewReconcile,
  "mrp": viewMrp,
  "routing": viewRouting,
  "approval": viewApproval,
  "notes": viewNotes,
  "custom-reports": viewCustomReports,
  "archive": viewArchive,
  "budget-versions": viewBudgetVersions,
  "budget-alerts": viewBudgetAlerts,
  "po-reconcile": viewPoReconcile,
  "so-reconcile": viewSoReconcile,
  "inv-aging": viewInvAging,
  "inv-abc": viewInvAbc,
  "inv-serial": viewInvSerial,
  "inv-unit": viewInvUnit,
  "inv-assemble": viewInvAssemble,
  "inv-warehouse": viewInvWarehouse,
  "inv-transfer": viewInvTransfer,
  "po-estimate": viewPoEstimate,
  "procure-quota": viewProcureQuota,
  "po-doc": viewPoDoc,
  "so-doc": viewSoDoc,
  "order-change-log": viewOrderChangeLog,
  "work-report": viewWorkReport,
  "funds": viewFunds,
  "budget-analysis": viewBudgetAnalysis,
  "cost": viewCost,
  "security": viewSecurity,
  "accounts": viewAccounts,
  "begin": viewBegin,
  "aux": viewAux,
  "options": viewOptions,
  "logs": viewLogs,
  "backup": viewBackup,
  "templates": viewTemplates,
  "payroll": viewPayroll,
  "claims": viewClaims,
};

let shellBuilt = false;

// 骨架只渲染一次；切换视图只更新 .main，不再重建 topbar/sidebar
function renderShell() {
  const u = session.user;
  const app = document.getElementById("app");
  const nav = NAV_ITEMS.filter((n) => !n.perm || can(n.perm));  const periodOpts = state.periods.map((p) => `<option value="${p}" ${p === state.current ? "selected" : ""}>${p}</option>`).join("");
  app.innerHTML = `
    <div class="app">
      <div class="topbar">
        <button class="hamburger" id="menu-btn" aria-label="打开菜单">☰</button>
        <span class="logo" id="logo-home" title="回到仪表盘">FinBook</span>
        <span class="who">${esc(u.display_name)}（${esc(u.role_label)}）</span>
        <select id="period-sel" title="会计期间">${periodOpts}</select>
        <span class="grow"></span>
        <button class="btn ghost sm" id="switch-book">切换账套</button>
        <button class="btn ghost sm" id="change-pwd">修改口令</button>
        <button class="btn ghost sm" id="logout">退出登录</button>
      </div>
      <div class="sidebar" id="sidebar">
        ${(() => {
          const pa = !!session.platformAdmin;
          const visible = nav.filter((n) => !(n.admin && !session.user.is_admin) && !(n.platform && !pa));
          let lastGroup = "";
          let html = "";
          for (const n of visible) {
            if (n.group && n.group !== lastGroup) {
              html += `<div class="group">${esc(n.group)}</div>`;
              lastGroup = n.group;
            }
            html += `<button class="nav-item ${n.id === state.view ? "active" : ""}" data-view="${n.id}">${n.label}</button>`;
          }
          return html;
        })()}
      </div>
      <div class="side-mask" id="side-mask"></div>
      <div class="main" id="main"></div>
    </div>`;

  function closeMenu() { document.body.classList.remove("menu-open"); }
  function toggleMenu() { document.body.classList.toggle("menu-open"); }

  // 点击 logo 回到仪表盘（不再是无行为的死元素）
  $("#logo-home").addEventListener("click", () => { state.view = "dashboard"; closeMenu(); renderMain(); });
  $("#menu-btn").addEventListener("click", toggleMenu);
  $("#side-mask").addEventListener("click", closeMenu);
  // 事件委托：nav-item 只在 sidebar 容器上绑一次
  $(".sidebar").addEventListener("click", (e) => {
    const btn = e.target.closest(".nav-item");
    if (!btn) return;
    state.view = btn.dataset.view;
    $all(".nav-item").forEach((b) => b.classList.toggle("active", b.dataset.view === state.view));
    closeMenu();
    renderMain();
  });
  $("#period-sel").addEventListener("change", async (e) => {
    state.current = e.target.value;
    try { await api("/period", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ymm: ymm(e.target.value) }) }); } catch (err) {}
    renderMain();
  });
  $("#logout").addEventListener("click", logout);
  $("#switch-book").addEventListener("click", async () => {
    shellBuilt = false;
    state.bookKey = "";
    await showBookPicker();
  });
  $("#change-pwd").addEventListener("click", () => openChangePwd(false));
  shellBuilt = true;
}

function render() {
  // session.user 有 perms = 已进入账套（PublicUser）；只有平台身份（PlatformUser）时停在账套选择页
  if (!session.user) { shellBuilt = false; showLogin(); return; }
  if (!session.user.perms) { shellBuilt = false; showBookPicker(); return; }
  if (!shellBuilt) renderShell();
  renderMain();
}

function renderMain() {
  const main = document.getElementById("main");
  const fn = VIEWS[state.view] || viewDashboard;
  return fn(main);
}

async function logout() {
  try { await api("/logout", { method: "POST" }); } catch (e) {}
  session.user = null;
  session.platformAdmin = false;
  render();
}

// ===========================================================================
// 仪表盘
// ===========================================================================
async function viewDashboard(main) {
  main.innerHTML = `<h2>仪表盘</h2><div class="muted">加载中…</div>`;
  let d;
  try { d = await api("/dashboard"); } catch (e) { main.innerHTML = `<h2>仪表盘</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  let status = { admin_set: false };
  try { status = await api("/setup/status"); } catch (e) {}
  const adminBanner = status.admin_set
    ? `<div class="banner set">✅ 管理员账号已设定</div>`
    : `<div class="banner unset">🔧 管理员账号未设定 —— 首次成功登录的账号将自动成为系统管理员。</div>`;
  main.innerHTML = `
    <h2>仪表盘</h2>
    ${adminBanner}
    <div class="cards">
      <div class="card"><div class="k">公司名称</div><div class="v" style="font-size:16px">${esc(d.company || "—")}</div></div>
      <div class="card"><div class="k">当前会计期间</div><div class="v">${esc(d.current_period)}</div></div>
      <div class="card"><div class="k">已结账至</div><div class="v">${esc(d.closed_upto || "未结账")}</div></div>
    </div>
    <div class="cards" style="margin-top:14px">
      <div class="card"><div class="k">凭证数</div><div class="v">${esc(d.vouchers)}</div></div>
      <div class="card"><div class="k">分录数</div><div class="v">${esc(d.entries)}</div></div>
      <div class="card"><div class="k">科目数</div><div class="v">${esc(d.accounts)}</div></div>
    </div>`;
}

// ===========================================================================
// 管理员 · 账目总览（只读视角，仅系统管理员可见）
// ===========================================================================

// 金额字符串（"1,234.56" / "-1,234.56"）转数字
function moneyNum(s) {
  const n = parseFloat(String(s == null ? "0" : s).replace(/[^0-9.-]/g, ""));
  return Number.isFinite(n) ? n : 0;
}
function moneyFmt(n) {
  const neg = n < 0;
  const a = Math.abs(n).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  return neg ? "-" + a : a;
}
function pctFmt(n) { return n.toLocaleString("en-US", { maximumFractionDigits: 1 }) + "%"; }

// 折线图（SVG，纯原生实现）。series: [{name, color, values:number[], anomalies:bool[]}]
function lineChartSvg(labels, series, height) {
  const W = 760, H = height || 250;
  const padL = 62, padR = 14, padT = 16, padB = 30;
  const iw = W - padL - padR, ih = H - padT - padB;
  const n = labels.length;
  if (!n) return "";
  let min = 0, max = 0;
  for (const s of series) for (const v of s.values) { if (v < min) min = v; if (v > max) max = v; }
  if (max === min) max = min + 1;
  const span = max - min || 1;
  const x = (i) => padL + (n === 1 ? iw / 2 : (iw * i) / (n - 1));
  const y = (v) => padT + ih - ((v - min) / span) * ih;
  const y0 = min < 0 && max > 0 ? y(0) : null;

  // 网格与 Y 轴刻度（4 档）
  let grid = "", yticks = "";
  for (let k = 0; k <= 4; k++) {
    const v = min + (span * k) / 4;
    const gy = y(v);
    grid += `<line x1="${padL}" y1="${gy}" x2="${W - padR}" y2="${gy}" stroke="var(--z-200)" stroke-width="1"/>`;
    yticks += `<text x="${padL - 8}" y="${gy + 4}" text-anchor="end" class="ctick">${moneyFmt(v)}</text>`;
  }
  // X 轴标签（最多显示 12 个）
  let xlabels = "";
  const step = Math.ceil(n / 12);
  for (let i = 0; i < n; i += step) {
    xlabels += `<text x="${x(i)}" y="${H - 8}" text-anchor="middle" class="ctick">${esc(labels[i])}</text>`;
  }

  let lines = "", dots = "";
  for (const s of series) {
    const pts = s.values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
    lines += `<polyline points="${pts}" fill="none" stroke="${s.color}" stroke-width="2.2" stroke-linejoin="round" stroke-linecap="round"/>`;
    s.values.forEach((v, i) => {
      dots += `<circle cx="${x(i).toFixed(1)}" cy="${y(v).toFixed(1)}" r="${s.anomalies && s.anomalies[i] ? 4.5 : 2.6}" fill="${s.anomalies && s.anomalies[i] ? "#dc2626" : s.color}"/>`;
    });
  }
  const zeroAxis = y0 != null ? `<line x1="${padL}" y1="${y0.toFixed(1)}" x2="${W - padR}" y2="${y0.toFixed(1)}" stroke="var(--z-400)" stroke-width="1" stroke-dasharray="4 3"/>` : "";

  return `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" class="chart">${grid}${zeroAxis}${lines}${dots}${yticks}${xlabels}</svg>`;
}

// 分组柱状图（当月发生额）。groups: [{name, color, values, anomalies}]
function barChartSvg(labels, groups, height) {
  const W = 760, H = height || 250;
  const padL = 62, padR = 14, padT = 16, padB = 30;
  const iw = W - padL - padR, ih = H - padT - padB;
  const n = labels.length;
  if (!n) return "";
  let min = 0, max = 0;
  for (const g of groups) for (const v of g.values) { if (v < min) min = v; if (v > max) max = v; }
  if (max === min) max = min + 1;
  const span = max - min || 1;
  const y = (v) => padT + ih - ((v - min) / span) * ih;
  const y0 = min < 0 && max > 0 ? y(0) : null;

  let grid = "", yticks = "";
  for (let k = 0; k <= 4; k++) {
    const v = min + (span * k) / 4;
    const gy = y(v);
    grid += `<line x1="${padL}" y1="${gy}" x2="${W - padR}" y2="${gy}" stroke="var(--z-200)" stroke-width="1"/>`;
    yticks += `<text x="${padL - 8}" y="${gy + 4}" text-anchor="end" class="ctick">${moneyFmt(v)}</text>`;
  }
  let xlabels = "";
  const step = Math.ceil(n / 12);
  for (let i = 0; i < n; i += step) {
    xlabels += `<text x="${barX(i)}" y="${H - 8}" text-anchor="middle" class="ctick">${esc(labels[i])}</text>`;
  }

  const g = groups.length;
  const slot = iw / n;
  const bw = Math.min(18, (slot * 0.7) / g);
  let bars = "";
  groups.forEach((grp, gi) => {
    grp.values.forEach((v, i) => {
      const cx = barX(i) + (gi - (g - 1) / 2) * (bw + 2);
      const vy = y(v), base = y0 != null ? y0 : y(0);
      const h = Math.abs(base - vy);
      bars += `<rect x="${(cx - bw / 2).toFixed(1)}" y="${Math.min(vy, base).toFixed(1)}" width="${bw.toFixed(1)}" height="${Math.max(h, 1).toFixed(1)}" fill="${grp.color}" rx="1.5"/>`;
      if (grp.anomalies && grp.anomalies[i]) {
        bars += `<path d="M ${cx.toFixed(1)} ${(vy - 7).toFixed(1)} l 4 7 l -8 0 z" fill="#dc2626"/>`;
      }
    });
  });

  function barX(i) { return padL + slot * (i + 0.5); }
  const zeroAxis = y0 != null ? `<line x1="${padL}" y1="${y0.toFixed(1)}" x2="${W - padR}" y2="${y0.toFixed(1)}" stroke="var(--z-400)" stroke-width="1" stroke-dasharray="4 3"/>` : "";
  return `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" class="chart">${grid}${zeroAxis}${bars}${yticks}${xlabels}</svg>`;
}

// 指标内因：横向条形列表（带环比变化与占比）
function driverBars(items, signed) {
  const maxAbs = Math.max(1, ...items.map((it) => Math.abs(moneyNum(it.amount))));
  return items.map((it) => {
    const cur = moneyNum(it.amount);
    const prev = moneyNum(it.prev_amount);
    const w = Math.round((Math.abs(cur) / maxAbs) * 100);
    const color = signed ? (cur < 0 ? "var(--err)" : "var(--ok)") : "var(--primary)";
    let change = "";
    if (prev !== 0) {
      const pct = ((cur - prev) / Math.abs(prev)) * 100;
      const dir = pct > 0.5 ? "▲" : pct < -0.5 ? "▼" : "—";
      change = `<span class="drv-chg ${pct > 0.5 ? "up" : pct < -0.5 ? "down" : ""}">${dir} ${pctFmt(Math.abs(pct))}</span>`;
    } else if (cur !== 0) {
      change = `<span class="drv-chg up">▲ 新增</span>`;
    }
    return `<div class="drv-row">
      <div class="drv-head"><span class="drv-name">${esc(it.name)}</span>
        <span class="drv-amt">${moneyFmt(cur)}</span>${change}</div>
      <div class="drv-bar"><i style="width:${w}%;background:${color}"></i></div>
    </div>`;
  }).join("");
}

// 财务指标解读（阀值判断，给出好/中/差）
function ratioVerdict(key, v) {
  switch (key) {
    case "current_ratio": return v >= 2 ? "good" : v >= 1 ? "warn" : "bad";
    case "quick_ratio": return v >= 1 ? "good" : v >= 0.5 ? "warn" : "bad";
    case "debt_ratio": return v <= 0.5 ? "good" : v <= 0.7 ? "warn" : "bad";
    case "gross_margin": return v >= 0.3 ? "good" : v >= 0.1 ? "warn" : "bad";
    case "net_margin": return v >= 0.1 ? "good" : v >= 0 ? "warn" : "bad";
    case "roe": return v >= 0.1 ? "good" : v >= 0 ? "warn" : "bad";
    case "roa": return v >= 0.05 ? "good" : v >= 0 ? "warn" : "bad";
    default: return "warn";
  }
}
const VERDICT_LABEL = { good: "健康", warn: "关注", bad: "预警" };

async function viewOverview(main) {
  main.innerHTML = `<h2>账目总览</h2><div class="muted">加载中…</div>`;
  let d;
  try { d = await api(`/overview?period=${encodeURIComponent(state.current || "")}`); } catch (e) {
    main.innerHTML = `<h2>账目总览</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`;
    return;
  }
  const t = d.totals || {};
  const a = d.analysis || {};
  const stMap = { draft: ["未记账", "warn"], audited: ["已审核", "warn"], posted: ["已记账", "ok"], void: ["已作废", "err"] };
  const card = (k, v, style) => `<div class="card"><div class="k">${k}</div><div class="v" ${style ? `style="${style}"` : ""}>${v}</div></div>`;

  // ---- 走势图数据 ----
  const trend = a.trend || [];
  const labels = trend.map((x) => String(x.period).slice(5) + "月");
  const cumSeries = [
    { name: "累计营业收入", color: "#2563eb", values: trend.map((x) => moneyNum(x.cum_revenue)), anomalies: trend.map((x) => x.anomaly_revenue) },
    { name: "累计营业成本", color: "#f59e0b", values: trend.map((x) => moneyNum(x.cum_cost)), anomalies: trend.map((x) => x.anomaly_cost) },
    { name: "累计净利润", color: "#16a34a", values: trend.map((x) => moneyNum(x.cum_net_profit)), anomalies: trend.map((x) => x.anomaly_net_profit) },
  ];
  const monthGroups = [
    { name: "营业收入", color: "#2563eb", values: trend.map((x) => moneyNum(x.revenue)), anomalies: trend.map((x) => x.anomaly_revenue) },
    { name: "营业成本", color: "#f59e0b", values: trend.map((x) => moneyNum(x.cost)), anomalies: trend.map((x) => x.anomaly_cost) },
    { name: "净利润", color: "#16a34a", values: trend.map((x) => moneyNum(x.net_profit)), anomalies: trend.map((x) => x.anomaly_net_profit) },
  ];
  const legend = (series) => series.map((s) =>
    `<span class="lg-item"><i style="background:${s.color}"></i>${esc(s.name)}</span>`).join("");

  main.innerHTML = `
    <h2>账目总览</h2>
    <p class="muted" style="margin:0 0 12px">
      ${esc(d.company || "")} · 期间 ${esc(d.period || "")} · 已结账至 ${esc(d.closed_upto || "未结账")}
      —— 管理员只读视角：查看账目全貌，不做录入；录入请用「记账凭证」，明细见左侧各查询页。
    </p>
    <div class="cards">
      ${card("资产总额", t.total_asset || "—", "font-size:18px")}
      ${card("负债总额", t.total_liab || "—", "font-size:18px")}
      ${card("所有者权益", t.equity || "—", "font-size:18px")}
      ${card("净利润（年初至今）", t.net_profit || "—", "font-size:18px")}
    </div>

    <div class="panel" style="margin-top:14px">
      <div class="chart-head"><b>财务走势 · 年初至今累计</b><span class="lg">${legend(cumSeries)}</span></div>
      <div class="chart-box">${lineChartSvg(labels, cumSeries, 230)}</div>
      <div class="chart-note">红线圆点 = 异常月份（偏离年内均值 ±2σ 或出现亏损）；折线为 1 月起累计值。</div>
    </div>
    <div class="panel" style="margin-top:14px">
      <div class="chart-head"><b>当月发生额（逐月对比）</b><span class="lg">${legend(monthGroups)}</span></div>
      <div class="chart-box">${barChartSvg(labels, monthGroups, 230)}</div>
      <div class="chart-note">红三角 = 异常月份；柱状为各月发生额，便于发现突增突减与亏损月。</div>
    </div>

    ${(a.anomaly_notes || []).length ? `<div class="panel warn-panel" style="margin-top:14px">
      <b>⚠ 异常提示</b>
      ${(a.anomaly_notes || []).slice(0, 6).map((n) => `<div class="anom">${esc(n)}</div>`).join("")}
      ${(a.anomaly_notes || []).length > 6 ? `<div class="muted">…共 ${a.anomaly_notes.length} 条</div>` : ""}
    </div>` : ""}

    <div class="grid-3" style="margin-top:14px">
      <div class="panel">
        <h4>营业收入构成（本月 vs 上月）</h4>
        ${driverBars(a.revenue_drivers || [], false) || `<div class="muted">暂无数据</div>`}
      </div>
      <div class="panel">
        <h4>营业成本构成（本月 vs 上月）</h4>
        ${driverBars(a.cost_drivers || [], false) || `<div class="muted">暂无数据</div>`}
      </div>
      <div class="panel">
        <h4>净利润构成（利润表口径）</h4>
        ${driverBars(a.profit_drivers || [], true) || `<div class="muted">暂无数据</div>`}
      </div>
    </div>

    <div class="panel" style="margin-top:14px">
      <b>财务状况分析</b>
      <div class="ratio-grid">
        ${(a.ratios || []).map((r) => {
          const verdict = ratioVerdict(r.key, parseFloat(r.value));
          return `<div class="ratio-card ${verdict}">
            <div class="rc-name">${esc(r.name)}</div>
            <div class="rc-val">${esc(r.display)}</div>
            <div class="rc-tag ${verdict}">${VERDICT_LABEL[verdict]}</div>
            <div class="rc-formula">${esc(r.formula)}</div>
          </div>`;
        }).join("") || `<div class="muted">暂无指标</div>`}
      </div>
    </div>

    <div class="cards" style="margin-top:14px">
      ${card("凭证总数（全账套）", esc(d.vouchers))}
      ${card("当期未记账", esc(d.unposted))}
      ${card("当期已记账", esc(d.posted))}
      ${card("科目数（全账套）", esc(d.accounts))}
    </div>
    <div class="cards" style="margin-top:14px">
      ${card("进项发票价税合计", `${(d.invoice_in && d.invoice_in.amount_tax) || "0.00"}（${(d.invoice_in && d.invoice_in.count) || 0} 张）`)}
      ${card("销项发票价税合计", `${(d.invoice_out && d.invoice_out.amount_tax) || "0.00"}（${(d.invoice_out && d.invoice_out.count) || 0} 张）`)}
    </div>
    <div class="toolbar" style="margin-top:14px">
      <span class="muted">科目表维护：</span>
      <button class="btn ghost sm" id="ov-fill">补齐新版科目表</button>
      <span class="muted">内置模板共 199 个科目；旧账套一键补入缺少的科目，不影响已有科目。</span>
    </div>
    <div class="panel" style="margin-top:14px;padding:0;overflow:hidden">
      <table class="grid"><thead><tr>
        <th>期间</th><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>状态</th><th>制单</th>
      </tr></thead><tbody>
        ${(d.recent || []).length ? d.recent.map((v) => {
          const s = stMap[v.status] || [v.status_label, ""];
          return `<tr><td>${esc(v.period)}</td><td>${esc(v.date)}</td><td>${esc(v.voucher_no)}</td><td>${esc(v.summary)}</td><td class="num">${esc(v.debit_total)}</td><td class="num">${esc(v.credit_total)}</td><td><span class="tag ${s[1]}">${esc(s[0])}</span></td><td>${esc(v.prepared_by)}</td></tr>`;
        }).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:18px">暂无凭证</td></tr>`}
      </tbody></table>
    </div>`;
  const fillBtn = $("#ov-fill", main);
  if (fillBtn) fillBtn.addEventListener("click", async () => {
    try {
      const r = await api("/accounts/fill-defaults", { method: "POST" });
      toast(r.inserted > 0 ? `已补入 ${r.inserted} 个科目，当前共 ${r.total} 个` : `科目表已完整（共 ${r.total} 个）`, "ok");
      viewOverview(main);
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 凭证
// ===========================================================================
async function ensureAccounts() {
  if (!state.accounts) {
    try { state.accounts = await api("/accounts"); } catch (e) { state.accounts = []; }
  }
  return state.accounts;
}
function accountOptions(sel) {
  const list = state.accounts || [];
  return `<select class="acct-sel">${list.map((a) => `<option value="${esc(a.code)}">${esc(a.code)} ${esc(a.name)}</option>`).join("")}</select>`;
}

async function viewVouchers(main) {
  main.innerHTML = `
    <h2>记账凭证</h2>
    <div class="toolbar">
      ${can("voucher_new") ? `<button class="btn sm" id="new-v">新增凭证</button>` : ""}
      <input id="v-q" placeholder="摘要 / 凭证号 / 科目" style="width:200px" />
      <select id="v-status">
        <option value="">全部状态</option>
        <option value="draft">未记账</option>
        <option value="audited">已审核</option>
        <option value="posted">已记账</option><option value="void">已作废</option>
      </select>
      <button class="btn ghost sm" id="v-refresh">查询</button>
      ${can("voucher_edit") ? `<button class="btn ghost sm" id="v-renumber">重排断号</button>` : ""}
      ${can("voucher_post") ? `<button class="btn ghost sm" id="v-batch">批量记账</button>` : ""}
      ${can("report") ? `<button class="btn ghost sm" id="v-printform">凭证套打</button>` : ""}
      ${can("export") ? `<button class="btn ghost sm" id="v-export">导出 CSV</button>` : ""}
      <span class="spacer"></span>
      <span class="muted">期间：${esc(state.current || "")}</span>
    </div>
    <div class="panel"><table class="grid" id="v-table"><thead><tr>
      ${can("voucher_post") ? `<th style="width:26px"><input type="checkbox" id="v-all" title="全选未记账" /></th>` : ""}
      <th>期间</th><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>状态</th><th>制单</th><th></th>
    </tr></thead><tbody><tr><td colspan="${can("voucher_post") ? 10 : 9}" class="muted">加载中…</td></tr></tbody></table></div>`;
  if (can("voucher_new")) $("#new-v").addEventListener("click", () => openVoucherEditor(null));
  $("#v-refresh").addEventListener("click", () => loadVouchers());
  $("#v-q").addEventListener("keydown", (e) => { if (e.key === "Enter") loadVouchers(); });
  if ($("#v-all")) $("#v-all").addEventListener("change", (e) => { $all(".v-sel").forEach((c) => c.checked = e.target.checked); });
  if ($("#v-batch")) $("#v-batch").addEventListener("click", batchPost);
  if ($("#v-printform")) $("#v-printform").addEventListener("click", () => {
    window.open(`/api/vouchers/print-form?period=${encodeURIComponent(state.current || "")}`, "_blank");
  });
  if ($("#v-export")) $("#v-export").addEventListener("click", () => {
    const qs = new URLSearchParams({ period: (state.current || "").replace("-", ""), q: $("#v-q").value, status: $("#v-status").value });
    window.open(`/api/export/vouchers?${qs.toString()}`, "_blank");
  });
  if ($("#v-renumber")) $("#v-renumber").addEventListener("click", async () => {
    if (!(await confirmDialog(`将当前期间「记」字凭证的凭证号重排为连续？`, true))) return;
    try {
      const r = await api("/vouchers/renumber", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(state.current || ""), word: "记" }) });
      toast(`已重排 ${r.renumbered} 张凭证`, "ok"); loadVouchers();
    } catch (e) { toast(e.message, "err"); }
  });
  await ensureAccounts();
  loadVouchers();
}

async function loadVouchers() {
  const tb = $("#v-table tbody");
  const q = $("#v-q").value.trim();
  const st = $("#v-status").value;
  let url = `/vouchers?period=${encodeURIComponent(state.current || "")}`;
  if (q) url += `&q=${encodeURIComponent(q)}`;
  if (st) url += `&status=${st}`;
  let rows;
  try { rows = await api(url); } catch (e) { tb.innerHTML = `<tr><td colspan="${can("voucher_post") ? 10 : 9}" style="color:var(--err)">${esc(e.message)} <button class="btn ghost sm" id="v-retry">重试</button></td></tr>`; const rb = $("#v-retry", tb); if (rb) rb.addEventListener("click", loadVouchers); return; }
  if (!rows.length) { tb.innerHTML = `<tr><td colspan="9" class="muted">暂无凭证</td></tr>`; return; }
  const stMap = { draft: ["未记账", "warn"], audited: ["已审核", "warn"], posted: ["已记账", "ok"], void: ["已作废", "err"] };
  tb.innerHTML = rows.map((v) => {
    const s = stMap[v.status] || [v.status_label, ""];
    const selectable = can("voucher_post") && (v.status === "draft" || v.status === "audited");
    return `<tr>
      ${can("voucher_post") ? `<td>${selectable ? `<input type="checkbox" class="v-sel" data-id="${v.id}" />` : ""}</td>` : ""}
      <td>${esc(v.period)}</td><td>${esc(v.date)}</td><td>${esc(v.voucher_no)}</td>
      <td>${esc(v.summary)}</td><td class="num">${esc(v.debit_total)}</td><td class="num">${esc(v.credit_total)}</td>
      <td><span class="tag ${s[1]}">${esc(s[0])}</span></td><td>${esc(v.prepared_by)}</td>
      <td class="row-actions"><button class="btn ghost sm" data-edit="${v.id}">打开</button></td>
    </tr>`;
  }).join("");
  $all("[data-edit]").forEach((b) => b.addEventListener("click", () => openVoucherEditor(parseInt(b.dataset.edit, 10))));
}

async function batchPost() {
  const ids = $all(".v-sel:checked").map((c) => parseInt(c.dataset.id, 10));
  if (!ids.length) { toast("请先勾选未记账的凭证", "err"); return; }
  try {
    const r = await api("/vouchers/batch-post", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ids }) });
    const errs = r.errors || [];
    if (r.ok > 0 && errs.length === 0) { toast(`已记账 ${r.ok} 张`, "ok"); }
    else if (r.ok > 0) { toast(`已记账 ${r.ok} 张，失败 ${errs.length} 张`, "warn"); errs.slice(0, 5).forEach((e) => toast(e, "err")); }
    else { toast(`记账失败：${errs[0] || "未知原因"}`, "err"); }
    loadVouchers();
  } catch (e) { toast(e.message, "err"); }
}

// seedEntries：可选，凭证模板生成凭证时预填分录 [{account_code, summary, dir, amount}]
async function openVoucherEditor(id, seedEntries) {
  await ensureAccounts();
  // 辅助核算维度（与后端 AuxKind::bit 对齐）；cashflow 单独用现金流量项目输入
  const AUX_DEFS = [["customer", 1, "客户"], ["supplier", 2, "供应商"], ["dept", 4, "部门"],
    ["employee", 8, "职员"], ["project", 16, "项目"], ["item", 32, "存货"], ["bank", 128, "银行账户"]];
  const blank = { line: 1, account_code: "", summary: "", debit: "0", credit: "0", aux: {}, cf: "", qty: "", price: "" };
  let v = {
    id: 0, period: state.current, date: today(), word: "记", no: 0, attachments: 0, memo: "",
    entries: (seedEntries && seedEntries.length ? seedEntries.map((e, i) => ({
      line: i + 1, account_code: e.account_code || "", summary: e.summary || "",
      debit: e.dir === "credit" ? "0" : (e.amount || "0"),
      credit: e.dir === "credit" ? (e.amount || "0") : "0",
    })) : [Object.assign({}, blank, { line: 1 }), Object.assign({}, blank, { line: 2 })]),
  };
  let status = "draft", voucher_no = "";
  if (id) {
    try { v = await api(`/vouchers/${id}`); status = v.status; voucher_no = v.voucher_no; } catch (e) { toast(e.message, "err"); return; }
  } else {
    try { const n = await api(`/vouchers/next-no?period=${encodeURIComponent(state.current || "")}&word=记`); v.no = n.no; } catch (e) {}
  }
  // 规范化分录：辅助/数量/单价/现金流量统一成表单形态（后端返回 aux.cash_flow）
  v.entries = (v.entries || []).map((e, i) => {
    const aux = Object.assign({}, e.aux || {});
    const cf = aux.cash_flow || "";
    delete aux.cash_flow;
    return {
      line: i + 1, account_code: e.account_code || "", summary: e.summary || "",
      debit: e.debit != null ? String(e.debit) : "0",
      credit: e.credit != null ? String(e.credit) : "0",
      aux, cf,
      qty: e.qty != null ? String(e.qty) : "",
      price: e.price != null ? String(e.price) : "",
      currency: e.currency || "",
      rate: e.rate != null ? String(e.rate) : "",
      amount_for: e.amount_for != null ? String(e.amount_for) : "",
    };
  });
  if (!v.entries.length) v.entries = [Object.assign({}, blank, { line: 1 }), Object.assign({}, blank, { line: 2 })];
  // 可编辑状态与后端 can_edit() 对齐：未记账（含历史"已审核"）可改；已记账需先反记账
  const editable = (id === 0) || status === "draft" || status === "audited";
  const canPost = status === "draft" || status === "audited";
  const mask = modal(`
    <h3>记账凭证 ${esc(voucher_no)} <span class="muted" style="font-size:13px">${({ draft: "未记账", audited: "已审核", posted: "已记账", void: "已作废" })[status] || esc(status)}</span></h3>
    <div class="toolbar">
      <label>日期 <input id="v-date" type="date" value="${esc(v.date)}" ${editable ? "" : "disabled"} />${editable ? `<button class="btn ghost sm" id="v-today">今天</button>` : ""}</label>
      <span id="v-date-hint" class="muted" style="font-size:12px"></span>
      <label>字 <input id="v-word" value="${esc(v.word)}" style="width:60px" ${editable ? "" : "disabled"} /></label>
      <label>号 <input id="v-no" type="number" value="${v.no}" style="width:70px" ${editable ? "" : "disabled"} /></label>
      <label>附单据 <input id="v-att" type="number" value="${v.attachments}" style="width:60px" ${editable ? "" : "disabled"} /></label>
    </div>
    <table class="grid" id="v-entries">
      <thead><tr><th style="width:40px">行</th><th>科目</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th style="width:64px">辅助</th><th></th></tr></thead>
      <tbody></tbody>
    </table>
    ${editable ? `<button class="btn ghost sm" id="v-add">+ 增加分录</button>` : ""}
    <div style="margin-top:10px" class="muted">合计：借 <b id="v-dt">0.00</b> 　贷 <b id="v-ct">0.00</b> 　差额 <b id="v-diff">0.00</b></div>
    <div id="v-attach" class="muted" style="margin-top:8px">附件加载中…</div>
    <div class="foot">
      ${editable ? `<button class="btn" id="v-save">保存</button>` : ""}
      ${can("voucher_audit") && id > 0 && status === "draft" ? `<button class="btn ghost" id="v-audit">审核</button>` : ""}
      ${can("voucher_unaudit") && id > 0 && status === "audited" ? `<button class="btn ghost" id="v-unaudit">反审核</button>` : ""}
      ${can("voucher_post") && canPost ? `<button class="btn primary" id="v-post">记账</button>` : ""}
      ${can("voucher_unpost") && status === "posted" ? `<button class="btn ghost" id="v-unpost">反记账</button>` : ""}
      ${can("voucher_new") && id > 0 && status !== "void" ? `<button class="btn ghost" id="v-reverse">红字冲销</button>` : ""}
      ${can("voucher_delete") && (status === "draft" || status === "audited") ? `<button class="btn danger" id="v-del">删除</button>` : ""}
      <button class="btn ghost" id="v-close">关闭</button>
    </div>
  `, true);

  const tbody = $("#v-entries tbody", mask);
  let expanded = -1;
  function acctOf(code) { return (state.accounts || []).find((a) => a.code === code); }
  function auxMissing(e) {
    const a = acctOf(e.account_code);
    if (!a) return false;
    for (const [k, bit] of AUX_DEFS) {
      if ((a.aux & bit) && !(e.aux && e.aux[k])) return true;
    }
    if (a.has_qty && !e.qty) return true;
    return false;
  }
  function auxDetailHtml(e, i) {
    const a = acctOf(e.account_code);
    const mask = (a && a.aux) || 0;
    const parts = [];
    for (const [k, bit, label] of AUX_DEFS) {
      if (!(mask & bit)) continue;
      parts.push(`<label>${label}* <input class="aux-in" data-i="${i}" data-k="${k}" value="${esc((e.aux || {})[k] || "")}" style="width:140px" /></label>`);
    }
    if (a && a.has_qty) {
      parts.push(`<label>数量 <input class="aux-in" data-i="${i}" data-k="qty" value="${esc(e.qty)}" style="width:90px" /></label>`);
      parts.push(`<label>单价 <input class="aux-in" data-i="${i}" data-k="price" value="${esc(e.price)}" style="width:90px" /></label>`);
    }
    if (a && (a.is_cash || a.is_bank)) {
      parts.push(`<label>现金流量项目 <input class="aux-in" data-i="${i}" data-k="cf" value="${esc(e.cf)}" placeholder="如 0101" style="width:100px" /></label>`);
    }
    if (a && a.currency) {
      parts.push(`<label>币种 <input class="aux-in" data-i="${i}" data-k="currency" value="${esc(e.currency || a.currency)}" style="width:64px" /></label>`);
      parts.push(`<label>汇率 <input class="aux-in" data-i="${i}" data-k="rate" value="${esc(e.rate)}" placeholder="1 外币=?" style="width:90px" /></label>`);
      parts.push(`<label>原币金额 <input class="aux-in" data-i="${i}" data-k="amount_for" value="${esc(e.amount_for)}" style="width:100px" /></label>`);
    }
    if (!parts.length) parts.push(`<span class="muted">该科目无需辅助核算/数量</span>`);
    return `<div class="muted" style="padding:6px 2px"><span style="font-size:12px">${parts.join(" ")}</span>${editable ? ` <button class="btn ghost sm" id="v-aux-close">收起</button>` : ""}</div>`;
  }
  function renderRows() {
    tbody.innerHTML = v.entries.map((e, i) => {
      const miss = auxMissing(e);
      const hasAux = (e.aux && Object.keys(e.aux).some((k) => e.aux[k])) || e.qty || e.cf;
      const detail = expanded === i ? `<tr><td colspan="7" style="background:rgba(0,0,0,0.03)">${auxDetailHtml(e, i)}</td></tr>` : "";
      return `<tr>
      <td>${e.line}</td>
      <td>${accountOptions()}</td>
      <td><input class="e-sum" value="${esc(e.summary)}" style="width:100%" ${editable ? "" : "disabled"} /></td>
      <td class="num"><input class="e-d num" value="${esc(e.debit)}" style="width:110px;text-align:right" ${editable ? "" : "disabled"} /></td>
      <td class="num"><input class="e-c num" value="${esc(e.credit)}" style="width:110px;text-align:right" ${editable ? "" : "disabled"} /></td>
      <td><button class="btn ghost sm e-aux" title="辅助核算/数量/现金流量" ${editable ? "" : "disabled"} style="${miss ? "color:var(--err)" : ""}">${miss ? "补录!" : (hasAux ? "已填" : "⋯")}</button></td>
      <td>${editable ? `<button class="btn ghost sm e-del">×</button>` : ""}</td>
    </tr>${detail}`;
    }).join("");
    $all("select.acct-sel", tbody).forEach((sel, i) => { sel.value = v.entries[i].account_code; sel.onchange = () => { v.entries[i].account_code = sel.value; renderRows(); }; });
    $all(".e-sum", tbody).forEach((inp, i) => inp.oninput = () => v.entries[i].summary = inp.value);
    $all(".e-d", tbody).forEach((inp, i) => inp.oninput = () => { v.entries[i].debit = inp.value; recalc(); });
    $all(".e-c", tbody).forEach((inp, i) => inp.oninput = () => { v.entries[i].credit = inp.value; recalc(); });
    $all(".e-del", tbody).forEach((b, i) => b.onclick = () => { v.entries.splice(i, 1); v.entries.forEach((e, k) => e.line = k + 1); expanded = -1; renderRows(); recalc(); });
    $all(".e-aux", tbody).forEach((b, i) => b.onclick = () => { expanded = (expanded === i ? -1 : i); renderRows(); });
    $all(".aux-in", tbody).forEach((inp) => {
      inp.oninput = () => {
        const i = parseInt(inp.dataset.i, 10), k = inp.dataset.k;
        const e = v.entries[i];
        if (k === "qty" || k === "price" || k === "cf" || k === "currency" || k === "rate" || k === "amount_for") { e[k] = inp.value; }
        else { e.aux = e.aux || {}; e.aux[k] = inp.value; }
      };
    });
    const closeAux = $("#v-aux-close", tbody);
    if (closeAux) closeAux.onclick = () => { expanded = -1; renderRows(); };
    recalc();
  }
  function recalc() {
    const sum = (arr, k) => arr.reduce((a, e) => a + (parseFloat(e[k]) || 0), 0);
    const dt = sum(v.entries, "debit"), ct = sum(v.entries, "credit");
    $("#v-dt", mask).textContent = dt.toFixed(2);
    $("#v-ct", mask).textContent = ct.toFixed(2);
    $("#v-diff", mask).textContent = (dt - ct).toFixed(2);
  }
  renderRows();
  if (editable) $("#v-add", mask).onclick = () => { v.entries.push(Object.assign({}, blank, { line: v.entries.length + 1 })); renderRows(); };
  if (editable && $("#v-today", mask)) $("#v-today", mask).onclick = () => { $("#v-date", mask).value = today(); updateDateHint(); };
  // 跨期提示：所选日期与当前期间不一致时提前告知（保存时会按日期归入对应期间）
  function updateDateHint() {
    const el = $("#v-date-hint", mask);
    if (!el) return;
    const dv = $("#v-date", mask).value;
    if (dv && state.current && ymm(dv.slice(0, 7)) !== ymm(state.current)) {
      el.textContent = `该日期属于 ${dv.slice(0, 7)} 期，保存后将归入该期间`;
    } else {
      el.textContent = "";
    }
  }
  if (editable) $("#v-date", mask).addEventListener("change", updateDateHint);

  const save = async () => {
    const dateVal = $("#v-date", mask).value;
    if (!/^\d{4}-\d{2}-\d{2}$/.test(dateVal)) { toast("日期格式应为 YYYY-MM-DD", "err"); return; }
    const payload = {
      id: v.id,
      // 期间取业务日期所属月份（YYYYMM）；编辑已有凭证时后端以原期间为准
      period: ymm(dateVal.slice(0, 7)),
      date: dateVal,
      word: $("#v-word", mask).value,
      no: parseInt($("#v-no", mask).value, 10) || 0,
      attachments: parseInt($("#v-att", mask).value, 10) || 0,
      memo: "",
      entries: v.entries.map((e, i) => {
        const o = { line: i + 1, account_code: e.account_code, summary: e.summary, debit: String(parseFloat(e.debit) || 0), credit: String(parseFloat(e.credit) || 0) };
        const aux = {};
        for (const k of Object.keys(e.aux || {})) { if (e.aux[k]) aux[k] = e.aux[k]; }
        if (Object.keys(aux).length) o.aux = aux;
        if (e.qty) o.qty = String(e.qty);
        if (e.price) o.price = String(e.price);
        if (e.currency) o.currency = String(e.currency);
        if (e.rate) o.rate = String(e.rate);
        if (e.amount_for) o.amount_for = String(e.amount_for);
        if (e.cf) o.cf = e.cf;
        return o;
      }),
    };
    if (!payload.entries.some((e) => e.account_code)) { toast("请至少选择一条科目", "err"); return; }
    try {
      await api("/vouchers", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
      toast("已保存", "ok"); closeModal(); loadVouchers();
    } catch (e) { toast(e.message, "err"); }
  };
  if (editable) $("#v-save", mask).onclick = save;
  if ($("#v-post", mask)) $("#v-post", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/post`, { method: "POST" }); toast("已记账", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-unpost", mask)) $("#v-unpost", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/unpost`, { method: "POST" }); toast("已反记账，凭证可修改", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-audit", mask)) $("#v-audit", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/audit`, { method: "POST" }); toast("已审核", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-unaudit", mask)) $("#v-unaudit", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/unaudit`, { method: "POST" }); toast("已反审核，凭证可修改", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-reverse", mask)) $("#v-reverse", mask).onclick = async () => { if (!(await confirmDialog("生成该凭证的红字冲销凭证（借贷互换、摘要加「冲销」前缀），原凭证保留不动？", true))) return; try { await api(`/vouchers/${v.id}/reverse`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(state.current || ""), date: today() }) }); toast("已生成冲销凭证", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-del", mask)) $("#v-del", mask).onclick = async () => { if (!(await confirmDialog("确定删除该凭证？", true))) return; try { await api(`/vouchers/${v.id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  async function loadAttachments() {
    const box = $("#v-attach", mask);
    if (!box) return;
    if (!v.id) { box.innerHTML = `<span class="muted">保存凭证后可上传附件（单据影像等）</span>`; return; }
    let list = [];
    try { list = await api(`/vouchers/${v.id}/attachments`); }
    catch (e) { box.innerHTML = `<span class="muted">${esc(e.message)}</span>`; return; }
    const canEdit = can("voucher_edit");
    box.innerHTML = `<b>附件</b> ${list.length
      ? `<ul style="margin:6px 0 0 16px">${list.map((a) => `<li>${esc(a.name)} <span class="muted">(${esc(a.size_text)} ${esc(a.added_by || "")})</span> <a href="/api/attachments/${a.id}" target="_blank">下载</a> ${canEdit ? `<button class="btn sm ghost" data-attdel="${a.id}">删除</button>` : ""}</li>`).join("")}</ul>`
      : `<span class="muted">暂无附件</span>`}
      ${canEdit ? `<div style="margin-top:6px"><input type="file" id="v-attfile" /> <button class="btn sm" id="v-attup">上传</button> <span class="muted" style="font-size:12px">单文件 ≤ 10MB</span></div>` : ""}`;
    if ($("#v-attup", box)) $("#v-attup", box).onclick = async () => {
      const f = $("#v-attfile", box).files[0];
      if (!f) { toast("请选择文件", "err"); return; }
      if (f.size > 10 * 1024 * 1024) { toast("文件超过 10MB 上限", "err"); return; }
      const fd = new FormData();
      fd.append("file", f, f.name);
      try {
        const resp = await fetch(`/api/vouchers/${v.id}/attachments`, { method: "POST", body: fd });
        if (!resp.ok) { let msg = resp.statusText; try { msg = (await resp.json()).error || msg; } catch (e) {} throw new Error(msg); }
        toast("已上传", "ok"); loadAttachments();
      } catch (e) { toast(e.message, "err"); }
    };
    $all("[data-attdel]", box).forEach((b) => b.onclick = async () => {
      if (!(await confirmDialog("删除该附件？", true))) return;
      try { await api(`/attachments/${b.dataset.attdel}`, { method: "DELETE" }); toast("已删除", "ok"); loadAttachments(); } catch (e) { toast(e.message, "err"); }
    });
  }
  loadAttachments();
  $("#v-close", mask).onclick = closeModal;
}
function today() { const d = new Date(); const p = (n) => String(n).padStart(2, "0"); return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`; }

// ===========================================================================
// 发票管理
// ===========================================================================
let invoiceCache = null;

async function loadInvoices(filter) {
  const q = new URLSearchParams();
  if (filter && filter.kind) q.set("kind", filter.kind);
  if (filter && filter.status) q.set("status", filter.status);
  if (filter && filter.keyword) q.set("keyword", filter.keyword);
  const s = q.toString();
  const data = await api(`/invoices${s ? "?" + s : ""}`);
  // 汇总卡片：单独拉取（始终全量）
  let summary = {};
  try { summary = await api("/invoices/summary"); } catch (e) {}
  data.summary = (summary && summary.by_kind) || {};
  invoiceCache = data;
  return data;
}

async function viewInvoices(main) {
  main.innerHTML = `<h2>发票管理</h2><div class="muted">加载中…</div>`;
  let d;
  try { d = await loadInvoices(); } catch (e) { main.innerHTML = `<h2>发票管理</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  renderInvoices(main, d);
}

function renderInvoices(main, d) {
  const rows = (d && d.rows) || [];
  const sum = (d && d.summary) || {};
  const statusBadge = (s) => {
    if (s === "verified") return `<span class="tag ok">已认证</span>`;
    if (s === "rejected") return `<span class="tag err">已作废</span>`;
    return `<span class="tag warn">待认证</span>`;
  };
  const kindLabel = (k) => (k === "out" ? "销项" : "进项");
  main.innerHTML = `
    <h2>发票管理</h2>
    <div class="cards" style="margin-bottom:14px">
      <div class="card"><div class="k">发票总数</div><div class="v">${rows.length}</div></div>
      <div class="card"><div class="k">进项价税合计</div><div class="v" style="font-size:18px">${esc((sum.in && sum.in.amount_tax) || "0.00")}</div></div>
      <div class="card"><div class="k">销项价税合计</div><div class="v" style="font-size:18px">${esc((sum.out && sum.out.amount_tax) || "0.00")}</div></div>
    </div>
    <div class="toolbar">
      <input id="inv-kw" placeholder="号码 / 代码 / 购销方" style="width:180px" />
      <select id="inv-kind">
        <option value="">全部类型</option>
        <option value="in">进项</option>
        <option value="out">销项</option>
      </select>
      <select id="inv-status">
        <option value="">全部状态</option>
        <option value="pending">待认证</option>
        <option value="verified">已认证</option>
        <option value="rejected">已作废</option>
      </select>
      <button class="btn" id="inv-query">查询</button>
      <div class="spacer"></div>
      ${can("voucher_new") ? `<button class="btn primary" id="inv-new">新增发票</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:hidden">
      <table class="grid">
        <thead><tr>
          <th>类型</th><th>发票号码</th><th>开票日期</th><th>购买方</th><th>销售方</th>
          <th class="num">不含税</th><th class="num">税额</th><th class="num">价税合计</th><th>状态</th><th></th>
        </tr></thead>
        <tbody>
          ${rows.length ? rows.map((r) => `
            <tr>
              <td>${kindLabel(r.kind)}</td>
              <td>${esc(r.number)}</td>
              <td>${esc(r.date)}</td>
              <td>${esc(r.buyer)}</td>
              <td>${esc(r.seller)}</td>
              <td class="num">${esc(r.amount)}</td>
              <td class="num">${esc(r.tax)}</td>
              <td class="num">${esc(r.amount_tax)}</td>
              <td>${statusBadge(r.status)}</td>
              <td class="row-actions">
                ${can("voucher_edit") ? `<button class="btn sm ghost" data-act="edit" data-id="${r.id}">编辑</button>` : ""}
                ${r.status === "pending" && can("voucher_edit") ? `<button class="btn sm ghost" data-act="verify" data-id="${r.id}">认证</button>` : ""}
                ${r.status !== "rejected" && can("voucher_edit") ? `<button class="btn sm ghost" data-act="reject" data-id="${r.id}">作废</button>` : ""}
                ${can("voucher_delete") ? `<button class="btn sm ghost" data-act="del" data-id="${r.id}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="10" class="muted" style="text-align:center;padding:18px">暂无发票</td></tr>`}
        </tbody>
      </table>
    </div>`;
  $("#inv-query").addEventListener("click", async () => {
    const d2 = await loadInvoices({ kind: $("#inv-kind").value, status: $("#inv-status").value, keyword: $("#inv-kw").value });
    renderInvoices(main, d2);
  });
  if ($("#inv-new")) $("#inv-new").addEventListener("click", () => openInvoiceEditor(main, null));
  $all("[data-act]", main).forEach((b) => b.addEventListener("click", async () => {
    const id = parseInt(b.dataset.id, 10);
    const act = b.dataset.act;
    try {
      if (act === "edit") {
        const list = (await loadInvoices()).rows || [];
        const inv = list.find((x) => x.id === id);
        if (inv) openInvoiceEditor(main, inv);
      } else if (act === "verify") {
        const up = await api(`/invoices/${id}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: "verified" }) });
        if (up) { toast("已认证", "ok"); renderInvoices(main, await loadInvoices()); }
      } else if (act === "reject") {
        if (!(await confirmDialog("确定作废该发票？", true))) return;
        const up = await api(`/invoices/${id}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: "rejected" }) });
        if (up) { toast("已作废", "ok"); renderInvoices(main, await loadInvoices()); }
      } else if (act === "del") {
        if (!(await confirmDialog("确定删除该发票？", true))) return;
        await api(`/invoices/${id}`, { method: "DELETE" });
        toast("已删除", "ok");
        renderInvoices(main, await loadInvoices());
      }
    } catch (e) { toast(e.message, "err"); }
  }));
}

function openInvoiceEditor(main, inv) {
  const isEdit = !!inv;
  const mask = modal(`
    <h3>${isEdit ? "编辑发票" : "新增发票"}</h3>
    <div class="field"><label>类型</label>
      <select id="inv-kind2">
        <option value="in" ${!isEdit || inv.kind === "in" ? "selected" : ""}>进项</option>
        <option value="out" ${isEdit && inv.kind === "out" ? "selected" : ""}>销项</option>
      </select>
    </div>
    <div class="field"><label>发票代码</label><input id="inv-code" value="${esc(inv ? inv.code : "")}" /></div>
    <div class="field"><label>发票号码 *</label><input id="inv-number" value="${esc(inv ? inv.number : "")}" /></div>
    <div class="field"><label>开票日期</label><input id="inv-date" value="${esc(inv ? inv.date : today())}" /></div>
    <div class="field"><label>购买方</label><input id="inv-buyer" value="${esc(inv ? inv.buyer : "")}" /></div>
    <div class="field"><label>销售方</label><input id="inv-seller" value="${esc(inv ? inv.seller : "")}" /></div>
    <div class="field"><label>价税合计</label><input id="inv-amt" value="${esc(inv ? inv.amount_tax : "0")}" /></div>
    <div class="field"><label>不含税金额</label><input id="inv-amount" value="${esc(inv ? inv.amount : "0")}" /></div>
    <div class="field"><label>税额</label><input id="inv-tax" value="${esc(inv ? inv.tax : "0")}" /></div>
    <div class="field"><label>税率（如 0.13）</label><input id="inv-rate" value="${esc(inv ? inv.tax_rate : "0")}" /></div>
    <div class="field"><label>备注</label><input id="inv-memo" value="${esc(inv ? inv.memo : "")}" /></div>
    <div class="foot">
      <button class="btn ghost" id="inv-close">取消</button>
      <button class="btn primary" id="inv-save">保存</button>
    </div>
  `, true);
  $("#inv-close").addEventListener("click", closeModal);
  $("#inv-save").addEventListener("click", async () => {
    const number = $("#inv-number").value.trim();
    if (!number) { toast("发票号码必填", "err"); return; }
    const body = {
      id: isEdit ? inv.id : 0,
      kind: $("#inv-kind2").value,
      code: $("#inv-code").value.trim(),
      number,
      date: $("#inv-date").value.trim(),
      buyer: $("#inv-buyer").value.trim(),
      seller: $("#inv-seller").value.trim(),
      amount_tax: $("#inv-amt").value.trim() || "0",
      amount: $("#inv-amount").value.trim() || "0",
      tax: $("#inv-tax").value.trim() || "0",
      tax_rate: $("#inv-rate").value.trim() || "0",
      status: isEdit ? inv.status : "pending",
      memo: $("#inv-memo").value.trim(),
    };
    try {
      if (isEdit) {
        await api(`/invoices/${inv.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok");
      } else {
        await api("/invoices", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已新增", "ok");
      }
      closeModal();
      renderInvoices(main, await loadInvoices());
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 数据导入（其他软件 / CSV / Excel）
// ===========================================================================
async function viewImports(main) {
  main.innerHTML = `
    <h2>数据导入</h2>
    <div class="panel">
      <p class="muted" style="margin:0 0 12px;line-height:1.6">
        从其他财务软件（金蝶 / 用友）或 Excel 导入数据。
        支持导入 <b>期初余额表</b> 与 <b>记账凭证</b>；遇到账套里没有的科目可手动映射。
      </p>
      <div class="toolbar" style="box-shadow:none;border:none;padding:0;margin:0">
        <label>导入类型</label>
        <select id="imp-kind">
          <option value="begin">期初余额表</option>
          <option value="voucher">记账凭证</option>
        </select>
        <label>来源模板</label>
        <select id="imp-template">
          <option value="generic">通用</option>
          <option value="kingdee">金蝶</option>
          <option value="yonyou">用友</option>
        </select>
        <label>期间（凭证用，YYYYMM）</label>
        <input id="imp-period" placeholder="202601" value="${state.current ? String(state.current).replace('-','') : ""}" style="width:90px" />
        <div class="spacer"></div>
        <button class="btn primary" id="imp-analyze">预检科目</button>
        <button class="btn" id="imp-run">执行导入</button>
      </div>
      <div style="margin-top:10px">
        <label style="font-weight:600">选择 Excel 文件（.xlsx / .xls / .ods，可选）</label>
        <input type="file" id="imp-file" accept=".xlsx,.xls,.ods" style="display:block;margin:4px 0 8px" />
        <textarea id="imp-text" rows="8" placeholder="或直接粘贴 CSV 内容…
通用期初：科目编码, 方向(借/贷), 金额
通用凭证：日期, 凭证字, 摘要, 科目编码, 借方, 贷方"></textarea>
      </div>
      <div id="imp-result" class="muted" style="margin-top:10px;min-height:20px;white-space:pre-wrap;font-size:13px"></div>
    </div>
    <div class="panel" id="imp-mapping-wrap" style="display:none">
      <h3 style="margin:0 0 10px">缺失科目映射</h3>
      <p class="muted" style="margin:0 0 10px">以下科目在当前账套中不存在，请为每个选择目标科目（留空 = 忽略该科目对应行）。</p>
      <div id="imp-mapping"></div>
    </div>`;
  let mapping = {};
  let fileB64 = "";
  // 加载科目列表供映射下拉
  let accounts = [];
  try { accounts = await api("/accounts"); } catch (e) {}
  const acctOpts = (sel) => accounts.map((a) => `<option value="${esc(a.code)}" ${sel === a.code ? "selected" : ""}>${esc(a.code)} ${esc(a.name)}</option>`).join("");

  // Excel 文件 → base64
  $("#imp-file").addEventListener("change", (ev) => {
    const f = ev.target.files && ev.target.files[0];
    if (!f) { fileB64 = ""; return; }
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = String(reader.result || "");
      fileB64 = dataUrl.split(",").slice(1).join(","); // 去掉 data:...;base64, 前缀
      $("#imp-result").textContent = `已选择文件：${f.name}（${(f.size / 1024).toFixed(1)} KB），点击「预检科目」或「执行导入」。`;
    };
    reader.readAsDataURL(f);
  });

  $("#imp-analyze").addEventListener("click", async () => {
    const text = $("#imp-text").value;
    const kind = $("#imp-kind").value;
    const template = $("#imp-template").value;
    if (!text.trim() && !fileB64) { toast("请粘贴 CSV 内容或选择 Excel 文件", "err"); return; }
    try {
      const r = await api("/import/analyze", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind, text, template, file: fileB64 || null }) });
      const missing = (r && r.missing) || [];
      const wrap = $("#imp-mapping-wrap");
      const box = $("#imp-mapping");
      if (missing.length === 0) {
        wrap.style.display = "none";
        $("#imp-result").textContent = "✅ 预检通过：所有科目在账套中均存在，可直接执行导入。";
        return;
      }
      box.innerHTML = missing.map((m) => `
        <div style="display:flex;gap:8px;align-items:center;margin-bottom:6px">
          <span style="min-width:140px;font-family:monospace">${esc(m.code)} <span class="muted">×${m.count}</span></span>
          <select class="imp-map" data-code="${esc(m.code)}" style="flex:1">
            <option value="">— 忽略 —</option>
            ${acctOpts("")}
          </select>
        </div>`).join("");
      wrap.style.display = "";
      $("#imp-result").textContent = `找到 ${missing.length} 个缺失科目，请选择映射或忽略。`;
      $all(".imp-map", box).forEach((s) => s.addEventListener("change", () => {
        mapping[s.dataset.code] = s.value;
      }));
    } catch (e) { $("#imp-result").textContent = "预检失败：" + e.message; }
  });

  $("#imp-run").addEventListener("click", async () => {
    const text = $("#imp-text").value;
    const kind = $("#imp-kind").value;
    const template = $("#imp-template").value;
    const period = parseInt(($("#imp-period").value || "0").replace(/[^0-9]/g, ""), 10) || 0;
    if (!text.trim() && !fileB64) { toast("请粘贴 CSV 内容或选择 Excel 文件", "err"); return; }
    try {
      const r = await api("/import/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind, text, template, file: fileB64 || null, period, mapping }) });
      const lines = [`✅ 成功导入 ${r.ok} 条`, r.skipped ? `⚠ 跳过 ${r.skipped} 条` : ""].filter(Boolean);
      if (r.warnings && r.warnings.length) {
        lines.push("", "警告：");
        r.warnings.slice(0, 20).forEach((w) => lines.push("  · " + w));
        if (r.warnings.length > 20) lines.push(`  …共 ${r.warnings.length} 条警告`);
      }
      $("#imp-result").textContent = lines.join("\n");
      toast(`已导入 ${r.ok} 条`, "ok");
      state.view = "dashboard";
    } catch (e) { $("#imp-result").textContent = "导入失败：" + e.message; }
  });
}

// ===========================================================================
// 明细账
// ===========================================================================
let ledgerTab = "detail";
async function viewLedger(main) {
  await ensureAccounts();
  main.innerHTML = `
    <h2>账簿查询</h2>
    <div class="toolbar">
      <label>科目 <input id="l-code" list="acct-list" placeholder="科目编码，如 1002" style="width:160px" /></label>
      <datalist id="acct-list">${(state.accounts || []).map((a) => `<option value="${esc(a.code)}">${esc(a.name)}</option>`).join("")}</datalist>
      <label>从 <input id="l-from" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label>至 <input id="l-to" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label><input type="checkbox" id="l-children" checked /> 含下级</label>
      <label><input type="checkbox" id="l-posted" /> 仅已记账</label>
      <button class="btn sm" id="l-go">查询</button>
      <div class="spacer"></div>
      <button class="btn ghost sm" data-ltab="detail">明细账</button>
      <button class="btn ghost sm" data-ltab="general">总账</button>
      <button class="btn ghost sm" data-ltab="journal">日记账</button>
      <button class="btn ghost sm" id="l-print">打印预览</button>
      <button class="btn ghost sm" id="l-printform">套打</button>
      ${can("export") ? `<button class="btn ghost sm" id="l-export">导出 CSV</button>` : ""}
    </div>
    <div class="panel"><table class="grid" id="l-table"><thead></thead><tbody><tr><td class="muted">请输入科目后查询</td></tr></tbody></table></div>`;
  const setTab = (t) => {
    ledgerTab = t;
    $all("[data-ltab]", main).forEach((b) => b.classList.toggle("primary", b.dataset.ltab === t));
  };
  setTab("detail");
  $all("[data-ltab]", main).forEach((b) => b.onclick = () => { setTab(b.dataset.ltab); loadLedger(); });
  $("#l-go").addEventListener("click", loadLedger);
  $("#l-print").addEventListener("click", () => {
    const el = $("#l-table").querySelector("table");
    printPreview(({ detail: "明细账", general: "总账", journal: "日记账" })[ledgerTab] || "账簿", el);
  });
  $("#l-printform").addEventListener("click", () => {
    const code = $("#l-code").value.trim();
    if (!code) { toast("请先输入科目编码", "err"); return; }
    const q = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}&type=${ledgerTab}`;
    window.open(`/api/ledger/print-form?${q}`, "_blank");
  });
  if ($("#l-export")) $("#l-export").onclick = () => {
    const code = $("#l-code").value.trim();
    if (!code) { toast("请先输入科目编码", "err"); return; }
    const q = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}`;
    window.open(`/api/export/ledger?${q}`, "_blank");
  };
}
async function loadLedger() {
  const code = $("#l-code").value.trim();
  if (!code) { toast("请先输入科目编码", "err"); return; }
  const qs = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}`;
  const url = ledgerTab === "general" ? `/ledger/general?${qs}` : ledgerTab === "journal" ? `/ledger/journal?${qs}` : `/ledger?${qs}`;
  const tbl = $("#l-table");
  const message = (text, isErr) => {
    tbl.innerHTML = `<tbody><tr><td colspan="8" ${isErr ? 'style="color:var(--err)"' : 'class="muted"'}>${esc(text)}</td></tr></tbody>`;
  };
  let rows;
  try { rows = await api(url); } catch (e) { message(e.message, true); return; }
  if (!rows.length) { message("该科目在所选期间无记录", false); return; }
  if (ledgerTab === "general") {
    tbl.innerHTML = `<thead><tr><th>期间</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th></tr></thead><tbody>${rows.map((r) => `<tr><td>${esc(r.period)}</td><td>${esc(r.summary)}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td></tr>`).join("")}</tbody>`;
    return;
  }
  if (ledgerTab === "journal") {
    tbl.innerHTML = `<thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th>对方科目</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th></tr></thead><tbody>${rows.map((r) => `<tr><td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td><td>${esc(r.opposite_accounts || "")}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td></tr>`).join("")}</tbody>`;
    return;
  }
  const hasQty = rows.some((r) => r.qty_balance != null);
  const head = `<tr><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th>${hasQty ? '<th class="num">数量余额</th>' : ""}</tr>`;
  const body = rows.map((r) => `<tr>
    <td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td>
    <td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td>
    <td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td>
    ${hasQty ? `<td class="num" style="color:var(--err,#c62828)">${r.qty_balance != null ? esc(fmt(r.qty_balance)) : ""}</td>` : ""}
  </tr>`).join("");
  tbl.innerHTML = `<thead>${head}</thead><tbody>${body}</tbody>`;
}

// ===========================================================================
// 报表中心（打印 vs 导出 权限区分）
// ===========================================================================
async function viewReports(main) {
  main.innerHTML = `
    <h2>报表中心</h2>
    <div class="toolbar">
      <label>从 <input id="r-from" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label>至 <input id="r-to" value="${esc(state.current || "")}" style="width:90px" /></label>
      <button class="btn sm" id="r-go">生成科目余额表</button>
      <button class="btn ghost sm" id="r-aux">辅助账</button>
      <label>维度 <select id="r-auxkind">
        <option value="customer">客户</option><option value="supplier">供应商</option>
        <option value="dept">部门</option><option value="employee">职员</option>
        <option value="project">项目</option><option value="item">存货</option>
        <option value="bank">银行账户</option>
      </select></label>
      <button class="btn ghost sm" id="r-qty">数量金额账</button>
      <span class="spacer"></span>
      ${can("export") ? `<button class="btn ghost sm" id="r-export">导出 CSV</button>
      <button class="btn ghost sm" id="r-pdf">导出 PDF</button>` : `<span class="tag warn" title="无导出权限">无导出权限，仅可打印</span>`}
      <button class="btn ghost sm" id="r-print">打印预览</button>
    </div>
    <div class="panel"><table class="grid" id="r-table"><thead><tr>
      <th>科目编码</th><th>科目名称</th><th>方向</th><th class="num">期初</th><th class="num">本期借方</th><th class="num">本期贷方</th><th class="num">期末</th><th class="num">本年累计借方</th><th class="num">本年累计贷方</th>
    </tr></thead><tbody><tr><td colspan="9" class="muted">点击「生成科目余额表」</td></tr></tbody></table></div>
    <div id="r-extra" style="margin-top:10px"></div>`;
  $("#r-go").addEventListener("click", loadTrial);
  $("#r-print").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.open(`/api/reports/trial-balance/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  if (can("export")) {
    $("#r-export").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.location = `/api/reports/trial-balance/export?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`; });
    $("#r-pdf").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.location = `/api/reports/trial-balance/pdf?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`; });
  }
  $("#r-aux").addEventListener("click", async () => {
    const kind = $("#r-auxkind").value;
    const qs = new URLSearchParams({ kind, from: $("#r-from").value, to: $("#r-to").value });
    try {
      const d = await api(`/reports/aux-balance?${qs.toString()}`);
      const rows = d.rows || [];
      $("#r-extra").innerHTML = `<div class="panel"><b>${esc(d.kind_label)}辅助账（${esc(d.from)} ~ ${esc(d.to)}）</b>
        <button class="btn ghost sm" style="float:right" id="r-aux-print">打印预览</button>
        <table class="grid" id="r-aux-table" style="margin-top:6px"><thead><tr><th>${esc(d.kind_label)}</th><th class="num">期初</th><th class="num">本期借方</th><th class="num">本期贷方</th><th class="num">期末</th></tr></thead><tbody>${rows.length ? rows.map((r) => `<tr><td>${esc(r.key)}</td><td class="num">${esc(r.begin)}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td class="num">${esc(r.end)}</td></tr>`).join("") : `<tr><td colspan="5" class="muted">无数据</td></tr>`}</tbody></table></div>`;
      $("#r-aux-print").onclick = () => printPreview(`${d.kind_label}辅助账`, $("#r-aux-table"));
    } catch (e) { toast(e.message, "err"); }
  });
  $("#r-qty").addEventListener("click", async () => {
    const qs = new URLSearchParams({ from: $("#r-from").value, to: $("#r-to").value });
    try {
      const d = await api(`/reports/qty-balance?${qs.toString()}`);
      const rows = d.rows || [];
      $("#r-extra").innerHTML = `<div class="panel"><b>数量金额账（${esc(d.from)} ~ ${esc(d.to)}）</b>
        <button class="btn ghost sm" style="float:right" id="r-qty-print">打印预览</button>
        <table class="grid" id="r-qty-table" style="margin-top:6px"><thead><tr><th>科目编码</th><th>科目名称</th><th class="num">期初数量</th><th class="num">入库数量</th><th class="num">出库数量</th><th class="num">期末数量</th><th class="num">期初金额</th><th class="num">借方金额</th><th class="num">贷方金额</th><th class="num">期末金额</th></tr></thead><tbody>${rows.length ? rows.map((r) => `<tr><td>${esc(r.account_code)}</td><td>${esc(r.account_name)}</td><td class="num">${esc(r.qty_begin)}</td><td class="num">${esc(r.qty_in)}</td><td class="num">${esc(r.qty_out)}</td><td class="num">${esc(r.qty_end)}</td><td class="num">${esc(r.amount_begin)}</td><td class="num">${esc(r.amount_debit)}</td><td class="num">${esc(r.amount_credit)}</td><td class="num">${esc(r.amount_end)}</td></tr>`).join("") : `<tr><td colspan="10" class="muted">无数量核算科目数据</td></tr>`}</tbody></table></div>`;
      $("#r-qty-print").onclick = () => printPreview("数量金额账", $("#r-qty-table"));
    } catch (e) { toast(e.message, "err"); }
  });
}
async function loadTrial() {
  const f = $("#r-from").value, t = $("#r-to").value;
  const tb = $("#r-table tbody");
  let data;
  try { data = await api(`/reports/trial-balance?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`); } catch (e) { tb.innerHTML = `<tr><td colspan="9" style="color:var(--err)">${esc(e.message)}</td></tr>`; return; }
  const rows = data.rows || [];
  if (!rows.length) { tb.innerHTML = `<tr><td colspan="9" class="muted">无数据</td></tr>`; return; }
  tb.innerHTML = rows.map((r) => {
    // 后端 TrialRow：begin_dir/end_dir/begin/end/debit/credit/ytd_* 均为已格式化字符串
    return `<tr>
      <td>${esc(r.account_code)}</td><td>${esc(r.account_name)}</td>
      <td>${esc(r.end_dir)}</td>
      <td class="num">${esc(r.begin_dir)} ${esc(r.begin)}</td>
      <td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td>
      <td class="num">${esc(r.end_dir)} ${esc(r.end)}</td>
      <td class="num">${esc(r.ytd_debit)}</td><td class="num">${esc(r.ytd_credit)}</td>
    </tr>`;
  }).join("");
  // 合计行（借贷平衡校验参考）
  if (data.totals) {
    const t = data.totals;
    tb.innerHTML += `<tr style="background:#fafafa;font-weight:600">
      <td colspan="3">合计</td>
      <td class="num">借 ${esc(t.begin_debit)} / 贷 ${esc(t.begin_credit)}</td>
      <td class="num">${esc(t.debit)}</td><td class="num">${esc(t.credit)}</td>
      <td class="num">借 ${esc(t.end_debit)} / 贷 ${esc(t.end_credit)}</td>
      <td class="num">${esc(t.ytd_debit || "—")}</td><td class="num">${esc(t.ytd_credit || "—")}</td>
    </tr>`;
  }
}

// ===========================================================================
// 通用打印预览：把页面里的表格渲染成可打印 HTML（新窗口，自动弹打印）
// 数据只走内存，不落地文件；任何有 <table> 结果的报表页都能复用。
// ===========================================================================
function printPreview(title, tableEl) {
  if (!tableEl || !tableEl.outerHTML) { toast("没有可打印的数据", "err"); return; }
  const html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>${esc(title)}</title>
    <style>body{font-family:-apple-system,'Microsoft YaHei',sans-serif;color:#222;margin:16px;}
    h2{text-align:center;margin:8px 0;}
    .meta{display:flex;justify-content:space-between;color:#666;font-size:13px;margin-bottom:4px;}
    table{border-collapse:collapse;width:100%;font-size:13px;}
    th,td{border:1px solid #bbb;padding:4px 8px;}
    th{background:#f0f3f7;}td.r,td.num{text-align:right;}
    @media print{body{font-size:12px;margin:0;}}</style></head>
    <body><h2>${esc(title)}</h2>
    <div class="meta"><span>${esc(session.user ? session.user.display_name : "")}</span><span>打印时间：${esc(today())}</span></div>
    ${tableEl.outerHTML}
    <script>window.onload=function(){setTimeout(function(){window.print();},300);};</scr${"ipt"}>
    </body></html>`;
  const w = window.open("", "_blank");
  if (!w) { toast("浏览器拦截了打印窗口，请允许弹出窗口", "err"); return; }
  w.document.write(html);
  w.document.close();
}

// ===========================================================================
// 三大报表：资产负债表 / 利润表 / 现金流量表
// ===========================================================================
function statementTableHtml(t) {
  // t = { title, subtitle, company, columns, rows:[{no,name,indent,style,values,negative}] }
  const head = `<th>行次</th><th>项目</th>${(t.columns || []).map((c) => `<th class="num">${esc(c)}</th>`).join("")}`;
  const body = (t.rows || []).map((r) => {
    const indent = "　".repeat(r.indent || 0);
    const bold = r.style === "total" ? " style='font-weight:700;background:#fafafa'" : r.style === "subtotal" ? " style='font-weight:600'" : r.style === "header" ? " style='font-weight:600;background:#f5f7fa'" : "";
    const cells = (r.values || []).map((v) => `<td class="num"${r.negative && moneyNum(v) < 0 ? " style='color:var(--err)'" : ""}>${esc(v)}</td>`).join("");
    return `<tr${bold}><td class="muted">${esc(r.no)}</td><td>${indent}${esc(r.name)}</td>${cells}</tr>`;
  }).join("");
  return `<table class="grid"><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table>`;
}

async function viewBalanceSheet(main) {
  main.innerHTML = `<h2>资产负债表</h2>
    <div class="toolbar">
      <label>从 <input id="bs-from" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="bs-to" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="bs-run">查询</button>
      <button class="btn ghost sm" id="bs-print">打印预览</button>
    </div>
    <div id="bs-result" class="muted">填写期间后点击查询</div>`;
  const load = async () => {
    const f = $("#bs-from").value.trim(), t = $("#bs-to").value.trim();
    try {
      const r = await api(`/reports/balance-sheet?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`);
      $("#bs-result").innerHTML = `<div class="muted" style="margin-bottom:8px">${esc(r.table.subtitle)}</div>${statementTableHtml(r.table)}`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#bs-run").addEventListener("click", load);
  $("#bs-print").addEventListener("click", () => { const f = $("#bs-from").value, t = $("#bs-to").value; window.open(`/api/reports/balance-sheet/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}

async function viewIncomeStatement(main) {
  main.innerHTML = `<h2>利润表</h2>
    <div class="toolbar">
      <label>从 <input id="is-from" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="is-to" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="is-run">查询</button>
      <button class="btn ghost sm" id="is-print">打印预览</button>
    </div>
    <div id="is-result" class="muted">填写期间后点击查询</div>`;
  const load = async () => {
    const f = $("#is-from").value.trim(), t = $("#is-to").value.trim();
    try {
      const r = await api(`/reports/income-statement?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`);
      $("#is-result").innerHTML = `<div class="muted" style="margin-bottom:8px">${esc(r.table.subtitle)}</div>${statementTableHtml(r.table)}`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#is-run").addEventListener("click", load);
  $("#is-print").addEventListener("click", () => { const f = $("#is-from").value, t = $("#is-to").value; window.open(`/api/reports/income-statement/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}

async function viewCashFlow(main) {
  main.innerHTML = `<h2>现金流量表</h2>
    <div class="toolbar">
      <label>从 <input id="cf-from" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="cf-to" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="cf-run">查询</button>
      <button class="btn ghost sm" id="cf-print">打印预览</button>
    </div>
    <div id="cf-result" class="muted">填写期间后点击查询</div>`;
  const lineHtml = (l) => `<tr><td class="muted">${esc(l.code)}</td><td>${esc(l.name)}</td><td class="num">${esc(l.net)}</td></tr>`;
  const section = (title, lines, net) => `<tr style="background:#f5f7fa;font-weight:600"><td colspan="3">${esc(title)}</td></tr>
    ${lines.map(lineHtml).join("")}
    <tr style="font-weight:600"><td colspan="2">${esc(title)}小计</td><td class="num">${esc(net)}</td></tr>`;
  const load = async () => {
    const f = $("#cf-from").value.trim(), t = $("#cf-to").value.trim();
    try {
      const r = await api(`/reports/cash-flow?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`);
      const head = `<th>项目编码</th><th>项目</th><th class="num">金额</th>`;
      const tail = `<tr style="font-weight:700;background:#fafafa"><td colspan="2">现金及现金等价物净增加额</td><td class="num">${esc(r.net_increase)}</td></tr>
        <tr><td colspan="2">加：期初现金及现金等价物余额</td><td class="num">${esc(r.begin_cash)}</td></tr>
        <tr style="font-weight:700;background:#fafafa"><td colspan="2">期末现金及现金等价物余额</td><td class="num">${esc(r.end_cash)}</td></tr>
        <tr><td colspan="3" class="muted">${r.ties ? "✔ 净增加额与货币资金变动勾稽一致" : "✖ 勾稽不符"}</td></tr>`;
      $("#cf-result").innerHTML = `<div class="muted" style="margin-bottom:8px">${esc(r.from)} 至 ${esc(r.to)}</div>
        <table class="grid"><thead><tr>${head}</tr></thead><tbody>
        ${section("经营活动产生的现金流量", r.operating, r.operating_net)}
        ${section("投资活动产生的现金流量", r.investing, r.investing_net)}
        ${section("筹资活动产生的现金流量", r.financing, r.financing_net)}
        ${tail}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#cf-run").addEventListener("click", load);
  $("#cf-print").addEventListener("click", () => { const f = $("#cf-from").value, t = $("#cf-to").value; window.open(`/api/reports/cash-flow/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}

// ===========================================================================
// 安全中心（用户管理）
// ===========================================================================
async function viewSecurity(main) {
  const roles = await loadRoles();
  main.innerHTML = `
    <h2>安全中心</h2>
    <div class="panel">
      <div style="display:flex;gap:14px;align-items:center;flex-wrap:wrap">
        <button class="btn sm" id="me-pwd">修改我的口令</button>
        ${can("user_manage") ? `<button class="btn sm" id="new-user">新建用户</button>` : ""}
        ${can("user_manage") ? `<span class="spacer"></span>
          <label>搜索 <input id="u-kw" style="width:150px" placeholder="账号 / 姓名" /></label>
          <label>角色 <select id="u-role"><option value="">全部</option>${roles.map((r) => `<option value="${r.role}">${esc(r.label)}</option>`).join("")}</select></label>` : ""}
      </div>
    </div>
    ${can("user_manage") ? `<div class="panel"><table class="grid" id="u-table"><thead><tr>
      <th data-sort="username" class="sortable" title="点击排序">账号</th><th>姓名</th><th>角色</th>
      <th data-sort="last_login_at" class="sortable" title="点击排序">最近登录</th><th>锁定</th><th>强制改密</th><th>绑定设备</th><th>状态</th><th></th>
    </tr></thead><tbody><tr><td colspan="9" class="muted">加载中…</td></tr></tbody></table></div>` : `<div class="panel muted">您没有用户管理权限，仅可修改自己的口令。</div>`}`;
  $("#me-pwd").addEventListener("click", () => openChangePwd(false));
  if (can("user_manage")) {
    $("#new-user").addEventListener("click", openNewUser);
    $("#u-kw").addEventListener("input", loadUsers);
    $("#u-role").addEventListener("change", loadUsers);
    $all(".sortable", main).forEach((th) => th.addEventListener("click", () => {
      const key = th.dataset.sort;
      // 排序字段与方向：再次点击同列翻转；切换列时重置为升序
      window._usersSort = { key, asc: window._usersSort && window._usersSort.key === key ? !window._usersSort.asc : true };
      $all(".sortable", main).forEach((h) => h.classList.remove("sort-asc", "sort-desc"));
      th.classList.add(window._usersSort.asc ? "sort-asc" : "sort-desc");
      loadUsers();
    }));
    loadUsers();
  }
}

// 角色 → 权限矩阵（/api/roles），缓存一次
let rolesCache = null;
async function loadRoles() {
  if (rolesCache) return rolesCache;
  try { rolesCache = await api("/roles"); } catch (e) { rolesCache = []; }
  return rolesCache;
}
function rolePermsHtml(perms) {
  if (!perms || !perms.length) return `<span class="muted">无权限</span>`;
  return perms.map((p) => `<span class="tag">${esc(p.label)}</span>`).join(" ");
}

async function loadUsers() {
  const tb = $("#u-table tbody");
  const kw = ($("#u-kw") ? $("#u-kw").value : "").trim().toLowerCase();
  const roleF = $("#u-role") ? $("#u-role").value : "";
  let users;
  try { users = await api("/users"); } catch (e) { tb.innerHTML = `<tr><td colspan="9" style="color:var(--err)">${esc(e.message)} <button class="btn ghost sm" id="u-retry">重试</button></td></tr>`; const rb = $("#u-retry", tb); if (rb) rb.addEventListener("click", loadUsers); return; }
  users = users.filter((u) => {
    if (kw && !((u.username || "").toLowerCase().includes(kw) || (u.display_name || "").toLowerCase().includes(kw))) return false;
    if (roleF && u.role !== roleF) return false;
    return true;
  });
  // 列排序（账号 / 最近登录）
  const sort = window._usersSort;
  if (sort) {
    const dir = sort.asc ? 1 : -1;
    users.sort((a, b) => {
      const av = sort.key === "username" ? (a.username || "") : (a.last_login_at || "");
      const bv = sort.key === "username" ? (b.username || "") : (b.last_login_at || "");
      return av < bv ? -dir : av > bv ? dir : 0;
    });
  }
  if (!users.length) { tb.innerHTML = `<tr><td colspan="9" class="muted">暂无匹配用户</td></tr>`; return; }
  // 缓存当前页用户，供「编辑」弹窗按用户名取完整对象
  window._usersCache = users;
  tb.innerHTML = users.map((u) => {
    // role_label 由后端提供（Role::label），避免前端硬编码与角色扩展脱节
    const roleLabel = u.role_label || u.role;
    const dev = u.device_name ? `<span class="tag">${esc(u.device_name)}</span>` : `<span class="muted">未绑定</span>`;
    const dis = u.disabled ? `<span class="tag err">已停用</span>` : `<span class="tag ok">启用</span>`;
    const lock = u.locked_until ? `<span class="tag warn">已锁定</span>` : `<span class="muted">—</span>`;
    const must = u.must_change_pwd ? `<span class="tag warn">是</span>` : `<span class="muted">否</span>`;
    const last = u.last_login_at ? esc(u.last_login_at) : `<span class="muted">从未登录</span>`;
    const me = u.username === session.user.username;
    return `<tr>
      <td>${esc(u.username)}${u.memo ? `<div class="muted" style="font-size:11px">${esc(u.memo)}</div>` : ""}</td>
      <td>${esc(u.display_name)}</td><td>${esc(roleLabel)}</td>
      <td>${last}</td><td>${lock}</td><td>${must}</td>
      <td>${dev}</td><td>${dis}</td>
      <td class="row-actions">
        <button class="btn ghost sm" data-edit="${esc(u.username)}">编辑</button>
        <button class="btn ghost sm" data-reset-pwd="${esc(u.username)}">重置口令</button>
        <button class="btn ghost sm" data-reset-dev="${esc(u.username)}">重置设备</button>
        ${u.locked_until ? `<button class="btn ghost sm" data-unlock="${esc(u.username)}">解锁</button>` : ""}
        ${me ? "" : `<button class="btn ghost sm" data-toggle="${esc(u.username)}" data-next="${u.disabled ? "0" : "1"}">${u.disabled ? "启用" : "停用"}</button>
        <button class="btn danger sm" data-del="${esc(u.username)}">删除</button>`}
      </td>
    </tr>`;
  }).join("");
  $all("[data-edit]").forEach((b) => b.onclick = () => { const u = (window._usersCache || []).find((x) => x.username === b.dataset.edit); if (u) openEditUser(u); });
  $all("[data-reset-pwd]").forEach((b) => b.onclick = () => openAdminResetPwd(b.dataset.resetPwd));
  $all("[data-toggle]").forEach((b) => b.onclick = async () => {
    const dis = b.dataset.next === "1";
    if (dis && !(await confirmDialog(`停用 ${b.dataset.toggle}？其全部会话将被立即下线。`, true))) return;
    try {
      await api(`/users/${encodeURIComponent(b.dataset.toggle)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ disabled: dis }) });
      toast(dis ? "已停用，该账号全部会话已下线" : "已启用", "ok"); loadUsers();
    } catch (e) { toast(e.message, "err"); }
  });
  $all("[data-reset-dev]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`重置 ${b.dataset.resetDev} 的设备绑定？该账号可在新设备重新登录。`))) return; try { await api(`/users/${encodeURIComponent(b.dataset.resetDev)}/reset-device`, { method: "POST" }); toast("已重置设备绑定，其会话已下线", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
  $all("[data-unlock]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`解锁 ${b.dataset.unlock}？解锁后可重新登录。`))) return; try { await api(`/users/${encodeURIComponent(b.dataset.unlock)}/unlock`, { method: "POST" }); toast("已解锁", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
  $all("[data-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`删除用户 ${b.dataset.del}？删除后该账号无法登录（历史操作日志保留）。`, true))) return; try { await api(`/users/${encodeURIComponent(b.dataset.del)}`, { method: "DELETE" }); toast("已删除", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
}

async function openNewUser() {
  const roles = await loadRoles();
  const mask = modal(`
    <h3>新建用户（账套内成员）</h3>
    <div class="banner set" style="margin-bottom:12px">此账号用于本账套内的角色分工。对方需已拥有<b>平台账号</b>（同名）才能登录本账套；没有的请先让平台管理员在「平台账号」中开通。</div>
    <div class="field"><label>账号（须与平台账号同名）</label><input id="nu-u" /></div>
    <div class="field"><label>姓名</label><input id="nu-n" /></div>
    <div class="field"><label>角色</label><select id="nu-r">${roles.map((r) => `<option value="${r.role}">${esc(r.label)}</option>`).join("")}</select></div>
    <div class="field"><label>初始口令（至少 6 位）</label><input id="nu-p" type="password" /></div>
    <div class="field"><label>备注</label><input id="nu-memo" /></div>
    <div class="field"><label><input type="checkbox" id="nu-must" checked /> 首次登录强制改密</label></div>
    <div id="nu-perm-box">${permMatrixHtml(roles, { role: "accountant", extra_perms: [], deny_perms: [] })}</div>
    <div class="foot"><button class="btn" id="nu-save">创建</button><button class="btn ghost" id="nu-cancel">取消</button></div>`);
  // 切换角色时重新生成矩阵（跟随角色的默认勾选随角色变化）
  const refreshMatrix = () => {
    $("#nu-perm-box", mask).innerHTML = permMatrixHtml(roles, { role: $("#nu-r", mask).value, extra_perms: [], deny_perms: [] });
  };
  $("#nu-r", mask).addEventListener("change", refreshMatrix);
  $("#nu-cancel", mask).onclick = closeModal;
  $("#nu-save", mask).onclick = async () => {
    const extra = [], deny = [];
    $all("[data-perm]", mask).forEach((sel) => {
      if (sel.value === "on") extra.push(sel.dataset.perm);
      else if (sel.value === "off") deny.push(sel.dataset.perm);
    });
    const body = { username: $("#nu-u", mask).value.trim(), display_name: $("#nu-n", mask).value.trim(), password: $("#nu-p", mask).value, role: $("#nu-r", mask).value, memo: $("#nu-memo", mask).value.trim(), must_change_pwd: $("#nu-must", mask).checked, extra_perms: extra, deny_perms: deny };
    if (!body.username || body.password.length < 6) { toast("账号必填且口令至少 6 位", "err"); return; }
    try { await api("/users", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已创建用户", "ok"); closeModal(); loadUsers(); } catch (e) { toast(e.message, "err"); }
  };
}

// 用户编辑弹窗：角色预设基础上的逐项权限开关矩阵
function permMatrixHtml(roles, u) {
  // 全部权限清单（取所有角色权限的并集）
  const allPerms = [];
  const seen = new Set();
  (roles || []).forEach((r) => (r.perms || []).forEach((p) => { if (!seen.has(p.code)) { seen.add(p.code); allPerms.push(p); } }));
  const extraSet = new Set(u.extra_perms || []);
  const denySet = new Set(u.deny_perms || []);
  // 角色预设权限：按 code 收集（perms 是 {code,label} 对象数组，需映射为 code，
  // 否则 new Set 存的是对象、.has(p.code) 永远为 false，导致"角色默认"全部显示为 ✖）
  const rolePerms = (roles.find((x) => x.role === u.role) || {}).perms || [];
  const roleCodes = new Set(rolePerms.map((p) => p.code));
  const stateOf = (code) => (denySet.has(code) ? "off" : extraSet.has(code) ? "on" : "role");
  const rows = allPerms.map((p) => {
    const base = roleCodes.has(p.code) ? "（角色默认 ✔）" : "（角色默认 ✖）";
    const cur = stateOf(p.code);
    return `<tr><td>${esc(p.label)}</td><td><select data-perm="${esc(p.code)}">
      <option value="role" ${cur === "role" ? "selected" : ""}>跟随角色 ${base}</option>
      <option value="on" ${cur === "on" ? "selected" : ""}>强制开启</option>
      <option value="off" ${cur === "off" ? "selected" : ""}>强制关闭</option>
    </select></td></tr>`;
  }).join("");
  return `<div class="panel">
    <div class="muted" style="margin-bottom:6px">权限明细（在角色预设基础上逐项覆盖）</div>
    <table class="grid"><thead><tr><th>功能权限</th><th style="width:240px">授权方式</th></tr></thead>
    <tbody>${rows}</tbody></table>
  </div>`;
}

async function openEditUser(u) {
  const roles = await loadRoles();
  const ds = u.data_scope || {};
  const depts = (ds.depts || []).join(",");
  const me = u.username === session.user.username;
  const mask = modal(`
    <h3>编辑用户 · ${esc(u.username)}</h3>
    <div class="field"><label>姓名</label><input id="eu-n" value="${esc(u.display_name)}" /></div>
    <div class="field"><label>角色</label><select id="eu-r">${roles.map((r) => `<option value="${r.role}" ${r.role === u.role ? "selected" : ""}>${esc(r.label)}</option>`).join("")}</select></div>
    <div class="field"><label>备注</label><input id="eu-memo" value="${esc(u.memo || "")}" /></div>
    <div class="field"><label><input type="checkbox" id="eu-must" ${u.must_change_pwd ? "checked" : ""} /> 强制下次登录改密</label></div>
    <div class="field"><label><input type="checkbox" id="eu-dis" ${u.disabled ? "checked" : ""} ${me ? "disabled" : ""} /> 停用该账号</label></div>
    <div class="panel">
      <div class="muted" style="margin-bottom:6px">数据范围（留空 / 不勾选 = 不限制）</div>
      <div class="field"><label>可见部门（逗号分隔）</label><input id="eu-depts" value="${esc(depts)}" /></div>
      <div class="field"><label>科目范围 从 <input id="eu-acct-from" value="${esc(ds.account_from || "")}" style="width:90px" /> 至 <input id="eu-acct-to" value="${esc(ds.account_to || "")}" style="width:90px" /></label></div>
      <div class="field"><label><input type="checkbox" id="eu-own-v" ${ds.own_voucher_only ? "checked" : ""} /> 仅看本人填制的凭证</label></div>
      <div class="field"><label><input type="checkbox" id="eu-own-d" ${ds.own_doc_only ? "checked" : ""} /> 仅看本人经手的业务单据</label></div>
    </div>
    ${u.is_admin ? `<div class="panel muted">系统管理员默认拥有全部权限，不受逐项开关限制。</div>` : permMatrixHtml(roles, u)}
    <div class="foot"><button class="btn" id="eu-save">保存</button><button class="btn ghost" id="eu-cancel">取消</button></div>`);
  $("#eu-cancel", mask).onclick = closeModal;
  $("#eu-save", mask).onclick = async () => {
    const deptsVal = $("#eu-depts", mask).value.split(",").map((s) => s.trim()).filter(Boolean);
    const extra = [], deny = [];
    if (!u.is_admin) {
      $all("[data-perm]", mask).forEach((sel) => {
        const code = sel.dataset.perm;
        if (sel.value === "on") extra.push(code);
        else if (sel.value === "off") deny.push(code);
      });
    }
    const body = {
      display_name: $("#eu-n", mask).value.trim(),
      role: $("#eu-r", mask).value,
      memo: $("#eu-memo", mask).value.trim(),
      must_change_pwd: $("#eu-must", mask).checked,
      disabled: $("#eu-dis", mask).checked,
      data_scope: { depts: deptsVal, account_from: $("#eu-acct-from", mask).value.trim(), account_to: $("#eu-acct-to", mask).value.trim(), own_voucher_only: $("#eu-own-v", mask).checked, own_doc_only: $("#eu-own-d", mask).checked },
      extra_perms: extra,
      deny_perms: deny,
    };
    try { await api(`/users/${encodeURIComponent(u.username)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已保存", "ok"); closeModal(); loadUsers(); } catch (e) { toast(e.message, "err"); }
  };
}
function openAdminResetPwd(username) {
  const mask = modal(`
    <h3>重置口令 · ${esc(username)}</h3>
    <div class="field"><label>新口令（至少 6 位）</label><input id="rp-p" type="password" /></div>
    <div class="foot"><button class="btn" id="rp-save">重置</button><button class="btn ghost" id="rp-cancel">取消</button></div>`);
  $("#rp-cancel", mask).onclick = closeModal;
  $("#rp-save", mask).onclick = async () => {
    const np = $("#rp-p", mask).value;
    if (np.length < 6) { toast("口令至少 6 位", "err"); return; }
    try { await api(`/users/${encodeURIComponent(username)}/reset-password`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ new: np }) }); toast("已重置口令", "ok"); closeModal(); } catch (e) { toast(e.message, "err"); }
  };
}
// ===========================================================================
// 平台管理（仅平台管理员，作用于全局身份库，与账套内子账号无关）
// ===========================================================================
async function viewPlatformUsers(main) {
  main.innerHTML = `<h2>平台账号</h2>
    <div class="toolbar">
      <button class="btn primary" id="pu-add">＋ 开通账号</button>
      <span class="muted">平台账号用于登录 Web 系统；普通用户登录后自行创建与维护账套。</span>
    </div>
    <div id="pu-list" class="muted">加载中…</div>`;
  async function load() {
    try {
      const rows = await api("/platform/users");
      $("#pu-list").innerHTML = rows.length ? `<table class="grid"><thead><tr>
        <th>账号</th><th>展示名</th><th>类型</th><th>状态</th><th>设备</th><th>创建时间</th><th>操作</th></tr></thead>
        <tbody>${rows.map((x) => `<tr>
          <td>${esc(x.username)}</td><td>${esc(x.display_name)}</td>
          <td>${x.is_admin ? "平台管理员" : "普通用户"}</td>
          <td>${x.disabled ? `<span style="color:var(--err)">已停用</span>` : "正常"}</td>
          <td>${x.device_bound ? "已绑定" : "未绑定"}</td>
          <td class="muted">${esc(x.created_at)}</td>
          <td>
            <button class="btn sm" data-act="edit" data-u="${esc(x.username)}">编辑</button>
            <button class="btn sm" data-act="reset" data-u="${esc(x.username)}">重置口令</button>
            <button class="btn sm" data-act="dev" data-u="${esc(x.username)}">重置设备</button>
            <button class="btn sm ghost" data-act="del" data-u="${esc(x.username)}">删除</button>
          </td></tr>`).join("")}</tbody></table>` : `<div class="muted">暂无账号</div>`;
    } catch (e) { $("#pu-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
  $("#pu-add").addEventListener("click", () => {
    const mask = modal(`
      <h3>开通平台账号</h3>
      <div class="field"><label>账号（登录名）</label><input id="nc-u" autocomplete="off" /></div>
      <div class="field"><label>展示名</label><input id="nc-d" /></div>
      <div class="field"><label>初始口令（至少 6 位，首次登录会要求改密）</label><input id="nc-p" type="text" /></div>
      <div class="field"><label><input type="checkbox" id="nc-a" /> 设为平台管理员</label></div>
      <div class="foot"><button class="btn" id="nc-cancel">取消</button><button class="btn primary" id="nc-save">创建</button></div>`);
    $("#nc-cancel", mask).onclick = closeModal;
    $("#nc-save", mask).onclick = async () => {
      const username = $("#nc-u", mask).value.trim();
      const password = $("#nc-p", mask).value;
      if (!username) { toast("请输入账号", "err"); return; }
      if (password.length < 6) { toast("口令至少 6 位", "err"); return; }
      try {
        await api("/platform/users", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ username, display_name: $("#nc-d", mask).value.trim(), password, is_admin: $("#nc-a", mask).checked }) });
        toast(`账号「${esc(username)}」已开通`, "ok");
        closeModal(); load();
      } catch (e) { toast(e.message, "err"); }
    };
  });
  $("#pu-list").addEventListener("click", async (e) => {
    const btn = e.target.closest("button[data-act]");
    if (!btn) return;
    const u = btn.dataset.u, act = btn.dataset.act;
    if (act === "reset") {
      const mask = modal(`
        <h3>重置口令：${esc(u)}</h3>
        <div class="field"><label>新口令（至少 6 位）</label><input id="rp-n" type="text" /></div>
        <div class="foot"><button class="btn" id="rp-cancel">取消</button><button class="btn primary" id="rp-save">重置</button></div>`);
      $("#rp-cancel", mask).onclick = closeModal;
      $("#rp-save", mask).onclick = async () => {
        const np = $("#rp-n", mask).value;
        if (np.length < 6) { toast("口令至少 6 位", "err"); return; }
        try {
          await api(`/platform/users/${encodeURIComponent(u)}/reset-password`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ new: np }) });
          toast("口令已重置", "ok"); closeModal();
        } catch (err) { toast(err.message, "err"); }
      };
    } else if (act === "edit") {
      let info = null;
      try { info = (await api("/platform/users")).find((x) => x.username === u); } catch (err) {}
      const mask = modal(`
        <h3>编辑账号：${esc(u)}</h3>
        <div class="field"><label>展示名</label><input id="eu-d" value="${esc(info ? info.display_name : "")}" /></div>
        <div class="field"><label><input type="checkbox" id="eu-a" ${info && info.is_admin ? "checked" : ""} /> 平台管理员</label></div>
        <div class="field"><label><input type="checkbox" id="eu-x" ${info && info.disabled ? "checked" : ""} /> 停用该账号</label></div>
        <div class="foot"><button class="btn" id="eu-cancel">取消</button><button class="btn primary" id="eu-save">保存</button></div>`);
      $("#eu-cancel", mask).onclick = closeModal;
      $("#eu-save", mask).onclick = async () => {
        try {
          await api(`/platform/users/${encodeURIComponent(u)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ display_name: $("#eu-d", mask).value.trim(), is_admin: $("#eu-a", mask).checked, disabled: $("#eu-x", mask).checked }) });
          toast("已保存", "ok"); closeModal(); load();
        } catch (err) { toast(err.message, "err"); }
      };
    } else if (act === "dev") {
      if (!(await confirmDialog(`重置「${u}」的设备绑定？该账号将被强制下线，下次登录自动绑定新设备。`, true))) return;
      try { await api(`/platform/users/${encodeURIComponent(u)}/reset-device`, { method: "POST" }); toast("设备绑定已重置", "ok"); load(); } catch (err) { toast(err.message, "err"); }
    } else if (act === "del") {
      if (!(await confirmDialog(`确定删除账号「${u}」？该操作不可恢复（其创建的账套需先删除）。`, true))) return;
      try { await api(`/platform/users/${encodeURIComponent(u)}`, { method: "DELETE" }); toast("已删除", "ok"); load(); } catch (err) { toast(err.message, "err"); }
    }
  });
  await load();
}

async function viewPlatformBooks(main) {
  main.innerHTML = `<h2>全部账套</h2>
    <div class="muted" style="margin-bottom:10px">平台管理员可进入任意账套查看：以临时管理员身份进入，不在该账套留下账号记录，操作会记入账套审计日志。</div>
    <div id="pb-list" class="muted">加载中…</div>`;
  async function load() {
    try {
      const data = await loadMyBooks();
      const books = data.books || [];
      $("#pb-list").innerHTML = books.length ? `<table class="grid"><thead><tr>
        <th>账套标识</th><th>公司名称</th><th>归属用户</th><th>操作</th></tr></thead>
        <tbody>${books.map((b) => `<tr>
          <td>${esc(b.key)}</td><td>${esc(b.company || "（未命名）")}</td><td>${esc(b.owner)}</td>
          <td><button class="btn sm" data-key="${esc(b.key)}" data-company="${esc(b.company || b.key)}">进入</button>
          <button class="btn sm ghost" data-del="${esc(b.key)}" data-name="${esc(b.company || b.key)}">删除</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">系统中还没有账套。普通用户登录后可自行创建。</div>`;
    } catch (e) { $("#pb-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
  $("#pb-list").addEventListener("click", (e) => {
    const btn = e.target.closest("button[data-key]");
    if (btn) { enterBook(btn.dataset.key, btn.dataset.company); return; }
    const del = e.target.closest("button[data-del]");
    if (del) deleteBook(del.dataset.del, del.dataset.name);
  });
  await load();
}

function openChangePwd(forced, onDone) {
  const mask = modal(`
    <h3>${forced ? "首次登录 / 口令已重置：请设置新口令" : "修改口令"}</h3>
    ${forced ? `<div class="banner unset" style="margin-bottom:12px">为安全起见，请先设置一个强度较高的新口令。</div>` : ""}
    <div class="field"><label>原口令</label><input id="cp-o" type="password" ${forced ? "placeholder='首次登录可留空'" : ""} /></div>
    <div class="field"><label>新口令（至少 6 位）</label><input id="cp-n" type="password" /></div>
    <div class="field"><label>确认新口令</label><input id="cp-c" type="password" /></div>
    <div class="foot"><button class="btn" id="cp-save">保存</button>${forced ? "" : `<button class="btn ghost" id="cp-cancel">取消</button>`}</div>`);
  if (!forced) $("#cp-cancel", mask).onclick = closeModal;
  $("#cp-save", mask).onclick = async () => {
    const oldp = $("#cp-o", mask).value, np = $("#cp-n", mask).value, cp = $("#cp-c", mask).value;
    if (np.length < 6) { toast("新口令至少 6 位", "err"); return; }
    if (np !== cp) { toast("两次输入不一致", "err"); return; }
    try {
      await api("/change-password", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ old: oldp, new: np }) });
      toast("口令已更新", "ok");
      closeModal();
      if (typeof onDone === "function") onDone();
    } catch (e) { toast(e.message, "err"); }
  };
}

// ===========================================================================
// 多栏账
// ===========================================================================
async function viewMultiColumn(main) {
  main.innerHTML = `<h2>多栏账</h2>
    <div class="toolbar">
      <label>主科目 <input id="mc-main" value="6602" style="width:90px" /></label>
      <label>栏目(逗号分隔) <input id="mc-cols" value="660201,660202,660203" style="width:220px" /></label>
      <label>期间 <input id="mc-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="mc-run">查询</button>
      <button class="btn ghost sm" id="mc-run-print">打印预览</button>
    </div>
    <div id="mc-result" class="muted">填写条件后点击查询</div>`;
  $("#mc-run").addEventListener("click", async () => {
  $("#mc-run-print").addEventListener("click", () => { const el = $("#mc-result").querySelector("table"); printPreview("多栏账", el); });
    const mainCode = $("#mc-main").value.trim();
    const cols = $("#mc-cols").value.split(",").map((s) => s.trim()).filter(Boolean);
    const p = $("#mc-period").value.trim();
    if (!mainCode || !cols.length) { toast("请填写主科目与栏目", "err"); return; }
    try {
      const r = await api(`/reports/multi-column?main=${encodeURIComponent(mainCode)}&cols=${encodeURIComponent(cols.join(","))}&from=${encodeURIComponent(p)}&to=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      const head = ["日期", "凭证号", "摘要", "发生额", ...cols, "余额"];
      $("#mc-result").innerHTML = `<table><thead><tr>${head.map((h) => `<th>${esc(h)}</th>`).join("")}</tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.date)}</td><td>${esc(x.voucher_no)}</td><td>${esc(x.summary)}</td>
          <td class="r">${fmt(x.amount)}</td>${cols.map((_, i) => `<td class="r">${fmt(x.cols[i])}</td>`).join("")}
          <td class="r">${fmt(x.balance)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 摘要汇总表
// ===========================================================================
async function viewSummaryTable(main) {
  main.innerHTML = `<h2>摘要汇总表</h2>
    <div class="toolbar">
      <label>期间 <input id="st-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="st-run">查询</button>
      <button class="btn ghost sm" id="st-run-print">打印预览</button>
    </div>
    <div id="st-result" class="muted">填写期间后点击查询</div>`;
  $("#st-run").addEventListener("click", async () => {
  $("#st-run-print").addEventListener("click", () => { const el = $("#st-result").querySelector("table"); printPreview("摘要汇总表", el); });
    const p = $("#st-period").value.trim();
    try {
      const r = await api(`/reports/summary-table?from=${encodeURIComponent(p)}&to=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      $("#st-result").innerHTML = `<table><thead><tr><th>摘要</th><th>凭证张数</th><th>借方发生额</th><th>贷方发生额</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.summary)}</td><td class="r">${x.voucher_count}</td><td class="r">${fmt(x.debit)}</td><td class="r">${fmt(x.credit)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 财务指标
// ===========================================================================
async function viewRatios(main) {
  main.innerHTML = `<h2>财务指标分析</h2>
    <div class="toolbar">
      <label>期间 <input id="rt-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="rt-run">查询</button>
      <button class="btn ghost sm" id="rt-run-print">打印预览</button>
    </div>
    <div id="rt-result" class="muted">填写期间后点击查询</div>`;
  $("#rt-run").addEventListener("click", async () => {
  $("#rt-run-print").addEventListener("click", () => { const el = $("#rt-result").querySelector("table"); printPreview("财务指标分析", el); });
    const p = $("#rt-period").value.trim();
    try {
      const r = await api(`/reports/ratios?period=${encodeURIComponent(p)}`);
      const rows = r.ratios || [];
      $("#rt-result").innerHTML = `<table><thead><tr><th>指标</th><th>数值</th><th>计算公式</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.name)}</td><td class="r">${esc(x.display)}</td><td class="muted">${esc(x.formula)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// MRP 运算
// ===========================================================================
async function viewMrp(main) {
  main.innerHTML = `<h2>MRP 运算</h2>
    <div class="toolbar">
      <label>需求产品 <input id="mrp-item" style="width:120px" /></label>
      <label>数量 <input id="mrp-qty" value="10" style="width:80px" /></label>
      <button class="btn primary" id="mrp-run">运行</button>
      <button class="btn ghost" id="mrp-latest">最近一次结果</button>
    </div>
    <div id="mrp-result" class="muted">输入需求产品与数量后运行</div>`;
  const load = (rows) => {
    $("#mrp-result").innerHTML = `<table><thead><tr><th>层级</th><th>物料</th><th>毛需求</th><th>现有库存</th><th>净需求</th><th>计划量</th><th>行动</th><th>来源</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${x.level}</td><td>${esc(x.item_code)}</td><td class="r">${fmt(x.gross_req)}</td><td class="r">${fmt(x.on_hand)}</td><td class="r">${fmt(x.net_req)}</td><td class="r">${fmt(x.planned_qty)}</td><td>${x.action === "produce" ? "生产" : x.action === "purchase" ? "采购" : "无"}</td><td>${esc(x.source)}</td></tr>`).join("")}</tbody></table>`;
  };
  $("#mrp-run").addEventListener("click", async () => {
    const item = $("#mrp-item").value.trim();
    const qty = $("#mrp-qty").value.trim();
    if (!item || !qty) { toast("请填写需求产品与数量", "err"); return; }
    try {
      const r = await api("/mrp/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ demands: [{ item_code: item, qty, source: "手工" }] }) });
      load(r.rows || []);
      toast("MRP 运算完成", "ok");
    } catch (e) { toast(e.message, "err"); }
  });
  $("#mrp-latest").addEventListener("click", async () => {
    try { const r = await api("/mrp/latest"); load(r.rows || []); } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 工艺路线
// ===========================================================================
async function viewRouting(main) {
  main.innerHTML = `<h2>工艺路线</h2>
    <div class="toolbar">
      <label>产品代码 <input id="rt-item" style="width:120px" /></label>
      <button class="btn primary" id="rt-load">加载</button>
    </div>
    <div id="rt-result" class="muted">输入产品代码后加载</div>`;
  $("#rt-load").addEventListener("click", async () => {
    const item = $("#rt-item").value.trim();
    if (!item) { toast("请输入产品代码", "err"); return; }
    try {
      const r = await api(`/routing/${encodeURIComponent(item)}`);
      const ops = r.ops || [];
      $("#rt-result").innerHTML = `<table><thead><tr><th>序号</th><th>工序编码</th><th>工序名称</th><th>工作中心</th><th>标准工时</th><th>小时费率</th></tr></thead>
        <tbody>${ops.map((o) => `<tr><td>${o.seq}</td><td>${esc(o.op_code)}</td><td>${esc(o.op_name)}</td><td>${esc(o.work_center)}</td><td class="r">${fmt(o.std_hours)}</td><td class="r">${fmt(o.rate)}</td></tr>`).join("")}</tbody></table>`;
      if (!ops.length) $("#rt-result").innerHTML = `<div class="muted">该产品暂无工艺路线。维护请调用 POST /api/routing/:item。</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 审批中心
// ===========================================================================
async function viewApproval(main) {
  main.innerHTML = `<h2>审批中心</h2><div id="ap-list" class="muted">加载中…</div>`;
  const load = async () => {
    try {
      const r = await api("/approvals/todo");
      const rows = r.rows || [];
      $("#ap-list").innerHTML = rows.length ? rows.map((a) => `
        <div class="card"><div class="row"><b>${esc(a.title)}</b> <span class="tag">${esc(a.biz_kind)}/${a.biz_id}</span></div>
          <div class="muted">申请人 ${esc(a.applicant)} · 当前节点 ${a.current_node}/${a.steps.length}</div>
          <div class="row" style="margin-top:8px">
            <button class="btn sm" data-ap="${a.id}" data-act="1">通过</button>
            <button class="btn danger sm" data-ap="${a.id}" data-act="0">驳回</button>
          </div>
        </div>`).join("") : `<div class="muted">没有待审批的单据</div>`;
      $all("[data-ap]").forEach((b) => b.onclick = async () => {
        const approve = b.dataset.act === "1";
        try { await api(`/approvals/${b.dataset.ap}/act`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ approve, comment: approve ? "同意" : "驳回" }) }); toast(approve ? "已通过" : "已驳回", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 报表附注
// ===========================================================================
async function viewNotes(main) {
  main.innerHTML = `<h2>报表附注</h2>
    <div class="toolbar">
      <label>报表 <select id="nt-key"><option value="balance_sheet">资产负债表</option><option value="income_statement">利润表</option><option value="cash_flow">现金流量表</option></select></label>
      <label>期间 <input id="nt-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="nt-load">加载</button>
    </div>
    <div id="nt-list"></div>
    <div class="card" style="margin-top:12px"><h3>新增附注</h3>
      <div class="field"><label>标题</label><input id="nt-title" /></div>
      <div class="field"><label>内容</label><textarea id="nt-content" rows="3"></textarea></div>
      <button class="btn primary" id="nt-save">保存</button>
    </div>`;
  const load = async () => {
    const key = $("#nt-key").value, p = $("#nt-period").value.trim();
    try {
      const r = await api(`/reports/notes?report_key=${key}&period=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      $("#nt-list").innerHTML = rows.map((n) => `<div class="card"><div class="row"><b>${n.seq} · ${esc(n.title)}</b><button class="btn danger sm" data-nt-del="${n.id}">删除</button></div><div>${esc(n.content)}</div><div class="muted">${esc(n.updated_by)} ${esc(n.updated_at)}</div></div>`).join("") || `<div class="muted">暂无附注</div>`;
      $all("[data-nt-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该附注？", true))) return; try { await api(`/reports/notes/${b.dataset.ntDel}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#nt-load").addEventListener("click", load);
  $("#nt-save").addEventListener("click", async () => {
    const key = $("#nt-key").value, p = $("#nt-period").value.trim();
    const title = $("#nt-title").value.trim(), content = $("#nt-content").value.trim();
    if (!title) { toast("标题不能为空", "err"); return; }
    try {
      await api("/reports/notes", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ report_key: key, period: ymm(p), title, content }) });
      toast("已保存附注", "ok");
      $("#nt-title").value = ""; $("#nt-content").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

// ===========================================================================
// 电子档案
// ===========================================================================
async function viewArchive(main) {
  main.innerHTML = `<h2>会计电子档案</h2>
    <div class="toolbar">
      <label>期间 <input id="ar-period" value="${esc(state.current)}" style="width:90px" /></label>
      <label>类型(空=全部) <input id="ar-kind" placeholder="voucher/ledger/report/balance" style="width:180px" /></label>
      <button class="btn primary" id="ar-load">加载</button>
    </div>
    <div id="ar-list"></div>
    <div class="card" style="margin-top:12px"><h3>新增归档</h3>
      <div class="field"><label>类型</label><input id="ar-new-kind" value="voucher" style="width:120px" /></div>
      <div class="field"><label>标题</label><input id="ar-new-title" /></div>
      <div class="field"><label>内容(JSON)</label><textarea id="ar-new-payload" rows="3"></textarea></div>
      <button class="btn primary" id="ar-save">归档</button>
    </div>`;
  const load = async () => {
    const p = $("#ar-period").value.trim(), k = $("#ar-kind").value.trim();
    try {
      const r = await api(`/archives?period=${encodeURIComponent(p)}${k ? "&kind=" + encodeURIComponent(k) : ""}`);
      const rows = r.rows || [];
      $("#ar-list").innerHTML = rows.map((a) => `<div class="card"><div class="row"><b>${esc(a.file_no)}</b> <span class="tag">${esc(a.kind)}</span> <span class="tag ok">${a.sealed ? "封存" : "未封存"}</span></div><div>${esc(a.title)}</div><div class="muted">${esc(a.archived_by)} ${esc(a.archived_at)} · ${esc(a.content_hash).slice(0, 16)}</div></div>`).join("") || `<div class="muted">暂无档案</div>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#ar-load").addEventListener("click", load);
  $("#ar-save").addEventListener("click", async () => {
    const p = $("#ar-period").value.trim(), kind = $("#ar-new-kind").value.trim();
    const title = $("#ar-new-title").value.trim(), payload = $("#ar-new-payload").value.trim();
    if (!title || !payload) { toast("标题与内容不能为空", "err"); return; }
    try {
      await api("/archives", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(p), kind, title, payload }) });
      toast("已归档", "ok");
      $("#ar-new-title").value = ""; $("#ar-new-payload").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

// ===========================================================================
// 预算版本
// ===========================================================================
async function viewBudgetVersions(main) {
  main.innerHTML = `<h2>预算版本管理</h2>
    <div id="bv-list" class="muted">加载中…</div>
    <div class="card" style="margin-top:12px"><h3>新建版本</h3>
      <div class="field"><label>版本编码</label><input id="bv-key" placeholder="v2" style="width:140px" /></div>
      <div class="field"><label>版本名称</label><input id="bv-name" placeholder="2026 调整版" /></div>
      <div class="field"><label>备注</label><input id="bv-memo" /></div>
      <label class="muted"><input type="checkbox" id="bv-copy" checked /> 从当前版本复制数据</label>
      <div style="margin-top:8px"><button class="btn primary" id="bv-save">创建版本</button></div>
    </div>`;
  const load = async () => {
    try {
      const r = await api("/budget/versions");
      const versions = r.versions || [], current = r.current || "";
      $("#bv-list").innerHTML = versions.length ? versions.map((v) => `
        <div class="card"><div class="row">
          <b>${v.is_current ? "●" : "○"} ${esc(v.name)}</b> <span class="tag">${esc(v.key)}</span>
          <span class="grow"></span>
          ${v.is_current ? `<span class="tag ok">当前版本</span>` : `<button class="btn ghost sm" data-bv-act="${esc(v.key)}">设为当前</button>`}
          <button class="btn danger sm" data-bv-del="${esc(v.key)}">删除</button>
        </div><div class="muted">${esc(v.created_at)} ${esc(v.memo)}</div></div>`).join("")
        : `<div class="muted">暂无自定义版本，预算存于「默认」版本。当前版本：${current || "默认"}</div>`;
      $all("[data-bv-act]").forEach((b) => b.onclick = async () => { try { await api(`/budget/versions/${encodeURIComponent(b.dataset.bvAct)}/activate`, { method: "POST" }); toast("已设为当前版本", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      $all("[data-bv-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`删除版本 ${b.dataset.bvDel} 及其全部预算数据？`, true))) return; try { await api(`/budget/versions/${encodeURIComponent(b.dataset.bvDel)}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#bv-save").addEventListener("click", async () => {
    const key = $("#bv-key").value.trim(), name = $("#bv-name").value.trim(), memo = $("#bv-memo").value.trim();
    if (!key || !name) { toast("版本编码与名称不能为空", "err"); return; }
    try {
      await api("/budget/versions", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ key, name, memo, is_current: false }) });
      if ($("#bv-copy").checked) {
        const r = await api("/budget/versions");
        const current = r.current || "";
        await api("/budget/versions/copy", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ from: current, to: key }) });
      }
      toast("版本已创建", "ok");
      $("#bv-key").value = ""; $("#bv-name").value = ""; $("#bv-memo").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

// ===========================================================================
// 工序报工
// ===========================================================================
async function viewWorkReport(main) {
  main.innerHTML = `<h2>工序报工</h2>
    <div class="toolbar">
      <label>生产订单 <select id="wr-po" style="min-width:180px"></select></label>
      <button class="btn primary" id="wr-load">加载工序</button>
    </div>
    <div id="wr-ops" class="muted">选择生产订单后加载工序</div>`;
  let orders = [];
  try {
    const r = await api("/prod");
    orders = r.orders || [];
  } catch (e) { toast(e.message, "err"); }
  $("#wr-po").innerHTML = orders.map((o) => `<option value="${o.id}">${esc(o.no)} · ${esc(o.item_name)}（${o.status}）</option>`).join("") || `<option value="">当前期间无生产订单</option>`;
  if (!orders.length) { $("#wr-ops").innerHTML = `<div class="muted">当前期间没有生产订单。请先在业务模块下达生产订单。</div>`; return; }
  const loadOps = async () => {
    const poId = $("#wr-po").value;
    if (!poId) return;
    try {
      const r = await api(`/prod/${poId}/ops`);
      const ops = r.ops || [];
      $("#wr-ops").innerHTML = ops.length ? ops.map((op) => `
        <div class="card"><div class="row">
          <b>${esc(op.op_name)}</b> <span class="tag">${esc(op.work_center)}</span>
          <span class="tag ${op.status === "done" ? "ok" : op.status === "in_progress" ? "" : ""}">${op.status === "done" ? "完工" : op.status === "in_progress" ? "进行中" : "待开工"}</span>
        </div>
        <div class="row" style="margin-top:6px">
          <label>完工数量 <input id="wq-${op.id}" value="0" style="width:80px" /></label>
          <label>实际工时 <input id="wh-${op.id}" value="0" style="width:80px" /></label>
          <button class="btn sm" data-wr-report="${op.id}">报工</button>
          <button class="btn sm" data-wr-finish="${op.id}">完工</button>
          <span class="muted">已累计 数量 ${fmt(op.qty_done)} · 工时 ${fmt(op.hours)}</span>
        </div></div>`).join("") : `<div class="muted">该订单暂无工序，请先维护产品工艺路线</div>`;
      $all("[data-wr-report]").forEach((b) => b.onclick = async () => {
        const qty = $(`#wq-${b.dataset.wrReport}`).value.trim();
        const hours = $(`#wh-${b.dataset.wrReport}`).value.trim();
        if (!qty && !hours) { toast("数量与工时不能同时为空", "err"); return; }
        try { await api("/prod/op/report", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ op_id: parseInt(b.dataset.wrReport, 10), qty, hours }) }); toast("已报工", "ok"); loadOps(); } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-wr-finish]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("将该工序标记为完工？"))) return; try { await api("/prod/op/finish", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ op_id: parseInt(b.dataset.wrFinish, 10) }) }); toast("已完工", "ok"); loadOps(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#wr-load").addEventListener("click", loadOps);
}

// ===========================================================================
// 权益变动表
// ===========================================================================
async function viewEquity(main) {
  main.innerHTML = `<h2>所有者权益变动表</h2>
    <div class="toolbar"><label>期间 <input id="eq-period" value="${esc(state.current)}" style="width:90px" /></label>
    <button class="btn primary" id="eq-run">查询</button>
      <button class="btn ghost sm" id="eq-run-print">打印预览</button></div>
    <div id="eq-result" class="muted">填写期间后点击查询</div>`;
  $("#eq-run").addEventListener("click", async () => {
  $("#eq-run-print").addEventListener("click", () => { const el = $("#eq-result").querySelector("table"); printPreview("所有者权益变动表", el); });
    const p = $("#eq-period").value.trim();
    try {
      const r = await api(`/reports/equity?period=${encodeURIComponent(p)}`);
      const s = r.statement;
      const rows = [...s.lines, s.total];
      $("#eq-result").innerHTML = `<table><thead><tr><th>项目</th><th>本年年初余额</th><th>本年增减变动</th><th>本年年末余额</th></tr></thead>
        <tbody>${rows.map((l) => `<tr><td>${esc(l.name)}</td><td class="r">${fmt(l.begin)}</td><td class="r">${fmt(l.change)}</td><td class="r">${fmt(l.end)}</td></tr>`).join("")}</tbody></table>
        <div class="muted">${s.ties ? "✔ 年初 + 增减 = 年末，勾稽通过" : "✖ 勾稽不符"}</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 报表对比
// ===========================================================================
async function viewCompare(main) {
  main.innerHTML = `<h2>报表对比分析</h2>
    <div class="toolbar">
      <label>报表 <select id="cp-key"><option value="balance_sheet">资产负债表</option><option value="income_statement">利润表</option></select></label>
      <label>当前期 <input id="cp-cur" value="${esc(state.current)}" style="width:90px" /></label>
      <label>对比期 <input id="cp-prev" style="width:90px" /></label>
      <button class="btn primary" id="cp-run">对比</button>
      <button class="btn ghost sm" id="cp-run-print">打印预览</button></div>
    <div id="cp-result" class="muted">填写期间后点击对比</div>`;
  const p = $("#cp-cur").value.trim();
  try { $("#cp-prev").value = prevPeriod(p); } catch (e) {}
  $("#cp-run").addEventListener("click", async () => {
  $("#cp-run-print").addEventListener("click", () => { const el = $("#cp-result").querySelector("table"); printPreview("报表对比分析", el); });
    const key = $("#cp-key").value, cur = $("#cp-cur").value.trim(), prev = $("#cp-prev").value.trim();
    try {
      const r = await api(`/reports/compare?key=${encodeURIComponent(key)}&period=${encodeURIComponent(cur)}&prev=${encodeURIComponent(prev)}`);
      const rows = r.rows || [];
      $("#cp-result").innerHTML = `<table><thead><tr><th>项目</th><th>当前期</th><th>对比期</th><th>差额</th><th>变动率</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.name)}</td><td class="r">${fmt(x.current)}</td><td class="r">${fmt(x.previous)}</td><td class="r">${fmt(x.diff)}</td><td class="r">${x.rate == null ? "—" : x.rate + "%"}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}
function prevPeriod(p) {
  const [y, m] = String(p).split("-").map(Number);
  const pm = m - 1 < 1 ? 12 : m - 1, py = m - 1 < 1 ? y - 1 : y;
  return `${py}-${String(pm).padStart(2, "0")}`;
}

// ===========================================================================
// 科目日报表
// ===========================================================================
async function viewDaily(main) {
  main.innerHTML = `<h2>科目日报表</h2>
    <div class="toolbar">
      <label>科目 <input id="dl-code" placeholder="1001" style="width:90px" /></label>
      <label>期间 <input id="dl-from" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="dl-run">查询</button>
      <button class="btn ghost sm" id="dl-run-print">打印预览</button></div>
    <div id="dl-result" class="muted">填写科目与期间后点击查询</div>`;
  $("#dl-run").addEventListener("click", async () => {
  $("#dl-run-print").addEventListener("click", () => { const el = $("#dl-result").querySelector("table"); printPreview("科目日报表", el); });
    const code = $("#dl-code").value.trim();
    if (!code) { toast("请输入科目编码", "err"); return; }
    const from = $("#dl-from").value.trim();
    try {
      const r = await api(`/reports/daily?code=${encodeURIComponent(code)}&from=${encodeURIComponent(from)}&to=${encodeURIComponent(from)}`);
      const rows = r.rows || [];
      $("#dl-result").innerHTML = `<table><thead><tr><th>日期</th><th>借方</th><th>贷方</th><th>日末余额</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.date)}</td><td class="r">${fmt(x.debit)}</td><td class="r">${fmt(x.credit)}</td><td class="r">${fmt(x.balance)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 期末对账
// ===========================================================================
async function viewReconcile(main) {
  main.innerHTML = `<h2>期末对账</h2>
    <div class="toolbar"><label>期间 <input id="rc-period" value="${esc(state.current)}" style="width:90px" /></label>
    <button class="btn primary" id="rc-run">对账</button>
      <button class="btn ghost sm" id="rc-run-print">打印预览</button></div>
    <div id="rc-result" class="muted">填写期间后点击对账</div>`;
  $("#rc-run").addEventListener("click", async () => {
  $("#rc-run-print").addEventListener("click", () => { const el = $("#rc-result").querySelector("table"); printPreview("期末对账", el); });
    const p = $("#rc-period").value.trim();
    try {
      const r = await api(`/reports/reconcile?period=${encodeURIComponent(p)}`);
      const items = r.items || [];
      $("#rc-result").innerHTML = items.map((i) => `<div class="card"><div class="row"><b>${esc(i.name)}</b><span class="tag ${i.ok ? "ok" : "err"}">${i.ok ? "通过" : "异常"}</span></div><div class="muted">${esc(i.detail)}</div></div>`).join("") || `<div class="muted">暂无对账数据</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 预算预警
// ===========================================================================
async function viewBudgetAlerts(main) {
  main.innerHTML = `<h2>预算预警</h2>
    <div class="toolbar">
      <label>期间 <input id="ba-period" value="${esc(state.current)}" style="width:90px" /></label>
      <label>阈值(%) <input id="ba-thr" value="90" style="width:60px" /></label>
      <button class="btn primary" id="ba-run">查询</button>
      <button class="btn ghost sm" id="ba-run-print">打印预览</button></div>
    <div id="ba-result" class="muted">填写期间后点击查询</div>`;
  $("#ba-run").addEventListener("click", async () => {
  $("#ba-run-print").addEventListener("click", () => { const el = $("#ba-result").querySelector("table"); printPreview("预算预警", el); });
    const p = $("#ba-period").value.trim(), thr = $("#ba-thr").value.trim() || "90";
    try {
      const r = await api(`/budget/alerts?period=${encodeURIComponent(p)}&threshold=${encodeURIComponent(thr)}`);
      const rows = r.rows || [];
      $("#ba-result").innerHTML = rows.length
        ? `<table><thead><tr><th>科目</th><th>部门</th><th>预算数</th><th>实际数</th><th>执行率</th><th>超支额</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x.account_code)} ${esc(x.account_name)}</td><td>${esc(x.dept || "—")}</td><td class="r">${fmt(x.budget)}</td><td class="r">${fmt(x.actual)}</td><td class="r">${x.rate}%</td><td class="r" style="color:var(--err)">${fmt(x.over_amount)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无预警科目</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 期末处理（与桌面端对齐：结转损益 / 年末结转 / 结账 / 反结账）
// ===========================================================================
async function viewPeriodEnd(main) {
  const cur = (state.current || "").replace("-", "");
  main.innerHTML = `<h2>期末处理</h2>
    <div class="toolbar">
      <label>期间 <input id="pe-period" value="${esc(cur)}" style="width:90px" placeholder="YYYYMM" /></label>
      <label><input type="checkbox" id="pe-reqcarry" checked /> 要求先结转损益</label>
      <button class="btn" id="pe-check">重新检查</button>
      <div class="spacer"></div>
      ${can("carry_forward") ? `<button class="btn ghost" id="pe-carry">结转损益</button>` : ""}
      ${can("carry_forward") ? `<button class="btn ghost" id="pe-yearend">年末结转</button>` : ""}
      ${can("period_close") ? `<button class="btn primary" id="pe-close">结账</button>` : ""}
      ${can("period_close") ? `<button class="btn danger" id="pe-unclose">反结账</button>` : ""}
    </div>
    <div class="muted" id="pe-status" style="margin-bottom:8px">加载中…</div>
    <div id="pe-issues"></div>`;
  const periodOf = () => $("#pe-period", main).value.trim();
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
  async function refresh() {
    const p = periodOf();
    if (!/^\d{6}$/.test(p)) { $("#pe-status", main).textContent = "期间格式应为 YYYYMM"; return; }
    try {
      const chk = await api(`/periods/${encodeURIComponent(p)}/precheck`);
      const issues = chk.issues || [];
      $("#pe-status", main).innerHTML = `期间 <b>${esc(chk.period)}</b>　已结账至：<b>${esc(chk.closed_upto || "—")}</b>　损益科目：<b>${chk.pl_count}</b> 个　本年利润余额：<b>${esc(chk.profit_balance)}</b>`;
      $("#pe-issues", main).innerHTML = issues.length
        ? `<div class="panel"><b>待处理问题：</b><ul style="margin:6px 0 0 18px">${issues.map((s) => `<li>${esc(s)}</li>`).join("")}</ul></div>`
        : `<div class="panel" style="color:var(--ok,#2e7d32)">✔ 未发现阻断项（是否要求先结转以左侧选项为准）</div>`;
    } catch (e) { $("#pe-status", main).textContent = e.message; $("#pe-issues", main).innerHTML = ""; }
  }
  $("#pe-check", main).onclick = refresh;
  if ($("#pe-carry", main)) $("#pe-carry", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`为 ${p} 生成结转损益凭证（转入本年利润）？生成的凭证为未记账状态，需到凭证列表记账。`, false))) return;
    try { const r = await post(`/periods/${encodeURIComponent(p)}/carry-forward`); toast(`已生成结转凭证 ${r.voucher_no || r.id}（未记账）`, "ok"); refresh(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#pe-yearend", main)) $("#pe-yearend", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`将 ${p} 的「本年利润」余额转入未分配利润？一般在 12 期结账后执行。`, false))) return;
    try { const r = await post(`/periods/${encodeURIComponent(p)}/year-end`); toast(`已生成年末结转凭证 ${r.voucher_no || r.id}（未记账）`, "ok"); refresh(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#pe-close", main)) $("#pe-close", main).onclick = async () => {
    const p = periodOf();
    const req = $("#pe-reqcarry", main).checked;
    if (!(await confirmDialog(`确定对 ${p} 结账？结账后该期间不能再录入/修改凭证。`, true))) return;
    try { await post(`/periods/${encodeURIComponent(p)}/close`, { require_carry: req }); toast(`${p} 已结账`, "ok"); refresh(); } catch (e) { toast(e.message, "err"); refresh(); }
  };
  if ($("#pe-unclose", main)) $("#pe-unclose", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`确定对 ${p} 反结账？反结账后该期间可以重新录入凭证。`, true))) return;
    try { await post(`/periods/${encodeURIComponent(p)}/unclose`); toast(`${p} 已反结账`, "ok"); refresh(); } catch (e) { toast(e.message, "err"); }
  };
  refresh();
}

// ===========================================================================
// 固定资产（与桌面端对齐：卡片 / 折旧计划 / 计提 / 清理）
// ===========================================================================
async function viewAssets(main) {
  const cur = (state.current || "").replace("-", "");
  const METHODS = [["straight", "直线法"], ["ddb", "双倍余额递减法"], ["sum_of_years", "年数总和法"], ["one_time", "一次性摊销法"], ["fifty_fifty", "五五摊销法"]];
  main.innerHTML = `<h2>固定资产</h2>
    <div class="toolbar">
      <label>期间 <input id="as-period" value="${esc(cur)}" style="width:90px" placeholder="YYYYMM" /></label>
      <button class="btn" id="as-load">查询</button>
      <div class="spacer"></div>
      <button class="btn ghost" id="as-print">打印预览</button>
      ${can("voucher_new") ? `<button class="btn ghost" id="as-accrue">计提本期折旧</button>` : ""}
      ${can("account_edit") ? `<button class="btn ghost" id="as-deldep">删除本期折旧</button>` : ""}
      ${can("account_edit") ? `<button class="btn primary" id="as-new">新增卡片</button>` : ""}
    </div>
    <div class="muted" id="as-summary" style="margin-bottom:8px">加载中…</div>
    <div class="panel"><table class="grid" id="as-table"><thead><tr>
      <th>编码</th><th>名称</th><th>部门</th><th class="num">原值</th><th class="num">累计折旧</th><th class="num">净值</th><th>状态</th><th>操作</th>
    </tr></thead><tbody><tr><td colspan="8" class="muted">加载中…</td></tr></tbody></table></div>
    <div id="as-plan" style="margin-top:10px"></div>`;
  const periodOf = () => $("#as-period", main).value.trim();
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });

  async function load() {
    const p = periodOf();
    if (!/^\d{6}$/.test(p)) { $("#as-summary", main).textContent = "期间格式应为 YYYYMM"; return; }
    let d;
    try { d = await api(`/assets?period=${encodeURIComponent(p)}`); }
    catch (e) { $("#as-summary", main).textContent = e.message; return; }
    const planTotal = (d.plan || []).reduce((a, x) => a + (parseFloat(String(x.amount).replace(/,/g, "")) || 0), 0);
    $("#as-summary", main).innerHTML = `共 <b>${(d.cards || []).length}</b> 张卡片　本期应计提 <b>${planTotal.toFixed(2)}</b>　本期已计提记录 <b>${(d.deps || []).length}</b> 条`;
    const rows = d.ledger || [];
    $("#as-table tbody", main).innerHTML = rows.length ? rows.map((r) => {
      const a = r.asset;
      const disposed = a.status === "disposed";
      return `<tr>
        <td>${esc(a.code)}</td><td>${esc(a.name)}</td><td>${esc(a.dept || "—")}</td>
        <td class="num">${esc(a.original_value)}</td><td class="num">${esc(r.accum)}</td><td class="num">${esc(r.net)}</td>
        <td>${disposed ? `<span class="tag err">已清理</span>` : `<span class="tag ok">在用</span>`} ${esc(a.method_label || "")}</td>
        <td class="row-actions">
          ${can("account_edit") && !disposed ? `<button class="btn sm ghost" data-as="edit" data-id="${a.id}">改</button>` : ""}
          ${can("account_edit") ? `<button class="btn sm ghost" data-as="deps" data-id="${a.id}">折旧</button>` : ""}
          ${can("account_edit") && !disposed ? `<button class="btn sm ghost" data-as="dispose" data-id="${a.id}">清理</button>` : ""}
          ${can("account_edit") ? `<button class="btn sm ghost" data-as="del" data-id="${a.id}">删</button>` : ""}
        </td></tr>`;
    }).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:16px">暂无固定资产卡片</td></tr>`;
    const plan = d.plan || [];
    $("#as-plan", main).innerHTML = plan.length ? `<div class="panel"><b>本期折旧计划（未生成凭证前可核对）：</b><table class="grid" style="margin-top:6px"><thead><tr><th>编码</th><th>名称</th><th>部门</th><th class="num">本期应提</th><th class="num">提后累计</th><th class="num">提后净值</th></tr></thead><tbody>${plan.map((x) => `<tr><td>${esc(x.code)}</td><td>${esc(x.name)}</td><td>${esc(x.dept || "—")}</td><td class="num">${esc(x.amount)}</td><td class="num">${esc(x.accum)}</td><td class="num">${esc(x.net)}</td></tr>`).join("")}</tbody></table></div>` : "";
    $all("[data-as]", main).forEach((b) => b.onclick = async () => {
      const id = parseInt(b.dataset.id, 10);
      const card = (d.cards || []).find((x) => x.id === id);
      if (b.dataset.as === "edit" && card) openAssetEditor(card);
      else if (b.dataset.as === "deps") showAssetDeps(card, id);
      else if (b.dataset.as === "dispose") {
        if (!(await confirmDialog(`确定对「${card ? card.name : id}」做资产清理？清理当月仍计提，次月停提。`, true))) return;
        const amt = prompt("清理金额（可留空）", "");
        if (amt === null) return;
        try { await post(`/assets/${id}/dispose`, { ymm: parseInt(periodOf(), 10), amount: amt || "" }); toast("已清理", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      } else if (b.dataset.as === "del") {
        if (!(await confirmDialog("确定删除该资产卡片？（已计提过折旧的卡片不能删除）", true))) return;
        try { await api(`/assets/${id}`, { method: "DELETE" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      }
    });
  }

  function showAssetDeps(card, id) {
    const m = modal(`<h3>折旧明细 — ${esc(card ? card.name : id)}</h3><div id="ad-body" class="muted">加载中…</div>
      <div class="foot"><button class="btn ghost" id="ad-close">关闭</button></div>`);
    $("#ad-close", m).onclick = closeModal;
    api(`/assets/${id}/depreciations`).then((r) => {
      const rows = r.rows || [];
      $("#ad-body", m).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>期间</th><th class="num">本期折旧</th><th class="num">累计折旧</th><th class="num">净值</th><th>凭证</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.period)}</td><td class="num">${esc(x.amount)}</td><td class="num">${esc(x.accum)}</td><td class="num">${esc(x.net_value)}</td><td>${x.voucher_id ? "#" + x.voucher_id : "—"}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无折旧记录</div>`;
    }).catch((e) => { $("#ad-body", m).textContent = e.message; });
  }

  function openAssetEditor(card) {
    const isNew = !card;
    const c = card || { code: "", name: "", category: "", spec: "", dept: "", asset_account: "160101", dep_account: "1602", expense_account: "660201", original_value: "", residual_rate: "5", life_months: 36, method: "straight", start_period: cur, memo: "" };
    const m = modal(`<h3>${isNew ? "新增" : "修改"}资产卡片</h3>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:8px">
        <label>编码 <input id="ae-code" value="${esc(c.code)}" ${isNew ? "" : "disabled"} /></label>
        <label>名称 <input id="ae-name" value="${esc(c.name)}" /></label>
        <label>类别 <input id="ae-cat" value="${esc(c.category || "")}" placeholder="如 电子设备" /></label>
        <label>规格 <input id="ae-spec" value="${esc(c.spec || "")}" /></label>
        <label>使用部门 <input id="ae-dept" value="${esc(c.dept || "")}" /></label>
        <label>启用期间 <input id="ae-start" value="${esc(c.start_period)}" placeholder="YYYYMM" /></label>
        <label>原值 <input id="ae-orig" value="${esc(c.original_value)}" /></label>
        <label>残值率% <input id="ae-residual" value="${esc(c.residual_rate)}" /></label>
        <label>使用月数 <input id="ae-life" type="number" value="${c.life_months}" /></label>
        <label>折旧方法 <select id="ae-method">${METHODS.map(([k, v]) => `<option value="${k}" ${c.method === k ? "selected" : ""}>${v}</option>`).join("")}</select></label>
        <label>资产科目 <input id="ae-acct" value="${esc(c.asset_account)}" /></label>
        <label>累计折旧科目 <input id="ae-depacct" value="${esc(c.dep_account)}" /></label>
        <label>费用科目 <input id="ae-expacct" value="${esc(c.expense_account)}" /></label>
        <label>备注 <input id="ae-memo" value="${esc(c.memo || "")}" /></label>
      </div>
      <div class="foot"><button class="btn primary" id="ae-save">保存</button><button class="btn ghost" id="ae-cancel">取消</button></div>`);
    $("#ae-cancel", m).onclick = closeModal;
    $("#ae-save", m).onclick = async () => {
      const body = {
        id: c.id || 0,
        code: $("#ae-code", m).value.trim(),
        name: $("#ae-name", m).value.trim(),
        category: $("#ae-cat", m).value.trim(),
        spec: $("#ae-spec", m).value.trim(),
        dept: $("#ae-dept", m).value.trim(),
        asset_account: $("#ae-acct", m).value.trim(),
        dep_account: $("#ae-depacct", m).value.trim(),
        expense_account: $("#ae-expacct", m).value.trim(),
        original_value: $("#ae-orig", m).value.trim(),
        residual_rate: $("#ae-residual", m).value.trim(),
        life_months: parseInt($("#ae-life", m).value, 10) || 0,
        method: $("#ae-method", m).value,
        start_period: parseInt($("#ae-start", m).value.trim(), 10) || 0,
        memo: $("#ae-memo", m).value.trim(),
      };
      try {
        if (isNew) await post("/assets", body);
        else await api(`/assets/${c.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok"); closeModal(); load();
      } catch (e) { toast(e.message, "err"); }
    };
  }

  $("#as-load", main).onclick = load;
  $("#as-print", main).onclick = () => printPreview("固定资产台账", $("#as-table", main).querySelector("table"));
  if ($("#as-new", main)) $("#as-new", main).onclick = () => openAssetEditor(null);
  if ($("#as-accrue", main)) $("#as-accrue", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`按部门/费用科目汇总生成 ${p} 折旧凭证？同一期间重复点击不会重复生成。`, false))) return;
    try {
      const r = await post("/assets/depreciate", { ymm: parseInt(p, 10) });
      toast(r.already ? `本期已计提过（凭证 ${r.voucher_no || "—"}）` : `已生成折旧凭证 ${r.voucher_no}，共 ${r.count} 项 ${r.total}`, "ok");
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#as-deldep", main)) $("#as-deldep", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`删除 ${p} 的全部折旧明细（不改凭证）？删除后可重新计提。`, true))) return;
    try { const r = await post("/assets/depreciations/delete-period", { ymm: parseInt(p, 10) }); toast(`已删除 ${r.removed} 条`, "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 银行对账（与桌面端对齐：导入 → 自动勾对 → 手工补勾 → 余额调节表）
// ===========================================================================
async function viewBank(main) {
  const cur = (state.current || "").replace("-", "");
  main.innerHTML = `<h2>银行对账</h2>
    <div class="toolbar">
      <label>期间 <input id="bk-period" value="${esc(cur)}" style="width:90px" /></label>
      <label>银行科目 <input id="bk-acct" value="1002" style="width:100px" /></label>
      <button class="btn" id="bk-load">查询</button>
      <div class="spacer"></div>
      <label>日期容差 <input id="bk-tol" type="number" value="3" style="width:56px" /> 天</label>
      ${can("voucher_new") ? `<button class="btn ghost" id="bk-auto">自动勾对</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="bk-import">导入对账单</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="bk-link">手工勾对</button>` : ""}
      ${can("voucher_new") ? `<button class="btn danger" id="bk-clear">清空对账单</button>` : ""}
    </div>
    <div class="muted" id="bk-sum" style="margin-bottom:8px">加载中…</div>
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:10px">
      <div class="panel"><b>银行对账单</b><div id="bk-stmts"></div></div>
      <div class="panel"><b>账面银行分录（已记账）</b><div id="bk-book"></div></div>
    </div>
    <div class="panel" id="bk-recon" style="margin-top:10px"></div>`;
  let data = null, selStmt = null, selBook = null;
  const periodOf = () => $("#bk-period", main).value.trim();
  const accountOf = () => $("#bk-acct", main).value.trim();
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });

  function render() {
    if (!data) return;
    const stmts = data.statements || [], book = data.book || [];
    $("#bk-sum", main).innerHTML = `银行流水 <b>${stmts.length}</b> 条（已勾 <b>${stmts.filter((s) => s.entry_id).length}</b>）　账面分录 <b>${book.length}</b> 条`;
    $("#bk-stmts", main).innerHTML = `<table class="grid"><thead><tr><th>日期</th><th>摘要</th><th>结算号</th><th class="num">进账</th><th class="num">支出</th><th class="num">余额</th><th></th></tr></thead><tbody>${stmts.length ? stmts.map((s) => `<tr class="${selStmt === s.id ? "row-sel" : ""}" data-stmt="${s.id}"><td>${esc(s.date)}</td><td>${esc(s.summary)}</td><td>${esc(s.settle_no || "")}</td><td class="num">${s.debit === "0.00" ? "" : esc(s.debit)}</td><td class="num">${s.credit === "0.00" ? "" : esc(s.credit)}</td><td class="num">${esc(s.balance)}</td><td>${s.entry_id ? `<button class="btn sm ghost" data-unlink="${s.id}">取消</button>` : `<span class="muted">未勾</span>`}</td></tr>`).join("") : `<tr><td colspan="7" class="muted">暂无对账单，请先导入</td></tr>`}</tbody></table>`;
    $("#bk-book", main).innerHTML = `<table class="grid"><thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借</th><th class="num">贷</th><th></th></tr></thead><tbody>${book.length ? book.map((b) => `<tr class="${selBook === b.entry_id ? "row-sel" : ""}" data-book="${b.entry_id}"><td>${esc(b.date)}</td><td>${esc(b.voucher_no)}</td><td>${esc(b.summary)}</td><td class="num">${b.debit === "0.00" ? "" : esc(b.debit)}</td><td class="num">${b.credit === "0.00" ? "" : esc(b.credit)}</td><td></td></tr>`).join("") : `<tr><td colspan="6" class="muted">该科目本期无已记账分录</td></tr>`}</tbody></table>`;
    const r = data.reconcile || {};
    const list = (arr, f) => (arr || []).length ? `<ul style="margin:4px 0 0 16px">${arr.map((x) => `<li>${f(x)}</li>`).join("")}</ul>` : `<span class="muted">无</span>`;
    $("#bk-recon", main).innerHTML = `<b>余额调节表</b>　${r.balanced ? '<span class="tag ok">调节后一致</span>' : `<span class="tag err">差额 ${esc(r.diff)}</span>`}
      <table class="grid" style="margin-top:6px"><thead><tr><th>口径</th><th class="num">余额</th><th class="num">调节后</th></tr></thead><tbody>
      <tr><td>银行对账单</td><td class="num">${esc(r.bank_balance || "0.00")}</td><td class="num">${esc(r.bank_adjusted || "0.00")}</td></tr>
      <tr><td>企业账面</td><td class="num">${esc(r.book_balance || "0.00")}</td><td class="num">${esc(r.book_adjusted || "0.00")}</td></tr>
      </tbody></table>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:10px;margin-top:8px;font-size:13px">
        <div><b>企业已收、银行未收</b>${list(r.book_only_in, (x) => `${x.voucher_no} ${x.summary} ${x.debit}`)}<b>企业已付、银行未付</b>${list(r.book_only_out, (x) => `${x.voucher_no} ${x.summary} ${x.credit}`)}</div>
        <div><b>银行已收、企业未记</b>${list(r.bank_only_in, (x) => `${x.date} ${x.summary} ${x.debit}`)}<b>银行已付、企业未记</b>${list(r.bank_only_out, (x) => `${x.date} ${x.summary} ${x.credit}`)}</div>
      </div>`;
    $all("[data-stmt]", main).forEach((tr) => tr.onclick = (ev) => { if (ev.target.dataset.unlink) return; selStmt = parseInt(tr.dataset.stmt, 10); render(); });
    $all("[data-book]", main).forEach((tr) => tr.onclick = () => { selBook = parseInt(tr.dataset.book, 10); render(); });
    $all("[data-unlink]", main).forEach((b) => b.onclick = async (ev) => {
      ev.stopPropagation();
      try { await post("/bank/unlink", { stmt_id: parseInt(b.dataset.unlink, 10) }); toast("已取消勾对", "ok"); load(); } catch (e) { toast(e.message, "err"); }
    });
  }

  async function load() {
    const p = periodOf(), acct = accountOf();
    if (!/^\d{6}$/.test(p) || !acct) { $("#bk-sum", main).textContent = "请填写期间与银行科目"; return; }
    try { data = await api(`/bank?period=${encodeURIComponent(p)}&account=${encodeURIComponent(acct)}`); }
    catch (e) { $("#bk-sum", main).textContent = e.message; return; }
    selStmt = selBook = null;
    render();
  }

  $("#bk-load", main).onclick = load;
  if ($("#bk-auto", main)) $("#bk-auto", main).onclick = async () => {
    const p = periodOf(), acct = accountOf(), tol = parseInt($("#bk-tol", main).value, 10) || 0;
    try {
      const r = await post("/bank/auto-match", { ymm: parseInt(p, 10), account: acct, tolerance: tol });
      toast(`自动勾对 ${r.matched} 对（结算号 ${r.by_no}、金额+日期 ${r.by_amount_date}、金额 ${r.by_amount}；存疑 ${r.ambiguous}）`, "ok");
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#bk-link", main)) $("#bk-link", main).onclick = async () => {
    if (!selStmt || !selBook) { toast("请分别在左右两表各选一行", "err"); return; }
    try { await post("/bank/link", { stmt_id: selStmt, entry_id: selBook }); toast("已勾对", "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#bk-import", main)) $("#bk-import", main).onclick = () => {
    const m = modal(`<h3>导入银行对账单</h3>
      <div class="muted" style="font-size:12px;margin-bottom:6px">每行一条，逗号/制表符/分号分隔。支持：日期,摘要,结算号,借方,贷方,余额 或 日期,摘要,结算号,金额,余额（负数支出）。首行表头自动跳过。</div>
      <textarea id="bi-text" style="width:100%;height:220px;font-family:monospace" placeholder="2026-01-06,收到货款,SN001,1000.00,0.00,101000.00"></textarea>
      <div class="foot"><button class="btn primary" id="bi-ok">导入</button><button class="btn ghost" id="bi-cancel">取消</button></div>`);
    $("#bi-cancel", m).onclick = closeModal;
    $("#bi-ok", m).onclick = async () => {
      try {
        const r = await post("/bank/import", { ymm: parseInt(periodOf(), 10), account: accountOf(), text: $("#bi-text", m).value });
        toast(`已导入 ${r.imported} 条`, "ok");
        (r.warnings || []).slice(0, 3).forEach((w) => toast(w, "err"));
        closeModal(); load();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  if ($("#bk-clear", main)) $("#bk-clear", main).onclick = async () => {
    if (!(await confirmDialog(`清空 ${periodOf()} 期 ${accountOf()} 的全部对账单流水？`, true))) return;
    try { const r = await post("/bank/clear", { ymm: parseInt(periodOf(), 10), account: accountOf(), tolerance: 0 }); toast(`已清空 ${r.removed} 条`, "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 往来核销（与桌面端对齐：未核销清单 / 自动核销 / 手工核销 / 账龄）
// ===========================================================================
async function viewSettle(main) {
  const cur = (state.current || "").replace("-", "");
  main.innerHTML = `<h2>往来核销</h2>
    <div class="toolbar">
      <label>往来科目 <input id="st-acct" value="1122" style="width:100px" /></label>
      <label>截止期间 <input id="st-upto" value="${esc(cur)}" style="width:90px" /></label>
      <button class="btn" id="st-load">查询未核销</button>
      <div class="spacer"></div>
      <label>尾差 <input id="st-tol" value="0.01" style="width:60px" /></label>
      ${can("voucher_new") ? `<button class="btn ghost" id="st-auto">自动核销</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="st-manual">手工核销</button>` : ""}
      <button class="btn ghost" id="st-aging">账龄分析</button>
      <button class="btn ghost" id="st-records">核销记录</button>
    </div>
    <div class="muted" id="st-sum" style="margin-bottom:8px">加载中…</div>
    <div class="panel"><div id="st-table"></div></div>
    <div id="st-extra" style="margin-top:10px"></div>`;
  let selFrom = null, selTo = null, rows = [];
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
  const acct = () => $("#st-acct", main).value.trim();
  const upto = () => $("#st-upto", main).value.trim();

  function render() {
    const totalOpen = rows.reduce((a, r) => a + (parseFloat(String(r.open).replace(/,/g, "")) || 0), 0);
    $("#st-sum", main).innerHTML = `未核销 <b>${rows.length}</b> 笔，合计 <b>${totalOpen.toFixed(2)}</b>`;
    $("#st-table", main).innerHTML = `<table class="grid"><thead><tr><th>日期</th><th>凭证号</th><th>往来对象</th><th>摘要</th><th>方向</th><th class="num">发生额</th><th class="num">已核销</th><th class="num">未核销</th><th>选择</th></tr></thead><tbody>${rows.length ? rows.map((r) => `<tr><td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.aux_key || "—")}</td><td>${esc(r.summary)}</td><td>${esc(r.dir)}</td><td class="num">${esc(r.debit === "0.00" ? r.credit : r.debit)}</td><td class="num">${esc(r.settled)}</td><td class="num">${esc(r.open)}</td><td>
      <button class="btn sm ${selFrom === r.entry_id ? "primary" : "ghost"}" data-from="${r.entry_id}">原单</button>
      <button class="btn sm ${selTo === r.entry_id ? "primary" : "ghost"}" data-to="${r.entry_id}">收/付</button></td></tr>`).join("") : `<tr><td colspan="9" class="muted" style="text-align:center;padding:16px">没有未核销分录</td></tr>`}</tbody></table>`;
    $all("[data-from]", main).forEach((b) => b.onclick = () => { selFrom = parseInt(b.dataset.from, 10); render(); });
    $all("[data-to]", main).forEach((b) => b.onclick = () => { selTo = parseInt(b.dataset.to, 10); render(); });
  }

  async function load() {
    const a = acct(), u = upto();
    try {
      const d = await api(`/settle/open?account=${encodeURIComponent(a)}&upto=${encodeURIComponent(u)}`);
      rows = d.rows || [];
      selFrom = selTo = null;
      $("#st-extra", main).innerHTML = "";
      render();
    } catch (e) { $("#st-sum", main).textContent = e.message; }
  }

  $("#st-load", main).onclick = load;
  if ($("#st-auto", main)) $("#st-auto", main).onclick = async () => {
    const p = /^\d{6}$/.test(upto()) ? parseInt(upto(), 10) : 0;
    if (!(await confirmDialog(`对 ${acct()} 自动核销（等额优先，保守不勾错）？`, false))) return;
    try { const r = await post("/settle/auto", { account: acct(), ymm: p, tolerance: $("#st-tol", main).value.trim() }); toast(`核销 ${r.pairs} 对，金额 ${r.amount}（精确 ${r.exact}、尾差抹平 ${r.written_off}）`, "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-manual", main)) $("#st-manual", main).onclick = async () => {
    if (!selFrom || !selTo) { toast("请分别选择「原单」与「收/付」两行", "err"); return; }
    const amt = prompt("核销金额", "");
    if (amt === null) return;
    try { await post("/settle/run", { from_entry: selFrom, to_entry: selTo, amount: amt }); toast("已核销", "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-aging", main)) $("#st-aging", main).onclick = async () => {
    try {
      const d = await api(`/settle/aging?account=${encodeURIComponent(acct())}&upto=${encodeURIComponent(upto())}`);
      $("#st-extra", main).innerHTML = `<div class="panel"><b>账龄分析（${esc(d.as_of)}）</b><table class="grid" style="margin-top:6px"><thead><tr><th>往来对象</th>${d.buckets.map((b) => `<th class="num">${esc(b)}</th>`).join("")}<th class="num">借方合计</th><th class="num">贷方合计</th><th class="num">净额</th><th class="num">最老天数</th></tr></thead><tbody>${(d.rows || []).length ? d.rows.map((r) => `<tr><td>${esc(r.key || "—")}</td>${r.amounts.map((x) => `<td class="num">${esc(x)}</td>`).join("")}<td class="num">${esc(r.total)}</td><td class="num">${esc(r.credit_total)}</td><td class="num">${esc(r.net)}</td><td class="num">${r.max_days}</td></tr>`).join("") : `<tr><td colspan="${d.buckets.length + 5}" class="muted">无数据</td></tr>`}</tbody></table></div>`;
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-records", main)) $("#st-records", main).onclick = async () => {
    try {
      const d = await api(`/settle/records?account=${encodeURIComponent(acct())}`);
      const rows2 = d.rows || [];
      $("#st-extra", main).innerHTML = `<div class="panel"><b>核销记录</b><table class="grid" style="margin-top:6px"><thead><tr><th>期间</th><th>往来对象</th><th class="num">金额</th><th>操作人</th><th>时间</th><th></th></tr></thead><tbody>${rows2.length ? rows2.map((r) => `<tr><td>${esc(r.period)}</td><td>${esc(r.aux_key || "—")}</td><td class="num">${esc(r.amount)}</td><td>${esc(r.settled_by)}</td><td>${esc(r.settled_at)}</td><td>${can("voucher_new") ? `<button class="btn sm ghost" data-unsettle="${r.id}">取消核销</button>` : ""}</td></tr>`).join("") : `<tr><td colspan="6" class="muted">暂无记录</td></tr>`}</tbody></table></div>`;
      $all("[data-unsettle]", main).forEach((b) => b.onclick = async () => {
        try { await post("/settle/unsettle", { id: parseInt(b.dataset.unsettle, 10) }); toast("已取消", "ok"); $("#st-records", main).click(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 自定义报表（UFO 公式，与桌面端对齐）
// ===========================================================================
async function viewCustomReports(main) {
  const cur = (state.current || "").replace("-", "");
  main.innerHTML = `<h2>自定义报表</h2>
    <div class="toolbar">
      <label>报表 <select id="cr-list" style="min-width:160px"></select></label>
      <button class="btn sm" id="cr-load">打开</button>
      ${can("account_edit") ? `<button class="btn ghost sm" id="cr-new">新建</button>
      <button class="btn danger sm" id="cr-del">删除</button>` : ""}
      <div class="spacer"></div>
      <label>期间 <input id="cr-period" value="${esc(cur)}" style="width:90px" /></label>
      <button class="btn ghost sm" id="cr-preview">生成</button>
      <button class="btn ghost sm" id="cr-print">打印预览</button>
    </div>
    <div class="panel">
      <div class="field"><label>名称</label><input id="cr-name" /></div>
      <div class="field"><label>列标题（逗号分隔）</label><input id="cr-cols" placeholder="本期,本年累计" /></div>
      <div style="margin-top:6px">
        ${can("account_edit") ? `<button class="btn ghost sm" id="cr-addrow">+ 行</button>
        <button class="btn ghost sm" id="cr-addcol">+ 列</button>
        <button class="btn sm" id="cr-save">保存</button>` : `<span class="muted">无修改权限（需要 account_edit）</span>`}
        <span id="cr-err" class="muted" style="color:var(--err)"></span>
      </div>
      <div id="cr-grid" style="margin-top:8px" class="muted">新建或打开一张报表</div>
      <div class="muted" style="font-size:12px;margin-top:6px">公式示例：QM("1001") 期末余额、QC("1001") 期初余额、FS("6001",-1,"贷") 上期贷方发生额、LFS("6001") 本年累计；支持 + - * / 与括号。</div>
    </div>
    <div id="cr-preview-box" style="margin-top:10px"></div>`;
  let report = null;
  async function refreshList() {
    try {
      const list = await api("/custom-reports");
      $("#cr-list", main).innerHTML = list.length
        ? list.map((r) => `<option value="${esc(r.key)}">${esc(r.key)} ${esc(r.name)}</option>`).join("")
        : `<option value="">（暂无）</option>`;
    } catch (e) { toast(e.message, "err"); }
  }
  function renderGrid() {
    const box = $("#cr-grid", main);
    if (!report) { box.className = "muted"; box.innerHTML = "新建或打开一张报表"; return; }
    box.className = "";
    const cols = report.columns || [];
    const head = `<tr><th>行名称</th>${cols.map((c, i) => `<th>${esc(c)} <button class="btn sm ghost" data-delcol="${i}">×</button></th>`).join("")}<th></th></tr>`;
    const body = (report.lines || []).map((l, li) => `<tr>
      <td><input class="cr-lname" data-li="${li}" value="${esc(l.name)}" style="width:130px" /></td>
      ${cols.map((_, ci) => `<td><input class="cr-f" data-li="${li}" data-ci="${ci}" value="${esc((l.formulas || [])[ci] || "")}" style="width:150px" placeholder='如 QM("1001")' /></td>`).join("")}
      <td><button class="btn sm ghost" data-delrow="${li}">×</button></td></tr>`).join("");
    box.innerHTML = `<table class="grid"><thead>${head}</thead><tbody>${body || `<tr><td colspan="${cols.length + 2}" class="muted">暂无行，点「+ 行」新增</td></tr>`}</tbody></table>`;
    $all(".cr-lname", box).forEach((inp) => inp.oninput = () => { report.lines[+inp.dataset.li].name = inp.value; });
    $all(".cr-f", box).forEach((inp) => inp.oninput = () => {
      const l = report.lines[+inp.dataset.li];
      l.formulas = l.formulas || [];
      while (l.formulas.length <= +inp.dataset.ci) l.formulas.push("");
      l.formulas[+inp.dataset.ci] = inp.value;
    });
    $all("[data-delrow]", box).forEach((b) => b.onclick = () => { report.lines.splice(+b.dataset.delrow, 1); renderGrid(); });
    $all("[data-delcol]", box).forEach((b) => b.onclick = () => {
      const i = +b.dataset.delcol;
      report.columns.splice(i, 1);
      report.lines.forEach((l) => (l.formulas || []).splice(i, 1));
      renderGrid();
    });
  }
  function fillForm() {
    $("#cr-name", main).value = report ? report.name : "";
    $("#cr-cols", main).value = report ? (report.columns || []).join(",") : "";
    renderGrid();
  }
  async function open(key) {
    try { const d = await api(`/custom-reports/${encodeURIComponent(key)}`); report = d.report; fillForm(); }
    catch (e) { toast(e.message, "err"); }
  }
  await refreshList();
  const first = $("#cr-list", main).value;
  if (first) await open(first);
  $("#cr-load", main).onclick = () => { const k = $("#cr-list", main).value; if (k) open(k); };
  if ($("#cr-new", main)) $("#cr-new", main).onclick = () => {
    report = { key: "", name: "新报表", columns: ["本期", "本年累计"], lines: [{ name: "", indent: 0, formulas: ["", ""], bold: false }] };
    fillForm();
  };
  if ($("#cr-del", main)) $("#cr-del", main).onclick = async () => {
    if (!report || !report.key) { toast("请先打开一张已保存的报表", "err"); return; }
    if (!(await confirmDialog(`删除自定义报表「${report.name}」？`, true))) return;
    try { await api(`/custom-reports/${encodeURIComponent(report.key)}/delete`, { method: "POST" }); toast("已删除", "ok"); report = null; fillForm(); refreshList(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#cr-addrow", main)) $("#cr-addrow", main).onclick = () => {
    if (!report) return;
    report.lines.push({ name: "", indent: 0, formulas: (report.columns || []).map(() => ""), bold: false });
    renderGrid();
  };
  if ($("#cr-addcol", main)) $("#cr-addcol", main).onclick = () => {
    if (!report) return;
    report.columns.push(`列${report.columns.length + 1}`);
    report.lines.forEach((l) => { l.formulas = l.formulas || []; l.formulas.push(""); });
    fillForm();
  };
  if ($("#cr-save", main)) $("#cr-save", main).onclick = async () => {
    if (!report) return;
    report.name = $("#cr-name", main).value.trim();
    report.columns = $("#cr-cols", main).value.split(",").map((s) => s.trim()).filter(Boolean);
    try {
      const r = await api("/custom-reports", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(report) });
      report.key = r.key;
      const errs = r.errors || [];
      $("#cr-err", main).textContent = errs.length ? `公式有误：第 ${errs[0].line + 1} 行第 ${errs[0].column + 1} 列 ${errs[0].error}` : "";
      toast("已保存", "ok");
      refreshList();
    } catch (e) { toast(e.message, "err"); }
  };
  $("#cr-preview", main).onclick = async () => {
    if (!report || !report.key) { toast("请先保存报表", "err"); return; }
    try {
      const d = await api(`/custom-reports/${encodeURIComponent(report.key)}?period=${encodeURIComponent($("#cr-period", main).value.trim())}`);
      const cols = d.report.columns || [];
      const vals = d.values || [];
      $("#cr-preview-box", main).innerHTML = `<div class="panel"><b>${esc(d.report.name)}（${esc(d.period)}）</b><table class="grid" id="cr-preview-table" style="margin-top:6px"><thead><tr><th>项目</th>${cols.map((c) => `<th class="num">${esc(c)}</th>`).join("")}</tr></thead><tbody>${vals.length ? vals.map((row, i) => `<tr><td>${esc((d.report.lines[i] || {}).name || "")}</td>${row.map((v) => `<td class="num">${esc(v)}</td>`).join("")}</tr>`).join("") : `<tr><td colspan="${cols.length + 1}" class="muted">无数据</td></tr>`}</tbody></table></div>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#cr-print", main).onclick = () => {
    const t = $("#cr-preview-table", main);
    if (!t) { toast("请先生成", "err"); return; }
    printPreview("自定义报表", t);
  };
}

// ===========================================================================
// 采购对账
// ===========================================================================
async function viewPoReconcile(main) {
    main.innerHTML = `<h2>采购对账</h2>
    <div class="toolbar"><button class="btn ghost sm" id="pr-result-print">打印预览</button></div>
    <div id="pr-result" class="muted">加载中…</div>`;
  $("#pr-result-print").addEventListener("click", () => { const el = $("#pr-result").querySelector("table"); printPreview("采购对账", el); });
  try {
    const r = await api("/procure/reconcile");
    const rows = r.rows || [];
    $("#pr-result").innerHTML = `<table><thead><tr><th>订单号</th><th>供应商</th><th>订单金额</th><th>已付款</th><th>未付款</th><th>暂估</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${esc(x.no)}</td><td>${esc(x.supplier)}</td><td class="r">${fmt(x.order_amount)}</td><td class="r">${fmt(x.paid)}</td><td class="r">${fmt(x.unpaid)}</td><td class="r">${fmt(x.open_estimate)}</td></tr>`).join("")}</tbody></table>`;
  } catch (e) { $("#pr-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 销售对账
// ===========================================================================
async function viewSoReconcile(main) {
    main.innerHTML = `<h2>销售对账</h2>
    <div class="toolbar"><button class="btn ghost sm" id="sr-result-print">打印预览</button></div>
    <div id="sr-result" class="muted">加载中…</div>`;
  $("#sr-result-print").addEventListener("click", () => { const el = $("#sr-result").querySelector("table"); printPreview("销售对账", el); });
  try {
    const r = await api("/sales/reconcile");
    const rows = r.rows || [];
    $("#sr-result").innerHTML = `<table><thead><tr><th>订单号</th><th>客户</th><th>订单金额</th><th>已收款</th><th>未收款</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${esc(x.no)}</td><td>${esc(x.customer)}</td><td class="r">${fmt(x.order_amount)}</td><td class="r">${fmt(x.received)}</td><td class="r">${fmt(x.unreceived)}</td></tr>`).join("")}</tbody></table>`;
  } catch (e) { $("#sr-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 库存账龄
// ===========================================================================
async function viewInvAging(main) {
    main.innerHTML = `<h2>库存账龄分析</h2>
    <div class="toolbar"><button class="btn ghost sm" id="ia-result-print">打印预览</button></div>
    <div id="ia-result" class="muted">加载中…</div>`;
  $("#ia-result-print").addEventListener("click", () => { const el = $("#ia-result").querySelector("table"); printPreview("库存账龄分析", el); });
  try {
    const r = await api("/inventory/aging");
    const rows = r.rows || [];
    $("#ia-result").innerHTML = `<table><thead><tr><th>存货</th><th>最近入库</th><th>账龄(天)</th><th>结存数量</th><th>结存金额</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${esc(x.item)}</td><td>${esc(x.last_in || "—")}</td><td class="r">${x.days}</td><td class="r">${fmt(x.qty)}</td><td class="r">${fmt(x.amount)}</td></tr>`).join("")}</tbody></table>`;
  } catch (e) { $("#ia-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 库存 ABC
// ===========================================================================
async function viewInvAbc(main) {
  main.innerHTML = `<h2>库存 ABC 分析</h2>
    <div class="toolbar"><button class="btn ghost sm" id="ib-result-print">打印预览</button></div>
    <div id="ib-result" class="muted">加载中…</div>`;
  $("#ib-result-print").addEventListener("click", () => { const el = $("#ib-result").querySelector("table"); printPreview("库存ABC分析", el); });
  try {
    const r = await api("/inventory/abc");
    const rows = r.rows || [];
    $("#ib-result").innerHTML = `<table><thead><tr><th>存货</th><th>结存金额</th><th>累计占比</th><th>分类</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${esc(x.item)}</td><td class="r">${fmt(x.amount)}</td><td class="r">${x.cum_pct}%</td><td>${esc(x.class)}</td></tr>`).join("")}</tbody></table>`;
  } catch (e) { $("#ib-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 库存深度：序列号 / 多单位 / 组装拆卸 / 分仓库 / 调拨
// ===========================================================================
function postJson(path, body) {
  return api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
}

async function viewInvSerial(main) {
  main.innerHTML = `<h2>序列号管理</h2>
    <div class="toolbar">
      <label>存货 <input id="is-item" style="width:140px" /></label>
      <button class="btn primary" id="is-load">查询在库</button>
      <span class="grow"></span>
    </div>
    <div class="toolbar">
      <label>日期 <input id="is-date" style="width:110px" /></label>
      <label>批次 <input id="is-batch" style="width:110px" /></label>
      <label>序列号(逗号分隔) <input id="is-serials" style="width:240px" placeholder="S001,S002" /></label>
      <button class="btn" id="is-in">入库登记</button>
      <button class="btn" id="is-out">出库登记</button>
    </div>
    <div id="is-result" class="muted">填写存货后点击查询</div>`;
  $("#is-date").value = new Date().toISOString().slice(0, 10);
  $("#is-load").addEventListener("click", async () => {
    const item = $("#is-item").value.trim();
    if (!item) { toast("请填写存货", "err"); return; }
    try {
      const r = await api(`/inventory/serial?item=${encodeURIComponent(item)}`);
      const rows = r.rows || [];
      $("#is-result").innerHTML = rows.length
        ? `<table><thead><tr><th>序列号</th><th>存货</th><th>批次</th><th>状态</th><th>入库日期</th><th>出库日期</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x.serial)}</td><td>${esc(x.item)}</td><td>${esc(x.batch_no)}</td><td>${esc(x.status === "in" ? "在库" : x.status === "out" ? "已出库" : "报废")}</td><td>${esc(x.in_date)}</td><td>${esc(x.out_date || "—")}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无序列号记录</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
  $("#is-in").addEventListener("click", async () => {
    const item = $("#is-item").value.trim();
    const serials = ($("#is-serials").value || "").split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean);
    if (!item || !serials.length) { toast("请填写存货与序列号", "err"); return; }
    try {
      const r = await postJson("/inventory/serial", { item, serials, batch_no: $("#is-batch").value.trim(), date: $("#is-date").value.trim() });
      toast(`已入库 ${r.count} 个序列号`, "ok");
      $("#is-serials").value = "";
      $("#is-load").click();
    } catch (e) { toast(e.message, "err"); }
  });
  $("#is-out").addEventListener("click", async () => {
    const serials = ($("#is-serials").value || "").split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean);
    if (!serials.length) { toast("请填写序列号", "err"); return; }
    try {
      const r = await postJson("/inventory/serial/out", { serials, date: $("#is-date").value.trim() });
      toast(`已出库 ${r.count} 个序列号`, "ok");
      $("#is-serials").value = "";
      $("#is-load").click();
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewInvUnit(main) {
  main.innerHTML = `<h2>多单位换算</h2>
    <div class="toolbar">
      <label>存货 <input id="iu-item" style="width:140px" /></label>
      <button class="btn primary" id="iu-load">载入</button>
      <span class="grow"></span>
    </div>
    <div class="toolbar">
      <label>主单位 <input id="iu-base" style="width:90px" /></label>
      <label>辅助单位 <input id="iu-alt" style="width:90px" /></label>
      <label>系数(1主=系数辅) <input id="iu-factor" style="width:90px" /></label>
      <button class="btn" id="iu-save">保存换算</button>
    </div>
    <div id="iu-info" class="muted"></div>`;
  $("#iu-load").addEventListener("click", async () => {
    const item = $("#iu-item").value.trim();
    if (!item) { toast("请填写存货", "err"); return; }
    try {
      const r = await api(`/inventory/unit?item=${encodeURIComponent(item)}`);
      if (r.unit) { $("#iu-base").value = r.unit.base_unit || ""; $("#iu-alt").value = r.unit.alt_unit || ""; $("#iu-factor").value = r.unit.factor || ""; $("#iu-info").textContent = "已载入现有换算"; }
      else { $("#iu-base").value = ""; $("#iu-alt").value = ""; $("#iu-factor").value = ""; $("#iu-info").textContent = "该存货尚未设置换算"; }
    } catch (e) { toast(e.message, "err"); }
  });
  $("#iu-save").addEventListener("click", async () => {
    const item = $("#iu-item").value.trim();
    if (!item) { toast("请填写存货", "err"); return; }
    try {
      await postJson("/inventory/unit", { item, base_unit: $("#iu-base").value.trim(), alt_unit: $("#iu-alt").value.trim(), factor: $("#iu-factor").value.trim() });
      toast("已保存换算", "ok");
      $("#iu-load").click();
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewInvAssemble(main) {
  main.innerHTML = `<h2>组装 / 拆卸</h2>
    <div class="toolbar">
      <label>日期 <input id="ia-date" style="width:110px" /></label>
      <label>成品/母件 <input id="ia-parent" style="width:140px" /></label>
      <label>备注 <input id="ia-memo" style="width:160px" /></label>
      <span class="grow"></span>
    </div>
    <div class="toolbar">
      <label>子件(名称:数量, 每行一个)</label>
      <textarea id="ia-children" style="width:420px;height:70px" placeholder="RM1:2&#10;RM2:1"></textarea>
      <button class="btn" id="ia-do">组装</button>
      <button class="btn" id="ia-undo">拆卸</button>
    </div>`;
  $("#ia-date").value = new Date().toISOString().slice(0, 10);
  const doOp = async (disassemble) => {
    const parent = $("#ia-parent").value.trim();
    const children = ($("#ia-children").value || "").split("\n").map((l) => l.trim()).filter(Boolean).map((l) => {
      const i = l.search(/[:：]/);
      return i < 0 ? [l, "0"] : [l.slice(0, i).trim(), l.slice(i + 1).trim()];
    });
    if (!parent || !children.length) { toast("请填写母件与子件", "err"); return; }
    try {
      await postJson(disassemble ? "/inventory/disassemble" : "/inventory/assemble", { parent, children, memo: $("#ia-memo").value.trim(), date: $("#ia-date").value.trim() });
      toast(disassemble ? "已拆卸" : "已组装", "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  $("#ia-do").addEventListener("click", () => doOp(false));
  $("#ia-undo").addEventListener("click", () => doOp(true));
}

async function viewInvWarehouse(main) {
  main.innerHTML = `<h2>分仓库库存</h2>
    <div class="toolbar">
      <label>存货 <input id="iw-item" style="width:160px" /></label>
      <button class="btn primary" id="iw-load">查询</button>
      <button class="btn ghost sm" id="iw-load-print">打印预览</button>
    </div>
    <div id="iw-result" class="muted">填写存货后点击查询</div>`;
  $("#iw-load").addEventListener("click", async () => {
  $("#iw-load-print").addEventListener("click", () => { const el = $("#iw-result").querySelector("table"); printPreview("分仓库库存", el); });
    const item = $("#iw-item").value.trim();
    if (!item) { toast("请填写存货", "err"); return; }
    try {
      const r = await api(`/inventory/warehouse-stock?item=${encodeURIComponent(item)}`);
      const rows = r.rows || [];
      $("#iw-result").innerHTML = rows.length
        ? `<table><thead><tr><th>仓库</th><th>存货</th><th>结存数量</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x.warehouse)}</td><td>${esc(x.item)}</td><td class="r">${fmt(x.qty)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无库存</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewInvTransfer(main) {
    main.innerHTML = `<h2>调拨报表</h2>
    <div class="toolbar"><button class="btn ghost sm" id="it-result-print">打印预览</button></div>
    <div id="it-result" class="muted">加载中…</div>`;
  $("#it-result-print").addEventListener("click", () => { const el = $("#it-result").querySelector("table"); printPreview("调拨报表", el); });
  try {
    const r = await api("/inventory/transfer");
    const rows = r.rows || [];
    $("#it-result").innerHTML = rows.length
      ? `<table><thead><tr><th>日期</th><th>存货</th><th>仓库</th><th>数量</th><th>备注</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.date)}</td><td>${esc(x.item)}</td><td>${esc(x.warehouse)}</td><td class="r">${fmt(x.qty)}</td><td>${esc(x.memo || "")}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">本期间无调拨流水</div>`;
  } catch (e) { $("#it-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 采购/销售深度：采购暂估 / 供应商配额 / 订单变更
// ===========================================================================
async function viewPoEstimate(main) {
  main.innerHTML = `<h2>采购暂估</h2>
    <div class="toolbar">
      <label>采购订单ID <input id="pe-poid" style="width:90px" /></label>
      <button class="btn primary" id="pe-load">查询暂估</button>
      <span class="grow"></span>
    </div>
    <div class="toolbar">
      <label>存货 <input id="pe-item" style="width:140px" /></label>
      <label>暂估金额 <input id="pe-amount" style="width:110px" /></label>
      <button class="btn" id="pe-add">登记暂估</button>
    </div>
    <div id="pe-result" class="muted">填写订单ID后查询</div>`;
  const load = async () => {
    const poId = $("#pe-poid").value.trim();
    if (!poId) { toast("请填写采购订单ID", "err"); return; }
    try {
      const r = await api(`/procure/estimate?po_id=${encodeURIComponent(poId)}`);
      const rows = r.rows || [];
      const open = rows.filter((x) => !x.settled).reduce((s, x) => s + Number(x.est_amount || 0), 0);
      $("#pe-result").innerHTML = `<div class="muted">未冲回暂估合计：<b>${fmtMoney(String(open))}</b></div>` + (rows.length
        ? `<table><thead><tr><th>#</th><th>存货</th><th>暂估金额</th><th>状态</th><th>操作</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${x.id}</td><td>${esc(x.item)}</td><td class="r">${fmt(x.est_amount)}</td><td>${x.settled ? "已冲回" : "未冲回"}</td><td>${x.settled ? "" : `<button class="btn sm" data-settle="${x.id}">冲回</button>`}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无暂估记录</div>`);
      $all("[data-settle]", $("#pe-result")).forEach((b) => b.onclick = async () => {
        try { await postJson(`/procure/estimate/${b.dataset.settle}/settle`, {}); toast("已冲回", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#pe-load").addEventListener("click", load);
  $("#pe-add").addEventListener("click", async () => {
    const po_id = parseInt($("#pe-poid").value.trim(), 10);
    if (!po_id) { toast("请填写采购订单ID", "err"); return; }
    try {
      await postJson("/procure/estimate", { po_id, item: $("#pe-item").value.trim(), est_amount: $("#pe-amount").value.trim() });
      toast("已登记暂估", "ok");
      $("#pe-amount").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewProcureQuota(main) {
  main.innerHTML = `<h2>供应商配额</h2>
    <div class="toolbar">
      <label>供应商 <input id="pq-sup" style="width:140px" /></label>
      <label>物料 <input id="pq-item" style="width:140px" /></label>
      <label>配额数量 <input id="pq-qty" style="width:110px" /></label>
      <button class="btn" id="pq-save">保存配额</button>
      <button class="btn primary" id="pq-query">查询剩余</button>
    </div>
    <div id="pq-result" class="muted"></div>`;
  $("#pq-save").addEventListener("click", async () => {
    const supplier = $("#pq-sup").value.trim(), item = $("#pq-item").value.trim();
    if (!supplier || !item) { toast("请填写供应商与物料", "err"); return; }
    try {
      await postJson("/procure/quota", { supplier, item, quota_qty: $("#pq-qty").value.trim() });
      toast("已保存配额", "ok");
      $("#pq-result").textContent = `剩余配额：${fmt($("#pq-qty").value.trim())}`;
    } catch (e) { toast(e.message, "err"); }
  });
  $("#pq-query").addEventListener("click", async () => {
    const supplier = $("#pq-sup").value.trim(), item = $("#pq-item").value.trim();
    if (!supplier || !item) { toast("请填写供应商与物料", "err"); return; }
    try {
      const r = await api(`/procure/quota?supplier=${encodeURIComponent(supplier)}&item=${encodeURIComponent(item)}`);
      $("#pq-result").textContent = r.remaining == null ? "该供应商/物料未设置配额" : `剩余配额：${fmt(String(r.remaining))}`;
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewPoDoc(main) {
  const period = encodeURIComponent(state.current || "");
  const table = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>单号</th><th>存货</th><th>数量</th><th>状态</th><th>请购人</th><th>备注</th><th></th></tr></thead>
      <tbody>${rows.map((r) => `<tr><td>${esc(r.no)}</td><td>${esc(r.item_name)}</td><td class="num">${esc(r.qty)}</td><td>${esc(r.status)}</td><td>${esc(r.requester)}</td><td>${esc(r.memo)}</td><td>${r.status === "draft" ? `<button class="btn ghost sm" data-req-approve="${r.id}">审批</button>` : ""}</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无请购单</div>`;
  const poTrack = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>订单号</th><th>供应商</th><th>订单数量</th><th>到货数量</th><th>执行率</th></tr></thead>
      <tbody>${rows.map((t) => `<tr><td>${esc(t.no)}</td><td>${esc(t.supplier_name)}</td><td class="num">${esc(t.ordered_qty)}</td><td class="num">${esc(t.received_qty)}</td><td class="num">${esc(t.rate)}%</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无采购订单</div>`;

  main.innerHTML = `<h2>采购单据</h2>
    <div class="toolbar">
      <label>存货 <input id="pd-item" style="width:130px" /></label>
      <label>数量 <input id="pd-qty" style="width:80px" /></label>
      <label>备注 <input id="pd-memo" style="width:140px" /></label>
      <button class="btn primary" id="pd-save">保存请购单</button>
    </div>
    <div class="toolbar">
      <label>采购订单ID <input id="pd-poid" style="width:80px" /></label>
      <label>数量/金额 <input id="pd-amt" style="width:100px" /></label>
      <label>备注 <input id="pd-memo2" style="width:120px" /></label>
      <button class="btn" id="pd-receipt">到货</button>
      <button class="btn" id="pd-return">退货</button>
      <button class="btn" id="pd-pay">付款</button>
    </div>
    <div class="panel" style="margin-top:12px"><h4>请购单</h4><div id="pd-list">加载中…</div></div>
    <div class="panel" style="margin-top:12px"><h4>采购订单执行跟踪</h4><div id="pd-track">加载中…</div></div>`;

  const load = async () => {
    try {
      const r = await api(`/procure/req?period=${period}`);
      $("#pd-list").innerHTML = table(r.rows || []);
      $all("[data-req-approve]").forEach((b) => b.onclick = async () => {
        try { await api(`/procure/req/${b.dataset.reqApprove}/approve`, { method: "POST" }); toast("已审批", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#pd-list").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
    try {
      const t = await api("/procure/track");
      $("#pd-track").innerHTML = poTrack(t.rows || []);
    } catch (e) { $("#pd-track").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#pd-save").addEventListener("click", async () => {
    try {
      await postJson("/procure/req", { id: 0, period: ymm(state.current || ""), date: today(), item_code: $("#pd-item").value.trim(), item_name: $("#pd-item").value.trim(), qty: $("#pd-qty").value.trim() || "0", status: "draft", requester: "", memo: $("#pd-memo").value.trim() });
      toast("已保存请购单", "ok"); $("#pd-qty").value = ""; $("#pd-memo").value = ""; load();
    } catch (e) { toast(e.message, "err"); }
  });
  const poid = () => parseInt($("#pd-poid").value.trim(), 10) || 0;
  const amt = () => $("#pd-amt").value.trim();
  const memo = () => $("#pd-memo2").value.trim();
  $("#pd-receipt").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { await postJson("/procure/receipt", { po_id: poid(), period: ymm(state.current || ""), date: today(), qty: amt(), memo: memo() }); toast("已到货", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#pd-return").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { await postJson("/procure/return", { po_id: poid(), period: ymm(state.current || ""), date: today(), qty: amt(), memo: memo() }); toast("已退货", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#pd-pay").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { await postJson("/procure/payment", { po_id: poid(), period: ymm(state.current || ""), date: today(), amount: amt(), memo: memo() }); toast("已付款", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  load();
}

async function viewSoDoc(main) {
  const period = encodeURIComponent(state.current || "");
  const table = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>单号</th><th>客户</th><th>存货</th><th>数量</th><th>单价</th><th>状态</th><th></th></tr></thead>
      <tbody>${rows.map((r) => `<tr><td>${esc(r.no)}</td><td>${esc(r.customer_name)}</td><td>${esc(r.item_name)}</td><td class="num">${esc(r.qty)}</td><td class="num">${esc(r.unit_price)}</td><td>${esc(r.status)}</td><td>${r.status === "draft" ? `<button class="btn ghost sm" data-quo-approve="${r.id}">审批</button>` : ""}</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无报价单</div>`;
  const soTrack = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>订单号</th><th>客户</th><th>订单数量</th><th>发货数量</th><th>执行率</th></tr></thead>
      <tbody>${rows.map((t) => `<tr><td>${esc(t.no)}</td><td>${esc(t.customer_name)}</td><td class="num">${esc(t.ordered_qty)}</td><td class="num">${esc(t.shipped_qty)}</td><td class="num">${esc(t.rate)}%</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无销售订单</div>`;

  main.innerHTML = `<h2>销售单据</h2>
    <div class="toolbar">
      <label>客户 <input id="sd-cust" style="width:110px" /></label>
      <label>存货 <input id="sd-item" style="width:130px" /></label>
      <label>数量 <input id="sd-qty" style="width:70px" /></label>
      <label>单价 <input id="sd-price" style="width:80px" /></label>
      <button class="btn primary" id="sd-save">保存报价单</button>
    </div>
    <div class="toolbar">
      <label>销售订单ID <input id="sd-soid" style="width:80px" /></label>
      <label>数量/金额 <input id="sd-amt" style="width:100px" /></label>
      <label>备注 <input id="sd-memo" style="width:120px" /></label>
      <button class="btn" id="sd-ship">发货</button>
      <button class="btn" id="sd-return">退货</button>
      <button class="btn" id="sd-pay">收款</button>
      <span class="spacer"></span>
      <label>客户 <input id="sd-credit-cust" style="width:110px" /></label>
      <button class="btn" id="sd-credit">信用检查</button>
    </div>
    <div id="sd-credit-result" class="muted" style="margin-top:6px"></div>
    <div class="panel" style="margin-top:12px"><h4>报价单</h4><div id="sd-list">加载中…</div></div>
    <div class="panel" style="margin-top:12px"><h4>销售订单执行跟踪</h4><div id="sd-track">加载中…</div></div>`;

  const load = async () => {
    try {
      const r = await api(`/sales/quote?period=${period}`);
      $("#sd-list").innerHTML = table(r.rows || []);
      $all("[data-quo-approve]").forEach((b) => b.onclick = async () => {
        try { await api(`/sales/quote/${b.dataset.quoApprove}/approve`, { method: "POST" }); toast("已审批", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#sd-list").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
    try {
      const t = await api("/sales/track");
      $("#sd-track").innerHTML = soTrack(t.rows || []);
    } catch (e) { $("#sd-track").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#sd-save").addEventListener("click", async () => {
    try {
      await postJson("/sales/quote", { id: 0, period: ymm(state.current || ""), date: today(), customer_code: $("#sd-cust").value.trim(), customer_name: $("#sd-cust").value.trim(), item_code: $("#sd-item").value.trim(), item_name: $("#sd-item").value.trim(), qty: $("#sd-qty").value.trim() || "0", unit_price: $("#sd-price").value.trim() || "0", status: "draft", prepared_by: "", memo: "" });
      toast("已保存报价单", "ok"); $("#sd-qty").value=""; $("#sd-price").value=""; load();
    } catch (e) { toast(e.message, "err"); }
  });
  const soid = () => parseInt($("#sd-soid").value.trim(), 10) || 0;
  const amt = () => $("#sd-amt").value.trim();
  const memo = () => $("#sd-memo").value.trim();
  $("#sd-ship").addEventListener("click", async () => { if (!soid()) { toast("请填写销售订单ID", "err"); return; } try { await postJson("/sales/shipment", { so_id: soid(), period: ymm(state.current || ""), date: today(), qty: amt(), memo: memo() }); toast("已发货", "ok"); $("#sd-amt").value=""; $("#sd-memo").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#sd-return").addEventListener("click", async () => { if (!soid()) { toast("请填写销售订单ID", "err"); return; } try { await postJson("/sales/return", { so_id: soid(), period: ymm(state.current || ""), date: today(), qty: amt(), memo: memo() }); toast("已退货", "ok"); $("#sd-amt").value=""; $("#sd-memo").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#sd-pay").addEventListener("click", async () => { if (!soid()) { toast("请填写销售订单ID", "err"); return; } try { await postJson("/sales/payment", { so_id: soid(), period: ymm(state.current || ""), date: today(), amount: amt(), memo: memo() }); toast("已收款", "ok"); $("#sd-amt").value=""; $("#sd-memo").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#sd-credit").addEventListener("click", async () => {
    const c = $("#sd-credit-cust").value.trim();
    if (!c) { toast("请填写客户", "err"); return; }
    try {
      const r = await api(`/sales/credit?customer=${encodeURIComponent(c)}`);
      $("#sd-credit-result").innerHTML = r.over
        ? `<span class="tag err">超额度</span> 占用 ${esc(r.receivable)} / 额度 ${esc(r.limit)}`
        : `<span class="tag ok">未超额度</span> 占用 ${esc(r.receivable)} / 额度 ${esc(r.limit || "未设")}`;
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

async function viewOrderChangeLog(main) {
  main.innerHTML = `<h2>订单变更历史</h2>
    <div class="toolbar">
      <label>类型 <select id="ocl-type"><option value="po">采购订单</option><option value="so">销售订单</option></select></label>
      <label>订单ID <input id="ocl-id" style="width:90px" /></label>
      <button class="btn primary" id="ocl-load">查询</button>
    </div>
    <div id="ocl-result" class="muted">填写订单ID后查询</div>`;
  $("#ocl-load").addEventListener("click", async () => {
    const type = $("#ocl-type").value, id = $("#ocl-id").value.trim();
    if (!id) { toast("请填写订单ID", "err"); return; }
    try {
      const r = await api(`/order/change-log?type=${encodeURIComponent(type)}&id=${encodeURIComponent(id)}`);
      const rows = r.rows || [];
      $("#ocl-result").innerHTML = rows.length
        ? `<table><thead><tr><th>字段</th><th>旧值</th><th>新值</th><th>操作人</th><th>时间</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x[0])}</td><td>${esc(x[1])}</td><td>${esc(x[2])}</td><td>${esc(x[3])}</td><td>${esc(x[4])}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无变更记录</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 资金管理：资金日报 / 票据 / 融资 / 资金预测
// ===========================================================================
async function viewFunds(main) {
  main.innerHTML = `<h2>资金管理</h2>
    <div class="toolbar">
      <button class="btn sm ${state.fundsTab === "daily" ? "primary" : "ghost"}" id="ft-daily">资金日报</button>
      <button class="btn sm ${state.fundsTab === "bill" ? "primary" : "ghost"}" id="ft-bill">票据</button>
      <button class="btn sm ${state.fundsTab === "loan" ? "primary" : "ghost"}" id="ft-loan">融资</button>
      <button class="btn sm ${state.fundsTab === "forecast" ? "primary" : "ghost"}" id="ft-forecast">资金预测</button>
    </div>
    <div id="funds-body" class="muted">加载中…</div>`;
  const tab = state.fundsTab || "daily";
  const switchTab = (t) => { state.fundsTab = t; viewFunds(main); };
  $("#ft-daily").onclick = () => switchTab("daily");
  $("#ft-bill").onclick = () => switchTab("bill");
  $("#ft-loan").onclick = () => switchTab("loan");
  $("#ft-forecast").onclick = () => switchTab("forecast");

  const body = $("#funds-body");
  if (tab === "daily") {
    body.className = "";
    try {
      const r = await api("/funds/daily");
      const rows = r.rows || [];
      body.innerHTML = rows.length
        ? `<div class="muted" style="margin-bottom:8px">期间：${esc(r.period)}</div><table class="grid"><thead><tr>
            <th>科目</th><th>科目名称</th><th class="num">期初</th><th class="num">收入</th><th class="num">支出</th><th class="num">期末</th>
          </tr></thead><tbody>${rows.map((x) => `<tr>
            <td>${esc(x.account_code)}</td><td>${esc(x.account_name)}</td>
            <td class="num">${fmt(x.begin)}</td><td class="num">${fmt(x.income)}</td>
            <td class="num">${fmt(x.expense)}</td><td class="num"><b>${fmt(x.end)}</b></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">本期无现金/银行科目数据</div>`;
    } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  } else if (tab === "bill") {
    renderBills(body);
  } else if (tab === "loan") {
    renderLoans(body);
  } else {
    body.className = "";
    try {
      const r = await api("/funds/forecast");
      const f = r.forecast || {};
      body.innerHTML = `<div class="cards">
        ${["现金/银行结存", "在库应收票据", "应付票据", "放款可收回", "借款需偿还", "预计资金头寸"].map((t, i) => {
          const k = ["cash_balance", "receivable_bills", "payable_bills", "lend", "borrow", "position"][i];
          const v = f[k] || "0";
          return `<div class="card"><div class="k">${t}</div><div class="v">${fmt(v)}</div></div>`;
        }).join("")}
      </div>`;
    } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function renderBills(body) {
  body.className = "";
  const toolbar = `<div class="toolbar">
      <button class="btn sm" id="bill-new">新增票据</button>
      <label>类型 <select id="bill-kind"><option value="">全部</option><option value="receivable">应收</option><option value="payable">应付</option></select></label>
    </div><div id="bill-list" class="muted">加载中…</div>`;
  body.innerHTML = toolbar;
  $("#bill-new").onclick = () => openBillEditor(null);
  $("#bill-kind").onchange = loadBills;
  await loadBills();
  async function loadBills() {
    const kind = $("#bill-kind").value;
    try {
      const r = await api(`/funds/bills?kind=${encodeURIComponent(kind)}`);
      const rows = r.rows || [];
      const stMap = { in_hand: ["在库", "ok"], endorsed: ["已背书", "warn"], discounted: ["已贴现", "warn"], matured: ["已到期", "err"], settled: ["已兑付", "ok"] };
      $("#bill-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>票据号</th><th>出票日</th><th>到期日</th><th>对方单位</th><th>承兑银行</th><th class="num">金额</th><th>状态</th><th></th></tr></thead>
          <tbody>${rows.map((b) => {
            const s = stMap[b.status] || [b.status, ""];
            return `<tr><td>${b.kind === "receivable" ? "应收" : "应付"}</td><td>${esc(b.no)}</td>
              <td>${esc(b.issue_date)}</td><td>${esc(b.due_date)}</td><td>${esc(b.counterpart || "—")}</td>
              <td>${esc(b.bank || "—")}</td><td class="num">${fmt(b.amount)}</td>
              <td><span class="tag ${s[1]}">${esc(s[0])}</span></td>
              <td class="row-actions">
                <button class="btn ghost sm" data-bill="${b.id}">打开</button>
                ${b.status === "in_hand" ? `<button class="btn ghost sm" data-bill-act="${b.id}" data-to="endorsed">背书</button>
                <button class="btn ghost sm" data-bill-act="${b.id}" data-to="discounted">贴现</button>
                <button class="btn ghost sm" data-bill-act="${b.id}" data-to="settled">兑付</button>` : ""}
              </td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无票据</div>`;
      $all("[data-bill]").forEach((b) => b.onclick = () => openBillEditor(parseInt(b.dataset.bill, 10)));
      $all("[data-bill-act]").forEach((b) => b.onclick = async () => {
        try { await api(`/funds/bills/${b.dataset.billAct}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: b.dataset.to, date: today() }) }); toast("已更新", "ok"); loadBills(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#bill-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function openBillEditor(id) {
  let b = { id: 0, kind: "receivable", no: "", issue_date: today(), due_date: today(), counterpart: "", bank: "", amount: "0", status: "in_hand", memo: "" };
  if (id) { try { const r = await api("/funds/bills"); b = (r.rows || []).find((x) => x.id === id) || b; } catch (e) { toast(e.message, "err"); return; } }
  const mask = modal(`
    <h3>${id ? "编辑票据" : "新增票据"}</h3>
    <div class="field"><label>类型</label><select id="b-kind">
      <option value="receivable" ${b.kind === "receivable" ? "selected" : ""}>应收票据</option>
      <option value="payable" ${b.kind === "payable" ? "selected" : ""}>应付票据</option></select></div>
    <div class="field"><label>票据号</label><input id="b-no" value="${esc(b.no)}" /></div>
    <div class="field"><label>出票日</label><input id="b-issue" type="date" value="${esc(b.issue_date)}" /></div>
    <div class="field"><label>到期日</label><input id="b-due" type="date" value="${esc(b.due_date)}" /></div>
    <div class="field"><label>对方单位</label><input id="b-cp" value="${esc(b.counterpart)}" /></div>
    <div class="field"><label>承兑银行</label><input id="b-bank" value="${esc(b.bank)}" /></div>
    <div class="field"><label>金额</label><input id="b-amt" value="${esc(b.amount)}" /></div>
    <div class="field"><label>备注</label><input id="b-memo" value="${esc(b.memo)}" /></div>
    <div class="foot"><button class="btn" id="b-save">保存</button>${id ? `<button class="btn danger ghost" id="b-del">删除</button>` : ""}<button class="btn ghost" id="b-cancel">取消</button></div>`);
  $("#b-cancel", mask).onclick = closeModal;
  $("#b-save", mask).onclick = async () => {
    const body2 = {
      id: b.id, kind: $("#b-kind", mask).value, no: $("#b-no", mask).value.trim(),
      period: ymm(state.current || ""), issue_date: $("#b-issue", mask).value,
      due_date: $("#b-due", mask).value, counterpart: $("#b-cp", mask).value.trim(),
      bank: $("#b-bank", mask).value.trim(), amount: $("#b-amt", mask).value.trim(),
      status: "in_hand", memo: $("#b-memo", mask).value.trim(),
    };
    if (!body2.no || !body2.issue_date || !body2.due_date) { toast("票据号与日期必填", "err"); return; }
    try { await api("/funds/bills", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body2) }); toast("已保存", "ok"); closeModal(); renderBills($("#funds-body")); } catch (e) { toast(e.message, "err"); }
  };
  if (id) $("#b-del", mask).onclick = async () => { if (!(await confirmDialog("删除该票据？", true))) return; try { await api(`/funds/bills/${id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); renderBills($("#funds-body")); } catch (e) { toast(e.message, "err"); } };
}

async function renderLoans(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar"><button class="btn sm" id="loan-new">新增融资</button>
      <label>类型 <select id="loan-kind"><option value="">全部</option><option value="borrow">借款</option><option value="lend">放款</option></select></label>
    </div><div id="loan-list" class="muted">加载中…</div>`;
  $("#loan-new").onclick = () => openLoanEditor(null);
  $("#loan-kind").onchange = loadLoans;
  await loadLoans();
  async function loadLoans() {
    const kind = $("#loan-kind").value;
    try {
      const r = await api(`/funds/loans?kind=${encodeURIComponent(kind)}`);
      const rows = r.rows || [];
      $("#loan-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>编号</th><th>机构</th><th class="num">本金</th><th class="num">年利率%</th><th>起息日</th><th>到期日</th><th>状态</th><th></th></tr></thead>
          <tbody>${rows.map((l) => `<tr>
            <td>${l.kind === "borrow" ? "借款" : "放款"}</td><td>${esc(l.no)}</td><td>${esc(l.bank || "—")}</td>
            <td class="num">${fmt(l.principal)}</td><td class="num">${fmt(l.rate_pct)}</td>
            <td>${esc(l.start_date)}</td><td>${esc(l.end_date)}</td>
            <td><span class="tag ${l.status === "active" ? "warn" : "ok"}">${l.status === "active" ? "存续" : "已结清"}</span></td>
            <td class="row-actions">
              <button class="btn ghost sm" data-loan="${l.id}">打开</button>
              ${l.status === "active" ? `<button class="btn ghost sm" data-loan-settle="${l.id}">结清</button>` : ""}
            </td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无融资记录</div>`;
      $all("[data-loan]").forEach((b) => b.onclick = () => openLoanEditor(parseInt(b.dataset.loan, 10)));
      $all("[data-loan-settle]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("结清该笔融资？", true))) return;
        try { await api(`/funds/loans/${b.dataset.loanSettle}/settle`, { method: "POST" }); toast("已结清", "ok"); loadLoans(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#loan-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function openLoanEditor(id) {
  let l = { id: 0, kind: "borrow", no: "", bank: "", principal: "0", rate_pct: "0", start_date: today(), end_date: today(), status: "active", memo: "" };
  if (id) { try { const r = await api("/funds/loans"); l = (r.rows || []).find((x) => x.id === id) || l; } catch (e) { toast(e.message, "err"); return; } }
  const mask = modal(`
    <h3>${id ? "编辑融资" : "新增融资"}</h3>
    <div class="field"><label>类型</label><select id="l-kind">
      <option value="borrow" ${l.kind === "borrow" ? "selected" : ""}>借款</option>
      <option value="lend" ${l.kind === "lend" ? "selected" : ""}>放款</option></select></div>
    <div class="field"><label>编号</label><input id="l-no" value="${esc(l.no)}" /></div>
    <div class="field"><label>机构</label><input id="l-bank" value="${esc(l.bank)}" /></div>
    <div class="field"><label>本金</label><input id="l-pr" value="${esc(l.principal)}" /></div>
    <div class="field"><label>年利率(%)</label><input id="l-rate" value="${esc(l.rate_pct)}" /></div>
    <div class="field"><label>起息日</label><input id="l-start" type="date" value="${esc(l.start_date)}" /></div>
    <div class="field"><label>到期日</label><input id="l-end" type="date" value="${esc(l.end_date)}" /></div>
    <div class="field"><label>备注</label><input id="l-memo" value="${esc(l.memo)}" /></div>
    <div class="foot"><button class="btn" id="l-save">保存</button>${id ? `<button class="btn danger ghost" id="l-del">删除</button>` : ""}<button class="btn ghost" id="l-cancel">取消</button></div>`);
  $("#l-cancel", mask).onclick = closeModal;
  $("#l-save", mask).onclick = async () => {
    const body2 = {
      id: l.id, kind: $("#l-kind", mask).value, no: $("#l-no", mask).value.trim(),
      bank: $("#l-bank", mask).value.trim(), principal: $("#l-pr", mask).value.trim(),
      rate_pct: $("#l-rate", mask).value.trim(), start_date: $("#l-start", mask).value,
      end_date: $("#l-end", mask).value, status: "active", memo: $("#l-memo", mask).value.trim(),
    };
    if (!body2.no || !body2.start_date || !body2.end_date) { toast("编号与日期必填", "err"); return; }
    try { await api("/funds/loans", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body2) }); toast("已保存", "ok"); closeModal(); renderLoans($("#funds-body")); } catch (e) { toast(e.message, "err"); }
  };
  if (id) $("#l-del", mask).onclick = async () => { if (!(await confirmDialog("删除该笔融资？", true))) return; try { await api(`/funds/loans/${id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); renderLoans($("#funds-body")); } catch (e) { toast(e.message, "err"); } };
}

// ===========================================================================
// 预算分析：年度逐月预算 vs 实际（含部门维度）
// ===========================================================================
async function viewBudgetAnalysis(main) {
  main.innerHTML = `<h2>预算分析</h2>
    <div class="toolbar">
      <label>年度 <input id="ana-year" value="${new Date().getFullYear()}" style="width:70px" /></label>
      <label>版本 <input id="ana-ver" value="" placeholder="留空=当前" style="width:110px" /></label>
      <button class="btn primary" id="ana-run">查询</button>
      <button class="btn ghost sm" id="ana-run-print">打印预览</button>
      <button class="btn ghost sm" id="ana-sum">仅汇总</button>
    </div>
    <div id="ana-body" class="muted">选择年度后查询</div>`;
  const run = async (summaryOnly) => {
    const year = $("#ana-year").value.trim() || String(new Date().getFullYear());
    const ver = $("#ana-ver").value.trim();
    try {
      const r = await api(`/budget/analysis?year=${encodeURIComponent(year)}&version=${encodeURIComponent(ver)}`);
      const rows = r.rows || [], sums = r.summary || [];
      if (summaryOnly || !rows.length) {
        $("#ana-body").innerHTML = sums.length
          ? `<div class="muted" style="margin-bottom:8px">年度汇总（科目 × 部门）</div><table class="grid"><thead><tr>
              <th>科目</th><th>部门</th><th class="num">预算</th><th class="num">实际</th><th class="num">执行率</th></tr></thead>
            <tbody>${sums.map((x) => `<tr><td>${esc(x.account_code)} ${esc(x.account_name)}</td><td>${esc(x.dept || "—")}</td>
              <td class="num">${fmt(x.budget)}</td><td class="num">${fmt(x.actual)}</td><td class="num">${x.rate}%</td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">该年度无预算数据</div>`;
      } else {
        // 按月透视：行=科目×部门，列=月份
        const months = Array.from(new Set(rows.map((x) => x.period))).sort();
        const groups = {};
        rows.forEach((x) => { const k = `${x.account_code}|${x.account_name}|${x.dept}`; (groups[k] = groups[k] || []).push(x); });
        $("#ana-body").innerHTML = `<div class="muted" style="margin-bottom:8px">逐月预算 vs 实际（预算/实际/执行率%）</div><div class="panel" style="overflow-x:auto"><table class="grid"><thead><tr>
            <th>科目 / 部门</th>${months.map((mo) => `<th colspan="3" class="num">${esc(String(mo).slice(4, 6))}月</th>`).join("")}</tr>
          <tr><th></th>${months.map(() => `<th class="num">预算</th><th class="num">实际</th><th class="num">率</th>`).join("")}</tr></thead>
          <tbody>${Object.entries(groups).map(([k, items]) => {
            const m = {};
            items.forEach((i) => m[i.period] = i);
            return `<tr><td>${esc(k.split("|").slice(0, 2).join(" "))}${k.split("|")[2] ? `<div class="muted" style="font-size:11px">${esc(k.split("|")[2])}</div>` : ""}</td>
              ${months.map((mo) => { const i = m[mo]; return i ? `<td class="num">${fmt(i.budget)}</td><td class="num">${fmt(i.actual)}</td><td class="num">${i.rate}%</td>` : `<td class="num">—</td><td class="num">—</td><td class="num">—</td>`; }).join("")}</tr>`;
          }).join("")}</tbody></table></div>`;
      }
    } catch (e) { $("#ana-body").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#ana-run").addEventListener("click", () => run(false));
  $("#ana-sum").addEventListener("click", () => run(true));
  $("#ana-run-print").addEventListener("click", () => { const el = $("#ana-body").querySelector("table"); printPreview("预算分析", el); });
}

// ===========================================================================
// 成本核算：计价方式配置 + 期末结价
// ===========================================================================
async function viewCost(main) {
  main.innerHTML = `<h2>成本核算</h2>
    <div class="toolbar">
      <button class="btn sm ${state.costTab === "config" ? "primary" : "ghost"}" id="ct-config">计价方式</button>
      <button class="btn sm ${state.costTab === "close" ? "primary" : "ghost"}" id="ct-close">期末结价</button>
    </div>
    <div id="cost-body" class="muted">加载中…</div>`;
  const tab = state.costTab || "config";
  $("#ct-config").onclick = () => { state.costTab = "config"; viewCost(main); };
  $("#ct-close").onclick = () => { state.costTab = "close"; viewCost(main); };
  const body = $("#cost-body");
  if (tab === "config") {
    body.className = "";
    body.innerHTML = `<div class="toolbar"><button class="btn sm" id="cost-new">新增配置</button></div><div id="cost-list" class="muted">加载中…</div>`;
    $("#cost-new").onclick = () => openCostConfig(null);
    await loadCostConfigs();
    async function loadCostConfigs() {
      try {
        const r = await api("/cost/configs");
        const rows = r.rows || [];
        $("#cost-list").innerHTML = rows.length
          ? `<table class="grid"><thead><tr><th>存货</th><th>计价方式</th><th class="num">标准成本</th><th></th></tr></thead>
            <tbody>${rows.map((x) => `<tr><td>${esc(x.item)}</td><td>${esc(x.method_label)}</td>
              <td class="num">${fmt(x.standard_cost)}</td>
              <td class="row-actions"><button class="btn ghost sm" data-cfg="${esc(x.item)}">编辑</button>
              <button class="btn ghost sm" data-cfg-del="${esc(x.item)}">清除</button></td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">尚未配置存货计价方式（默认移动加权平均）</div>`;
        $all("[data-cfg]").forEach((b) => b.onclick = () => openCostConfig(b.dataset.cfg));
        $all("[data-cfg-del]").forEach((b) => b.onclick = async () => {
          if (!(await confirmDialog(`清除 ${b.dataset.cfgDel} 的计价配置（恢复默认）？`, true))) return;
          try { await api(`/cost/configs/${encodeURIComponent(b.dataset.cfgDel)}/delete`, { method: "POST" }); toast("已清除", "ok"); loadCostConfigs(); } catch (e) { toast(e.message, "err"); }
        });
      } catch (e) { $("#cost-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    }
  } else {
    body.className = "";
    body.innerHTML = `<div class="toolbar">
        <label>期间 <input id="ce-period" value="${esc(state.current)}" style="width:90px" /></label>
        <button class="btn" id="ce-preview">试算（不落账）</button>
        <button class="btn primary" id="ce-apply">期末结价（写入调整）</button>
      </div><div id="ce-list" class="muted">选择期间后试算</div>`;
    const run = async (apply) => {
      const p = $("#ce-period").value.trim();
      try {
        const r = await api(`/cost/period-end?period=${encodeURIComponent(p)}&apply=${apply}`, apply ? { method: "POST" } : {});
        const rows = r.rows || [];
        const sumAdj = rows.reduce((a, x) => a + moneyNum(x.adjust), 0);
        $("#ce-list").innerHTML = rows.length
          ? `<div class="muted" style="margin-bottom:8px">期间 ${esc(r.period)}${apply ? "（已写入成本调整）" : "（试算）"} · 调整合计 ${moneyFmt(sumAdj)}</div>
            <table class="grid"><thead><tr><th>存货</th><th>计价方式</th><th class="num">结存数量</th><th class="num">结存金额</th><th class="num">单价</th><th class="num">调整额</th></tr></thead>
            <tbody>${rows.map((x) => `<tr><td>${esc(x.item)}</td><td>${esc(x.method)}</td>
              <td class="num">${fmt(x.end_qty)}</td><td class="num">${fmt(x.end_amount)}</td>
              <td class="num">${fmt(x.unit_cost)}</td>
              <td class="num" style="color:${moneyNum(x.adjust) < 0 ? "var(--err)" : "var(--ok)"}">${fmt(x.adjust)}</td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">该期间无存货流水</div>`;
      } catch (e) { $("#ce-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    };
    $("#ce-preview").addEventListener("click", () => run(false));
    $("#ce-apply").addEventListener("click", async () => { if (!(await confirmDialog("期末结价会写入成本调整流水（不影响数量），确认执行？", true))) return; run(true); });
  }
}

async function openCostConfig(item) {
  const mask = modal(`
    <h3>计价方式配置</h3>
    <div class="field"><label>存货编码</label><input id="c-item" value="${esc(item || "")}" ${item ? "disabled" : ""} /></div>
    <div class="field"><label>计价方式</label><select id="c-method">
      <option value="moving_average">移动加权平均</option>
      <option value="month_average">全月一次加权平均</option>
      <option value="fifo">先进先出</option>
      <option value="specific">个别计价</option>
      <option value="standard">标准成本</option></select></div>
    <div class="field"><label>标准成本单价</label><input id="c-std" value="0" /></div>
    <div class="foot"><button class="btn" id="c-save">保存</button><button class="btn ghost" id="c-cancel">取消</button></div>`);
  if (item) {
    try {
      const r = await api("/cost/configs");
      const cfg = (r.rows || []).find((x) => x.item === item);
      if (cfg) { $("#c-method", mask).value = cfg.method; $("#c-std", mask).value = cfg.standard_cost; }
    } catch (e) {}
  }
  $("#c-cancel", mask).onclick = closeModal;
  $("#c-save", mask).onclick = async () => {
    const it = item || $("#c-item", mask).value.trim();
    if (!it) { toast("存货编码必填", "err"); return; }
    try {
      await api("/cost/configs", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ item: it, method: $("#c-method", mask).value, standard_cost: $("#c-std", mask).value.trim() || "0" }) });
      toast("已保存", "ok"); closeModal(); viewCost($("#main"));
    } catch (e) { toast(e.message, "err"); }
  };
}

// ===========================================================================
// 基础资料与系统功能（对齐桌面端 finui：科目 / 期初 / 档案 / 参数 / 日志 / 备份 / 模板）
// ===========================================================================

// 辅助核算维度（bit 与后端 AuxKind::bit() 对齐，仅用于把掩码渲染成标签）
const AUX_KINDS = [
  { code: "customer", label: "客户", bit: 1 },
  { code: "supplier", label: "供应商", bit: 2 },
  { code: "dept", label: "部门", bit: 4 },
  { code: "employee", label: "职员", bit: 8 },
  { code: "project", label: "项目", bit: 16 },
  { code: "item", label: "存货", bit: 32 },
  { code: "cashflow", label: "现金流量", bit: 64 },
  { code: "bank", label: "银行账户", bit: 128 },
];
const ACCT_CATEGORIES = [
  { code: "asset", label: "资产" }, { code: "liability", label: "负债" }, { code: "common", label: "共同" },
  { code: "equity", label: "权益" }, { code: "cost", label: "成本" }, { code: "income", label: "收入" }, { code: "expense", label: "费用" },
];
const FREQ_LABELS = { manual: "手工调用", monthly: "每月生成", quarterly: "每季生成", yearly: "每年生成" };

function auxMaskLabel(mask) {
  return AUX_KINDS.filter((k) => (mask & k.bit) !== 0).map((k) => k.label).join("、") || "—";
}
function acctCatLabel(code) {
  const c = ACCT_CATEGORIES.find((x) => x.code === code);
  return c ? c.label : code;
}
function dirLabel(code) { return code === "credit" ? "贷" : "借"; }

// ---------------- 会计科目 ----------------
async function viewAccounts(main) {
  main.innerHTML = `<h2>会计科目</h2><div class="muted">加载中…</div>`;
  let rows;
  try { rows = await api("/accounts"); } catch (e) { main.innerHTML = `<h2>会计科目</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  state.accounts = rows;
  renderAccounts(main, rows);
}

function renderAccounts(main, rows) {
  const kw = (state.acctKw || "").trim();
  const shown = kw ? rows.filter((a) => a.code.includes(kw) || a.name.includes(kw)) : rows;
  main.innerHTML = `
    <h2>会计科目</h2>
    <div class="toolbar">
      <input id="acct-kw" placeholder="编码 / 名称" value="${esc(kw)}" style="width:160px" />
      <button class="btn" id="acct-search">查询</button>
      <div class="spacer"></div>
      ${session.platformAdmin ? `<button class="btn ghost" id="acct-fill">填充默认科目</button>` : ""}
      ${can("account_edit") ? `<button class="btn primary" id="acct-new">新增科目</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:auto;max-height:70vh">
      <table class="grid">
        <thead><tr><th>编码</th><th>名称</th><th>类别</th><th>方向</th><th>辅助核算</th><th>数量</th><th>币种</th><th>标志</th><th>状态</th><th>备注</th><th></th></tr></thead>
        <tbody>
          ${shown.length ? shown.map((a) => `
            <tr>
              <td>${esc(a.code)}</td>
              <td>${esc(a.name)}</td>
              <td>${esc(acctCatLabel(a.category))}</td>
              <td>${esc(dirLabel(a.dir))}</td>
              <td>${esc(auxMaskLabel(a.aux))}</td>
              <td>${esc(a.unit || "—")}</td>
              <td>${esc(a.currency || "—")}</td>
              <td>${a.is_cash ? "现金 " : ""}${a.is_bank ? "银行" : ""}${!a.is_cash && !a.is_bank ? "—" : ""}</td>
              <td>${a.disabled ? `<span class="tag err">停用</span>` : `<span class="tag ok">启用</span>`}</td>
              <td>${esc(a.memo)}</td>
              <td class="row-actions">
                ${can("account_edit") ? `<button class="btn sm ghost" data-act="edit" data-code="${esc(a.code)}">编辑</button>` : ""}
                ${can("account_edit") ? `<button class="btn sm ghost" data-act="del" data-code="${esc(a.code)}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="11" class="muted" style="text-align:center;padding:18px">无科目${kw ? "（无匹配）" : ""}，可点「填充默认科目」</td></tr>`}
        </tbody>
      </table>
    </div>`;
  $("#acct-search").addEventListener("click", () => { state.acctKw = $("#acct-kw").value; renderAccounts(main, rows); });
  $("#acct-kw").addEventListener("keydown", (e) => { if (e.key === "Enter") { state.acctKw = $("#acct-kw").value; renderAccounts(main, rows); } });
  const fill = $("#acct-fill");
  if (fill) fill.addEventListener("click", async () => {
    if (!(await confirmDialog("按编码推断补充系统内置科目表（已有的科目不动），继续？"))) return;
    try { const r = await api("/accounts/fill-defaults", { method: "POST" }); toast(`已补充 ${r.added || 0} 个科目`, "ok"); viewAccounts(main); }
    catch (e) { toast(e.message, "err"); }
  });
  if ($("#acct-new")) $("#acct-new").addEventListener("click", () => openAccountEditor(main, null));
  $all("[data-act]", main).forEach((b) => b.addEventListener("click", async () => {
    const code = b.dataset.code;
    if (b.dataset.act === "edit") {
      const acc = rows.find((x) => x.code === code);
      if (acc) openAccountEditor(main, acc);
    } else if (b.dataset.act === "del") {
      if (!(await confirmDialog(`确定删除科目 ${code}？已被凭证使用的科目无法删除。`, true))) return;
      try { await api(`/accounts/${encodeURIComponent(code)}`, { method: "DELETE" }); toast("已删除", "ok"); viewAccounts(main); }
      catch (e) { toast(e.message, "err"); }
    }
  }));
}

function openAccountEditor(main, acc) {
  const isEdit = !!acc;
  const a = acc || { code: "", name: "", category: "asset", dir: "debit", aux: 0, unit: null, currency: null, has_qty: false, is_cash: false, is_bank: false, cash_flow_item: null, bs_item: null, pl_item: null, disabled: false, memo: "" };
  const mask = modal(`
    <h3>${isEdit ? "编辑科目" : "新增科目"}</h3>
    <div class="field"><label>科目编码 *</label><input id="ac-code" value="${esc(a.code)}" ${isEdit ? "readonly" : ""} placeholder="1001" /></div>
    <div class="field"><label>科目名称 *</label><input id="ac-name" value="${esc(a.name)}" /></div>
    <div class="field"><label>类别</label>
      <select id="ac-cat">${ACCT_CATEGORIES.map((c) => `<option value="${c.code}" ${a.category === c.code ? "selected" : ""}>${c.label}</option>`).join("")}</select>
    </div>
    <div class="field"><label>余额方向</label>
      <select id="ac-dir"><option value="debit" ${a.dir !== "credit" ? "selected" : ""}>借</option><option value="credit" ${a.dir === "credit" ? "selected" : ""}>贷</option></select>
    </div>
    <div class="field"><label>辅助核算维度</label>
      <div style="display:flex;flex-wrap:wrap;gap:8px 16px">
        ${AUX_KINDS.map((k) => `<label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" class="ac-aux" value="${k.code}" ${(a.aux & k.bit) ? "checked" : ""} />${k.label}</label>`).join("")}
      </div>
    </div>
    <div class="field"><label>数量单位（留空不核算数量）</label><input id="ac-unit" value="${esc(a.unit || "")}" placeholder="件 / 吨" /></div>
    <div class="field"><label>外币币种（留空只核算人民币）</label><input id="ac-cur" value="${esc(a.currency || "")}" placeholder="USD" /></div>
    <div class="field"><label>备注</label><input id="ac-memo" value="${esc(a.memo)}" /></div>
    <div class="field" style="display:flex;gap:20px">
      <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="ac-cash" ${a.is_cash ? "checked" : ""} />现金科目</label>
      <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="ac-bank" ${a.is_bank ? "checked" : ""} />银行科目</label>
      <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="ac-disabled" ${a.disabled ? "checked" : ""} />停用</label>
    </div>
    <div class="foot">
      <button class="btn ghost" id="ac-cancel">取消</button>
      <button class="btn primary" id="ac-save">保存</button>
    </div>
  `);
  $("#ac-cancel", mask).addEventListener("click", closeModal);
  $("#ac-save", mask).addEventListener("click", async () => {
    const code = $("#ac-code", mask).value.trim();
    const name = $("#ac-name", mask).value.trim();
    if (!code || !name) { toast("编码与名称必填", "err"); return; }
    const auxKinds = $all(".ac-aux", mask).filter((c) => c.checked).map((c) => c.value);
    const unit = $("#ac-unit", mask).value.trim();
    const cur = $("#ac-cur", mask).value.trim();
    const body = {
      account: Object.assign({}, a, {
        code, name,
        category: $("#ac-cat", mask).value,
        dir: $("#ac-dir", mask).value,
        unit: unit || null,
        currency: cur || null,
        has_qty: !!unit,
        is_cash: $("#ac-cash", mask).checked,
        is_bank: $("#ac-bank", mask).checked,
        disabled: $("#ac-disabled", mask).checked,
        memo: $("#ac-memo", mask).value.trim(),
      }),
      aux_kinds: auxKinds,
    };
    try {
      if (isEdit) await api("/accounts", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      else await api("/accounts", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("已保存", "ok"); closeModal(); viewAccounts(main);
    } catch (e) { toast(e.message, "err"); }
  });
}

// ---------------- 期初建账 ----------------
async function viewBegin(main) {
  main.innerHTML = `<h2>期初建账</h2><div class="muted">加载中…</div>`;
  let rows, accounts;
  try {
    [rows, accounts] = await Promise.all([api("/begin"), api("/accounts")]);
  } catch (e) { main.innerHTML = `<h2>期初建账</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  state.accounts = accounts;
  renderBegin(main, rows);
}

// rows：BeginRow 列表；year_begin 带符号（借正贷负），前端拆成 方向 + 金额 文本框
function renderBegin(main, rows) {
  if (!state.beginDraft) state.beginDraft = rows.map((r) => ({
    id: r.id, account_code: r.account_code,
    dir: String(r.year_begin).trim().startsWith("-") ? "credit" : "debit",
    yb: fmt(String(Math.abs(parseFloat(r.year_begin) || 0))),
    ad: fmt(r.debit_accum), ac: fmt(r.credit_accum), qty: r.qty_begin == null ? "" : fmt(r.qty_begin),
  }));
  const draft = state.beginDraft;
  const num = (s) => parseFloat(String(s).replace(/,/g, "")) || 0;
  const render = () => {
    const nameOf = (code) => { const a = (state.accounts || []).find((x) => x.code === code); return a ? a.name : ""; };
    const sumYbDebit = draft.filter((r) => r.dir === "debit").reduce((s, r) => s + num(r.yb), 0);
    const sumYbCredit = draft.filter((r) => r.dir === "credit").reduce((s, r) => s + num(r.yb), 0);
    const sumAd = draft.reduce((s, r) => s + num(r.ad), 0);
    const sumAc = draft.reduce((s, r) => s + num(r.ac), 0);
    const balanced = Math.abs((sumYbDebit + sumAd) - (sumYbCredit + sumAc)) < 0.005;
    main.innerHTML = `
      <h2>期初建账</h2>
      <div class="toolbar">
        <button class="btn" id="bg-add">添加科目行</button>
        <div class="spacer"></div>
        ${can("opening") ? `<button class="btn primary" id="bg-save">保存全部</button>` : ""}
      </div>
      <div class="panel" style="padding:0;overflow:auto;max-height:60vh">
        <table class="grid">
          <thead><tr><th>科目编码</th><th>科目名称</th><th>方向</th><th class="num">年初余额</th><th class="num">借方累计</th><th class="num">贷方累计</th><th class="num">数量</th><th></th></tr></thead>
          <tbody>
            ${draft.length ? draft.map((r, i) => `
              <tr>
                <td><input class="bg-code" data-i="${i}" value="${esc(r.account_code)}" style="width:110px" ${r.id > 0 ? "readonly" : ""} /></td>
                <td class="muted">${esc(nameOf(r.account_code))}</td>
                <td><select class="bg-dir" data-i="${i}"><option value="debit" ${r.dir !== "credit" ? "selected" : ""}>借</option><option value="credit" ${r.dir === "credit" ? "selected" : ""}>贷</option></select></td>
                <td class="num"><input class="bg-yb num" data-i="${i}" value="${esc(r.yb)}" style="width:120px;text-align:right" /></td>
                <td class="num"><input class="bg-ad num" data-i="${i}" value="${esc(r.ad)}" style="width:120px;text-align:right" /></td>
                <td class="num"><input class="bg-ac num" data-i="${i}" value="${esc(r.ac)}" style="width:120px;text-align:right" /></td>
                <td class="num"><input class="bg-qty num" data-i="${i}" value="${esc(r.qty)}" style="width:90px;text-align:right" /></td>
                <td>${r.id > 0 ? `<span class="muted" style="font-size:12px">已有</span>` : `<button class="btn ghost sm" data-rm="${i}">移除</button>`}</td>
              </tr>`).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:18px">暂无期初数据，点「添加科目行」开始建账</td></tr>`}
          </tbody>
        </table>
      </div>
      <div class="cards" style="margin-top:14px">
        <div class="card"><div class="k">年初借方合计</div><div class="v" style="font-size:16px">${fmt(sumYbDebit.toFixed(2))}</div></div>
        <div class="card"><div class="k">年初贷方合计</div><div class="v" style="font-size:16px">${fmt(sumYbCredit.toFixed(2))}</div></div>
        <div class="card"><div class="k">借方累计合计</div><div class="v" style="font-size:16px">${fmt(sumAd.toFixed(2))}</div></div>
        <div class="card"><div class="k">贷方累计合计</div><div class="v" style="font-size:16px">${fmt(sumAc.toFixed(2))}</div></div>
        <div class="card"><div class="k">试算平衡</div><div class="v" style="font-size:16px;color:${balanced ? "var(--ok)" : "var(--err)"}">${balanced ? "✓ 平衡" : "✗ 不平衡"}</div></div>
      </div>`;
    $all(".bg-code", main).forEach((inp) => inp.oninput = () => draft[+inp.dataset.i].account_code = inp.value.trim());
    $all(".bg-dir", main).forEach((sel) => sel.onchange = () => draft[+sel.dataset.i].dir = sel.value);
    $all(".bg-yb", main).forEach((inp) => inp.oninput = () => draft[+inp.dataset.i].yb = inp.value);
    $all(".bg-ad", main).forEach((inp) => inp.oninput = () => draft[+inp.dataset.i].ad = inp.value);
    $all(".bg-ac", main).forEach((inp) => inp.oninput = () => draft[+inp.dataset.i].ac = inp.value);
    $all(".bg-qty", main).forEach((inp) => inp.oninput = () => draft[+inp.dataset.i].qty = inp.value);
    $all("[data-rm]", main).forEach((b) => b.onclick = () => { draft.splice(+b.dataset.rm, 1); render(); });
    $("#bg-add").onclick = () => { draft.push({ id: 0, account_code: "", dir: "debit", yb: "", ad: "", ac: "", qty: "" }); render(); };
    $("#bg-save").onclick = async () => {
      const payload = draft
        .filter((r) => r.account_code)
        .map((r) => ({ account_code: r.account_code, dir: r.dir, yb: r.yb.replace(/,/g, ""), ad: r.ad.replace(/,/g, ""), ac: r.ac.replace(/,/g, ""), qty: r.qty ? r.qty.replace(/,/g, "") : null }));
      try {
        const r = await api("/begin", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
        toast(`已保存 ${r.count || payload.length} 条期初`, "ok");
        state.beginDraft = null;
        viewBegin(main);
      } catch (e) { toast(e.message, "err"); }
    };
  };
  render();
}

// ---------------- 辅助核算档案 ----------------
async function viewAux(main) {
  const kind = state.auxKind || "customer";
  main.innerHTML = `<h2>辅助核算档案</h2><div class="muted">加载中…</div>`;
  let rows;
  try { rows = await api(`/aux?kind=${encodeURIComponent(kind)}`); }
  catch (e) { main.innerHTML = `<h2>辅助核算档案</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  renderAux(main, rows, kind);
}

function renderAux(main, rows, kind) {
  main.innerHTML = `
    <h2>辅助核算档案</h2>
    <div class="toolbar">
      ${AUX_KINDS.map((k) => `<button class="btn sm ${kind === k.code ? "primary" : "ghost"}" data-kind="${k.code}">${k.label}</button>`).join("")}
      <div class="spacer"></div>
      ${can("aux_edit") ? `<button class="btn primary" id="aux-new">新增${esc(AUX_KINDS.find((k) => k.code === kind).label)}</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:auto;max-height:65vh">
      <table class="grid">
        <thead><tr><th>编码</th><th>名称</th><th>上级编码</th><th>状态</th><th>备注</th><th></th></tr></thead>
        <tbody>
          ${rows.length ? rows.map((e) => `
            <tr>
              <td>${esc(e.code)}</td><td>${esc(e.name)}</td><td>${esc(e.parent_code || "—")}</td>
              <td>${e.disabled ? `<span class="tag err">停用</span>` : `<span class="tag ok">启用</span>`}</td>
              <td>${esc(e.memo)}</td>
              <td class="row-actions">
                ${can("aux_edit") ? `<button class="btn sm ghost" data-act="edit" data-id="${e.id}">编辑</button>` : ""}
                ${can("aux_edit") ? `<button class="btn sm ghost" data-act="del" data-id="${e.id}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="6" class="muted" style="text-align:center;padding:18px">暂无档案</td></tr>`}
        </tbody>
      </table>
    </div>`;
  $all("[data-kind]", main).forEach((b) => b.addEventListener("click", () => { state.auxKind = b.dataset.kind; viewAux(main); }));
  if ($("#aux-new")) $("#aux-new").addEventListener("click", () => openAuxEditor(main, null, kind));
  $all("[data-act]", main).forEach((b) => b.addEventListener("click", async () => {
    const id = parseInt(b.dataset.id, 10);
    if (b.dataset.act === "edit") {
      const e = rows.find((x) => x.id === id);
      if (e) openAuxEditor(main, e, kind);
    } else {
      if (!(await confirmDialog("确定删除该档案？", true))) return;
      try { await api(`/aux/${id}`, { method: "DELETE" }); toast("已删除", "ok"); viewAux(main); }
      catch (e2) { toast(e2.message, "err"); }
    }
  }));
}

function openAuxEditor(main, ent, kind) {
  const isEdit = !!ent;
  const e = ent || { id: 0, kind, code: "", name: "", parent_code: null, disabled: false, props: {}, memo: "" };
  const mask = modal(`
    <h3>${isEdit ? "编辑档案" : "新增档案"}（${esc(AUX_KINDS.find((k) => k.code === kind).label)}）</h3>
    <div class="field"><label>编码 *</label><input id="au-code" value="${esc(e.code)}" /></div>
    <div class="field"><label>名称 *</label><input id="au-name" value="${esc(e.name)}" /></div>
    <div class="field"><label>上级编码（分级档案用）</label><input id="au-parent" value="${esc(e.parent_code || "")}" /></div>
    <div class="field"><label>备注</label><input id="au-memo" value="${esc(e.memo)}" /></div>
    <div class="field"><label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="au-disabled" ${e.disabled ? "checked" : ""} />停用</label></div>
    <div class="foot">
      <button class="btn ghost" id="au-cancel">取消</button>
      <button class="btn primary" id="au-save">保存</button>
    </div>
  `);
  $("#au-cancel", mask).addEventListener("click", closeModal);
  $("#au-save", mask).addEventListener("click", async () => {
    const code = $("#au-code", mask).value.trim();
    const name = $("#au-name", mask).value.trim();
    if (!code || !name) { toast("编码与名称必填", "err"); return; }
    const parent = $("#au-parent", mask).value.trim();
    const body = Object.assign({}, e, {
      kind, code, name,
      parent_code: parent || null,
      disabled: $("#au-disabled", mask).checked,
      memo: $("#au-memo", mask).value.trim(),
      props: e.props || {},
    });
    try {
      if (isEdit) await api(`/aux/${e.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      else await api("/aux", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("已保存", "ok"); closeModal(); viewAux(main);
    } catch (err) { toast(err.message, "err"); }
  });
}

// ---------------- 账套参数 ----------------
async function viewOptions(main) {
  main.innerHTML = `<h2>账套参数</h2><div class="muted">加载中…</div>`;
  let o;
  try { o = await api("/options"); } catch (e) { main.innerHTML = `<h2>账套参数</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  main.innerHTML = `
    <h2>账套参数</h2>
    <div class="panel" style="max-width:640px">
      <div class="field"><label>企业名称</label><input id="op-company" value="${esc(o.company)}" /></div>
      <div class="field"><label>纳税识别号</label><input id="op-taxno" value="${esc(o.tax_no)}" /></div>
      <div class="field"><label>本位币</label><input id="op-currency" value="${esc(o.base_currency)}" /></div>
      <div class="field"><label>启用期间（YYYYMM）</label><input id="op-start" value="${esc(o.start_period)}" /></div>
      <div class="field"><label>科目编码级长（逗号分隔，如 4,2,2,2,2）</label><input id="op-scheme" value="${esc((o.code_scheme || []).join(","))}" /></div>
      <div class="field"><label>凭证字方案（逗号分隔，如 记,收,付,转）</label><input id="op-words" value="${esc((o.voucher_words || []).join(","))}" /></div>
      <div class="field" style="display:flex;gap:24px;flex-wrap:wrap">
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-audit" ${o.enable_audit ? "checked" : ""} />启用审核环节（未审核不能记账）</label>
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-qty" ${o.enable_qty ? "checked" : ""} />启用数量核算</label>
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-foreign" ${o.enable_foreign ? "checked" : ""} />启用外币核算</label>
      </div>
      <p class="muted" style="font-size:12px">启用期间与科目级长影响科目编码校验与凭证编号，修改请谨慎；已开账后不建议改动。</p>
      ${can("sys_option") ? `<div class="foot" style="margin-top:10px"><button class="btn primary" id="op-save">保存参数</button></div>` : `<p class="muted">无修改权限（需要 sys_option）</p>`}
    </div>`;
  const save = $("#op-save");
  if (save) save.addEventListener("click", async () => {
    const scheme = $("#op-scheme").value.split(",").map((s) => parseInt(s.trim(), 10)).filter((n) => n > 0);
    const words = $("#op-words").value.split(/[,，]/).map((s) => s.trim()).filter(Boolean);
    const start = $("#op-start").value.trim();
    const body = Object.assign({}, o, {
      company: $("#op-company").value.trim(),
      tax_no: $("#op-taxno").value.trim(),
      base_currency: $("#op-currency").value.trim() || "CNY",
      start_period: /^\d{6}$/.test(start) ? parseInt(start, 10) : o.start_period,
      code_scheme: scheme.length ? scheme : o.code_scheme,
      voucher_words: words.length ? words : o.voucher_words,
      enable_audit: $("#op-audit").checked,
      enable_qty: $("#op-qty").checked,
      enable_foreign: $("#op-foreign").checked,
    });
    try { await api("/options", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已保存账套参数", "ok"); }
    catch (e) { toast(e.message, "err"); }
  });
}

// ---------------- 操作日志 ----------------
async function viewLogs(main) {
  main.innerHTML = `
    <h2>操作日志</h2>
    <div class="toolbar">
      <input id="log-kw" placeholder="搜索关键字（用户 / 动作 / 详情）" style="width:220px" />
      <select id="log-limit">
        <option value="100">最近 100 条</option>
        <option value="200" selected>最近 200 条</option>
        <option value="500">最近 500 条</option>
        <option value="1000">最近 1000 条</option>
      </select>
      <button class="btn" id="log-query">查询</button>
    </div>
    <div class="panel" style="padding:0;overflow:auto;max-height:70vh">
      <table class="grid"><thead><tr><th>时间</th><th>用户</th><th>模块</th><th>动作</th><th>详情</th></tr></thead>
      <tbody id="log-rows"><tr><td colspan="5" class="muted" style="text-align:center;padding:18px">加载中…</td></tr></tbody></table>
    </div>`;
  async function load() {
    const q = $("#log-kw").value.trim();
    const limit = $("#log-limit").value;
    const rows = await api(`/logs?limit=${limit}${q ? `&q=${encodeURIComponent(q)}` : ""}`);
    $("#log-rows").innerHTML = rows.length ? rows.map((l) => `
      <tr>
        <td style="white-space:nowrap">${esc(l.ts)}</td>
        <td>${esc(l.user)}</td>
        <td><span class="tag">${esc(l.module)}</span></td>
        <td>${esc(l.action)}</td>
        <td class="muted">${esc(l.detail)}</td>
      </tr>`).join("") : `<tr><td colspan="5" class="muted" style="text-align:center;padding:18px">无日志</td></tr>`;
  }
  $("#log-query").addEventListener("click", () => load().catch((e) => toast(e.message, "err")));
  $("#log-kw").addEventListener("keydown", (e) => { if (e.key === "Enter") load().catch((err) => toast(err.message, "err")); });
  try { await load(); } catch (e) { $("#log-rows").innerHTML = `<tr><td colspan="5" style="color:var(--err);text-align:center;padding:18px">${esc(e.message)}</td></tr>`; }
}

// ---------------- 备份恢复 ----------------
async function viewBackup(main) {
  main.innerHTML = `
    <h2>备份恢复</h2>
    <div class="toolbar">
      <span class="muted">备份当前账套到服务器 backups 目录；恢复会先自动备份一次当前数据。</span>
      <div class="spacer"></div>
      ${can("backup") ? `<button class="btn primary" id="bk-new">立即备份</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:auto">
      <table class="grid"><thead><tr><th>备份文件</th><th class="num">大小</th><th>时间</th><th></th></tr></thead>
      <tbody id="bk-rows"><tr><td colspan="4" class="muted" style="text-align:center;padding:18px">加载中…</td></tr></tbody></table>
    </div>`;
  async function load() {
    const d = await api("/backups");
    const items = d.items || [];
    $("#bk-rows").innerHTML = items.length ? items.map((b) => {
      const kb = b.size / 1024;
      const size = kb > 1024 ? (kb / 1024).toFixed(2) + " MB" : kb.toFixed(1) + " KB";
      return `<tr>
        <td>${esc(b.name)}</td>
        <td class="num">${size}</td>
        <td class="muted">${esc(String(b.mtime).replace(/\.\d+ /, " "))}</td>
        <td class="row-actions">${can("backup") ? `<button class="btn sm ghost" data-restore="${esc(b.name)}">恢复</button>` : ""}</td>
      </tr>`;
    }).join("") : `<tr><td colspan="4" class="muted" style="text-align:center;padding:18px">暂无备份</td></tr>`;
    $all("[data-restore]", main).forEach((btn) => btn.addEventListener("click", async () => {
      const name = btn.dataset.restore;
      if (!(await confirmDialog(`确定从 ${name} 恢复账套？当前数据将先自动备份一份，但恢复后本账套将回到备份时点的状态。`, true))) return;
      try {
        await api("/restore", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ file: name }) });
        toast("恢复完成", "ok"); load();
      } catch (e) { toast(e.message, "err"); }
    }));
  }
  const mk = $("#bk-new");
  if (mk) mk.addEventListener("click", async () => {
    try { const r = await api("/backups", { method: "POST" }); toast(`已备份：${r.name}`, "ok"); load(); }
    catch (e) { toast(e.message, "err"); }
  });
  try { await load(); } catch (e) { $("#bk-rows").innerHTML = `<tr><td colspan="4" style="color:var(--err);text-align:center;padding:18px">${esc(e.message)}</td></tr>`; }
}

// ---------------- 凭证模板 ----------------
async function viewTemplates(main) {
  await ensureAccounts();
  main.innerHTML = `
    <h2>凭证模板</h2>
    <div class="toolbar">
      <button class="btn sm ${!state.tplTab || state.tplTab === "list" ? "primary" : "ghost"}" id="tpl-tab-list">模板列表</button>
      <button class="btn sm ${state.tplTab === "due" ? "primary" : "ghost"}" id="tpl-tab-due">本期到期</button>
      <div class="spacer"></div>
      ${can("voucher_new") && (!state.tplTab || state.tplTab === "list") ? `<button class="btn primary" id="tpl-new">新增模板</button>` : ""}
    </div>
    <div id="tpl-body" class="muted">加载中…</div>`;
  const body = $("#tpl-body");
  const tab = state.tplTab || "list";
  $("#tpl-tab-list").onclick = () => { state.tplTab = "list"; viewTemplates(main); };
  $("#tpl-tab-due").onclick = () => { state.tplTab = "due"; viewTemplates(main); };
  const refresh = () => viewTemplates(main);

  if (tab === "list") {
    let rows;
    try { rows = await api("/templates"); } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; return; }
    body.className = "";
    body.innerHTML = `
      <div class="panel" style="padding:0;overflow:auto">
        <table class="grid"><thead><tr><th>名称</th><th class="num">分录数</th><th>频率</th><th>生效期间</th><th>上次生成</th><th>状态</th><th>备注</th><th></th></tr></thead>
        <tbody>
          ${rows.length ? rows.map((t) => `
            <tr>
              <td><b>${esc(t.name)}</b></td>
              <td class="num">${t.entries.length}</td>
              <td>${esc(FREQ_LABELS[t.freq] || t.freq)}</td>
              <td>${t.start_period || "—"} ~ ${t.end_period || "—"}</td>
              <td>${t.last_period || "—"}</td>
              <td>${t.active ? `<span class="tag ok">启用</span>` : `<span class="tag err">停用</span>`}</td>
              <td class="muted">${esc(t.memo)}</td>
              <td class="row-actions">
                ${can("voucher_new") ? `<button class="btn sm ghost" data-act="edit" data-id="${t.id}">编辑</button>` : ""}
                ${can("voucher_new") ? `<button class="btn sm ghost" data-act="gen" data-id="${t.id}">生成凭证</button>` : ""}
                ${can("voucher_new") ? `<button class="btn sm ghost" data-act="del" data-id="${t.id}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:18px">暂无模板，点「新增模板」创建</td></tr>`}
        </tbody></table>
      </div>`;
    $all("[data-act]", body).forEach((b) => b.addEventListener("click", async () => {
      const id = parseInt(b.dataset.id, 10);
      const t = rows.find((x) => x.id === id);
      if (b.dataset.act === "edit" && t) openTemplateEditor(refresh, t);
      else if (b.dataset.act === "gen" && t) await genVoucherFromTemplate(t);
      else if (b.dataset.act === "del") {
        if (!(await confirmDialog(`确定删除模板「${t.name}」？`, true))) return;
        try { await api(`/templates/${id}`, { method: "DELETE" }); toast("已删除", "ok"); refresh(); }
        catch (e) { toast(e.message, "err"); }
      }
    }));
    if ($("#tpl-new")) $("#tpl-new").addEventListener("click", () => openTemplateEditor(refresh, null));
  } else {
    const period = ymm(state.current || "") || ymm(today().slice(0, 7));
    let rows;
    try { rows = await api(`/templates/due?period=${period}`); } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; return; }
    body.className = "";
    body.innerHTML = `
      <div class="muted" style="margin-bottom:8px">期间 ${period} 内应生成的模板（按频率与生效区间判断）：</div>
      <div class="panel" style="padding:0;overflow:auto">
        <table class="grid"><thead><tr><th>名称</th><th>频率</th><th class="num">分录数</th><th>上次生成</th><th></th></tr></thead>
        <tbody>
          ${rows.length ? rows.map((t) => `
            <tr>
              <td><b>${esc(t.name)}</b></td>
              <td>${esc(FREQ_LABELS[t.freq] || t.freq)}</td>
              <td class="num">${t.entries.length}</td>
              <td>${t.last_period || "从未"}</td>
              <td class="row-actions">${can("voucher_new") ? `<button class="btn sm primary" data-gen="${t.id}">生成凭证</button>` : ""}</td>
            </tr>`).join("") : `<tr><td colspan="5" class="muted" style="text-align:center;padding:18px">本期无到期模板</td></tr>`}
        </tbody></table>
      </div>`;
    $all("[data-gen]", body).forEach((b) => b.addEventListener("click", async () => {
      const t = rows.find((x) => x.id === parseInt(b.dataset.gen, 10));
      if (t) { await generateTemplateVoucher(t, period); refresh(); }
    }));
  }
}

// 按模板直接生成凭证并回写 last_period（走后端 generate，用于「本期到期」闭环）
async function generateTemplateVoucher(t, period) {
  if (!t.entries.length) { toast("模板没有分录", "err"); return null; }
  if (!(await confirmDialog(`按模板「${t.name}」生成 ${period} 的记账凭证？金额为空的科目将按 0 记账。`))) return null;
  try {
    const r = await api(`/templates/${t.id}/generate`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(period), date: "" }) });
    toast(`已生成凭证 #${r.id}`, "ok");
    return r;
  } catch (e) { toast(e.message, "err"); return null; }
}

// 由模板打开凭证编辑器（空金额分录预填为 0，可在编辑器内补全）——用于手工调用场景
async function genVoucherFromTemplate(t) {
  if (!t.entries.length) { toast("模板没有分录", "err"); return; }
  const unPriced = t.entries.filter((e) => !String(e.amount || "").trim());
  if (unPriced.length) {
    if (!(await confirmDialog(`模板有 ${unPriced.length} 条分录未填金额，将预填为 0，打开凭证编辑器后请补全。继续？`))) return;
  }
  await openVoucherEditor(null, t.entries);
}

function openTemplateEditor(refresh, t) {
  const isEdit = !!t;
  const tpl = t || { id: 0, name: "", memo: "", entries: [{ summary: "", account_code: "", dir: "debit", amount: "", aux: {} }], freq: "manual", start_period: null, end_period: null, last_period: null, active: true };
  const mask = modal(`
    <h3>${isEdit ? "编辑模板" : "新增模板"}</h3>
    <div class="field"><label>模板名称 *</label><input id="tp-name" value="${esc(tpl.name)}" /></div>
    <div class="field"><label>频率</label>
      <select id="tp-freq">
        <option value="manual" ${tpl.freq === "manual" ? "selected" : ""}>手工调用（录凭证时选用）</option>
        <option value="monthly" ${tpl.freq === "monthly" ? "selected" : ""}>每月生成</option>
        <option value="quarterly" ${tpl.freq === "quarterly" ? "selected" : ""}>每季生成</option>
        <option value="yearly" ${tpl.freq === "yearly" ? "selected" : ""}>每年生成</option>
      </select>
    </div>
    <div class="field" style="display:flex;gap:12px">
      <div><label>生效起始期间（YYYYMM）</label><input id="tp-start" value="${tpl.start_period || ""}" style="width:110px" /></div>
      <div><label>生效结束期间</label><input id="tp-end" value="${tpl.end_period || ""}" style="width:110px" /></div>
      <div style="align-self:flex-end"><label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="tp-active" ${tpl.active ? "checked" : ""} />启用</label></div>
    </div>
    <div class="field"><label>备注</label><input id="tp-memo" value="${esc(tpl.memo)}" /></div>
    <div class="field">
      <label>分录（金额留空 = 生成时待填）</label>
      <table class="grid" id="tp-entries"><thead><tr><th>摘要</th><th>科目</th><th>方向</th><th>金额</th><th></th></tr></thead><tbody></tbody></table>
      <button class="btn ghost sm" id="tp-add" style="margin-top:6px">加分录</button>
    </div>
    <div class="foot">
      <button class="btn ghost" id="tp-cancel">取消</button>
      <button class="btn primary" id="tp-save">保存</button>
    </div>
  `, true);
  const tbody = $("#tp-entries tbody", mask);
  function renderRows() {
    tbody.innerHTML = tpl.entries.map((e, i) => `
      <tr>
        <td><input class="te-sum" data-i="${i}" value="${esc(e.summary)}" style="width:100%" /></td>
        <td>${accountOptions()}</td>
        <td><select class="te-dir" data-i="${i}"><option value="debit" ${e.dir !== "credit" ? "selected" : ""}>借</option><option value="credit" ${e.dir === "credit" ? "selected" : ""}>贷</option></select></td>
        <td><input class="te-amt" data-i="${i}" value="${esc(e.amount)}" style="width:100px;text-align:right" /></td>
        <td><button class="btn ghost sm" data-rm="${i}">×</button></td>
      </tr>`).join("");
    $all("select.acct-sel", tbody).forEach((sel, i) => { sel.value = tpl.entries[i].account_code; sel.onchange = () => tpl.entries[i].account_code = sel.value; });
    $all(".te-sum", tbody).forEach((inp) => inp.oninput = () => tpl.entries[+inp.dataset.i].summary = inp.value);
    $all(".te-dir", tbody).forEach((sel) => sel.onchange = () => tpl.entries[+sel.dataset.i].dir = sel.value);
    $all(".te-amt", tbody).forEach((inp) => inp.oninput = () => tpl.entries[+inp.dataset.i].amount = inp.value.trim());
    $all("[data-rm]", tbody).forEach((b) => b.onclick = () => { tpl.entries.splice(+b.dataset.rm, 1); renderRows(); });
  }
  renderRows();
  $("#tp-add", mask).onclick = () => { tpl.entries.push({ summary: "", account_code: "", dir: "debit", amount: "", aux: {} }); renderRows(); };
  $("#tp-cancel", mask).addEventListener("click", closeModal);
  $("#tp-save", mask).addEventListener("click", async () => {
    const name = $("#tp-name", mask).value.trim();
    if (!name) { toast("模板名称必填", "err"); return; }
    if (tpl.entries.some((e) => !e.account_code)) { toast("每条分录都要选科目", "err"); return; }
    const start = $("#tp-start", mask).value.trim();
    const end = $("#tp-end", mask).value.trim();
    const body = Object.assign({}, tpl, {
      id: isEdit ? tpl.id : 0,
      name,
      freq: $("#tp-freq", mask).value,
      start_period: /^\d{6}$/.test(start) ? parseInt(start, 10) : null,
      end_period: /^\d{6}$/.test(end) ? parseInt(end, 10) : null,
      active: $("#tp-active", mask).checked,
      memo: $("#tp-memo", mask).value.trim(),
    });
    try {
      if (isEdit) await api(`/templates/${tpl.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      else await api("/templates", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("已保存模板", "ok"); closeModal(); if (refresh) refresh();
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 工资管理（对齐桌面端：工资表 / 个税明细 / 凭证生成）
// ===========================================================================
function nextPeriod(p) {
  const [y, m] = String(p).split("-").map(Number);
  const nm = m + 1 > 12 ? 1 : m + 1, ny = m + 1 > 12 ? y + 1 : y;
  return `${ny}-${String(nm).padStart(2, "0")}`;
}
const CLAIM_STATUS = [
  { code: "", label: "全部状态" },
  { code: "draft", label: "草稿" },
  { code: "submitted", label: "待审批" },
  { code: "approved", label: "已批准" },
  { code: "rejected", label: "已驳回" },
  { code: "paid", label: "已付款" },
];

async function viewPayroll(main) {
  const tab = state.payTab || "sheet";
  const period = state.payPeriod || state.current || today().slice(0, 7);
  main.innerHTML = `
    <h2>工资管理</h2>
    <div class="toolbar">
      <button class="btn sm ${tab === "sheet" ? "primary" : "ghost"}" id="py-tab-sheet">工资表</button>
      <button class="btn sm ${tab === "tax" ? "primary" : "ghost"}" id="py-tab-tax">个税明细</button>
      <button class="btn sm ${tab === "voucher" ? "primary" : "ghost"}" id="py-tab-voucher">凭证生成</button>
      <div class="spacer"></div>
      <button class="btn ghost sm" id="py-prev">◀ 上期</button>
      <label>期间 <input id="py-period" value="${esc(period)}" style="width:90px" /></label>
      <button class="btn ghost sm" id="py-next">下期 ▶</button>
      <button class="btn" id="py-refresh">刷新</button>
      ${can("export") ? `<button class="btn ghost sm" id="py-export">导出 CSV</button>` : ""}
    </div>
    <div id="py-body" class="muted">加载中…</div>`;
  const body = $("#py-body");
  const switchTab = (t) => { state.payTab = t; viewPayroll(main); };
  const switchPeriod = (p) => { state.payPeriod = p; viewPayroll(main); };
  $("#py-tab-sheet").onclick = () => switchTab("sheet");
  $("#py-tab-tax").onclick = () => switchTab("tax");
  $("#py-tab-voucher").onclick = () => switchTab("voucher");
  $("#py-prev").onclick = () => switchPeriod(prevPeriod(period));
  $("#py-next").onclick = () => switchPeriod(nextPeriod(period));
  $("#py-refresh").onclick = () => switchPeriod($("#py-period").value.trim() || period);
  $("#py-period").addEventListener("keydown", (e) => { if (e.key === "Enter") switchPeriod($("#py-period").value.trim() || period); });
  if ($("#py-export")) $("#py-export").onclick = () => window.open(`/api/export/payroll?period=${encodeURIComponent(period)}`, "_blank");

  const ymm6 = ymm(period);
  let rows = [], employees = [];
  try {
    [rows, employees] = await Promise.all([
      api(`/payroll?period=${ymm6}`),
      api("/aux?kind=employee").catch(() => []),
    ]);
  } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; return; }
  const empName = (code) => { const e = employees.find((x) => x.code === code); return e ? e.name : ""; };
  const voucherLink = (id) => (id ? `<a href="#" data-voucher="${id}">凭证 #${id}</a>` : "—");

  if (tab === "sheet") {
    body.className = "";
    const total = (k) => rows.reduce((s, r) => s + (parseFloat(r[k]) || 0), 0);
    body.innerHTML = `
      <div class="toolbar">
        <span class="muted">共 ${rows.length} 人 · 个税与实发由后端按累计预扣预缴法自动计算</span>
        <div class="spacer"></div>
        ${can("voucher_new") ? `<button class="btn primary" id="py-new">新增工资行</button>` : ""}
      </div>
      <div class="panel" style="padding:0;overflow:auto;max-height:58vh">
        <table class="grid">
          <thead><tr><th>员工</th><th>部门</th><th class="num">应发</th><th class="num">社保(个人)</th><th class="num">公积金(个人)</th><th class="num">其他扣除</th><th class="num">专项附加</th><th class="num">计税基数</th><th class="num">个税</th><th class="num">实发</th><th class="num">单位社保</th><th class="num">单位公积金</th><th>凭证</th><th></th></tr></thead>
          <tbody>
            ${rows.length ? rows.map((r) => `
              <tr>
                <td title="${esc(r.employee)}">${esc(empName(r.employee) || r.employee)}</td>
                <td>${esc(r.dept) || "—"}</td>
                <td class="num">${fmt(r.gross)}</td><td class="num">${fmt(r.social)}</td>
                <td class="num">${fmt(r.housing)}</td><td class="num">${fmt(r.deduction)}</td>
                <td class="num">${fmt(r.additional)}</td><td class="num">${fmt(r.tax_base)}</td>
                <td class="num">${fmt(r.tax)}</td><td class="num"><b>${fmt(r.net)}</b></td>
                <td class="num">${fmt(r.social_co)}</td><td class="num">${fmt(r.housing_co)}</td>
                <td>${voucherLink(r.voucher_id)}</td>
                <td class="row-actions">
                  ${can("voucher_new") && !r.voucher_id ? `<button class="btn sm ghost" data-edit="${r.id}">改</button>` : ""}
                  ${can("voucher_new") && !r.voucher_id ? `<button class="btn sm ghost" data-del="${r.id}">删</button>` : ""}
                </td>
              </tr>`).join("") : `<tr><td colspan="14" class="muted" style="text-align:center;padding:18px">${period} 无工资数据</td></tr>`}
          </tbody>
        </table>
      </div>
      ${rows.length ? `<div class="cards" style="margin-top:12px">
        <div class="card"><div class="k">应发合计</div><div class="v" style="font-size:16px">${fmt(total("gross").toFixed(2))}</div></div>
        <div class="card"><div class="k">个税合计</div><div class="v" style="font-size:16px">${fmt(total("tax").toFixed(2))}</div></div>
        <div class="card"><div class="k">实发合计</div><div class="v" style="font-size:16px">${fmt(total("net").toFixed(2))}</div></div>
        <div class="card"><div class="k">单位社保+公积金</div><div class="v" style="font-size:16px">${fmt((total("social_co") + total("housing_co")).toFixed(2))}</div></div>
      </div>` : ""}`;
    $all("[data-voucher]", body).forEach((a) => a.addEventListener("click", (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.voucher, 10)); }));
    const newBtn = $("#py-new");
    if (newBtn) newBtn.addEventListener("click", () => openPayrollEditor(main, null, period, employees));
    $all("[data-edit]", body).forEach((b) => b.addEventListener("click", () => {
      const r = rows.find((x) => x.id === parseInt(b.dataset.edit, 10));
      if (r) openPayrollEditor(main, r, period, employees);
    }));
    $all("[data-del]", body).forEach((b) => b.addEventListener("click", async () => {
      if (!(await confirmDialog("确定删除该工资行？", true))) return;
      try { await api(`/payroll/${b.dataset.del}`, { method: "DELETE" }); toast("已删除", "ok"); viewPayroll(main); }
      catch (e) { toast(e.message, "err"); }
    }));
  } else if (tab === "tax") {
    body.className = "";
    body.innerHTML = `
      <div class="toolbar">
        <label>员工 <select id="py-emp"><option value="">选择职员…</option>${employees.map((e) => `<option value="${esc(e.code)}">${esc(e.code)} ${esc(e.name)}</option>`).join("")}</select></label>
        <button class="btn" id="py-tax-run">查询累计</button>
      </div>
      <div id="py-tax-box" class="muted">选择员工后查看本年至上月的累计数与本期的个税计算。</div>`;
    $("#py-tax-run").addEventListener("click", async () => {
      const code = $("#py-emp").value;
      if (!code) { toast("请选择员工", "err"); return; }
      const box = $("#py-tax-box");
      try {
        const cur = rows.find((r) => r.employee === code);
        if (!cur) { box.innerHTML = `<div class="muted">${esc(empName(code) || code)} 在 ${period} 没有工资数据，请先在「工资表」录入。</div>`; return; }
        const ytd = await api(`/payroll/ytd?period=${ymm6}&employee=${encodeURIComponent(code)}`);
        box.innerHTML = `
          <div class="cards" style="margin-top:12px">
            <div class="card"><div class="k">累计收入（本年至上月）</div><div class="v" style="font-size:16px">${fmt(ytd.income)}</div></div>
            <div class="card"><div class="k">累计专项扣除（社保+公积金）</div><div class="v" style="font-size:16px">${fmt(ytd.special)}</div></div>
            <div class="card"><div class="k">累计专项附加扣除</div><div class="v" style="font-size:16px">${fmt(ytd.additional)}</div></div>
            <div class="card"><div class="k">累计已预扣个税</div><div class="v" style="font-size:16px">${fmt(ytd.withheld)}</div></div>
            <div class="card"><div class="k">已有月数</div><div class="v" style="font-size:16px">${ytd.months}</div></div>
          </div>
          <div class="cards" style="margin-top:12px">
            <div class="card"><div class="k">本期应发</div><div class="v" style="font-size:16px">${fmt(cur.gross)}</div></div>
            <div class="card"><div class="k">本期计税基数</div><div class="v" style="font-size:16px">${fmt(cur.tax_base)}</div></div>
            <div class="card"><div class="k">本期个税</div><div class="v" style="font-size:16px">${fmt(cur.tax)}</div></div>
            <div class="card"><div class="k">本期实发</div><div class="v" style="font-size:16px">${fmt(cur.net)}</div></div>
          </div>`;
      } catch (e) { box.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    });
  } else {
    // 凭证生成：默认科目与桌面端 finui VoucherCfg 一致
    body.className = "";
    body.innerHTML = `
      <div class="panel" style="max-width:660px">
        <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
          <div><label>凭证日期（留空 = 期间末日）</label><input id="pv-date" type="date" value="${esc(today())}" style="width:150px" /></div>
        </div>
        <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
          <div><label>费用科目</label><input id="pv-expense" value="6602" style="width:90px" /></div>
          <div><label>应付工资</label><input id="pv-wage" value="221101" style="width:90px" /></div>
          <div><label>应付社保</label><input id="pv-social" value="221103" style="width:90px" /></div>
          <div><label>应付公积金</label><input id="pv-housing" value="221104" style="width:90px" /></div>
          <div><label>其他应付款(个人)</label><input id="pv-personal" value="2241" style="width:90px" /></div>
          <div><label>银行存款</label><input id="pv-bank" value="100201" style="width:90px" /></div>
          <div><label>应交个税</label><input id="pv-tax" value="222103" style="width:90px" /></div>
        </div>
        <p class="muted" style="font-size:12px">计提凭证按部门拆分借方费用；社保缴纳与工资发放凭证走银行存款；同一期凭证只能生成一次，重复生成由引擎报错拦截。</p>
        <div class="foot" style="margin-top:6px;display:flex;gap:10px">
          ${rows.length ? `
          <button class="btn primary" id="pv-accrue">生成计提凭证</button>
          <button class="btn" id="pv-social">生成社保缴纳凭证</button>
          <button class="btn" id="pv-pay">生成发放凭证</button>` : `<span class="muted">本期无工资数据，先在「工资表」录入。</span>`}
        </div>
        <div id="pv-result" class="muted" style="margin-top:10px"></div>
      </div>`;
    const q = `period=${ymm6}`;
    const runVoucher = (url, bodyObj, label) => (async () => {
      if (!(await confirmDialog(`确认为 ${period} 生成${label}？`))) return;
      try {
        const r = await api(`${url}?${q}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(bodyObj) });
        $("#pv-result").innerHTML = r.id ? `<span style="color:var(--ok)">已生成凭证 #${r.id}，<a href="#" id="pv-open">点击查看</a></span>` : `<span class="muted">无可生成内容（金额全为零）</span>`;
        if (r.id) $("#pv-open").addEventListener("click", (e) => { e.preventDefault(); openVoucherEditor(r.id); });
      } catch (e) { toast(e.message, "err"); }
    });
    if ($("#pv-accrue")) $("#pv-accrue").onclick = runVoucher("/payroll/accrue", { date: $("#pv-date").value, expense: $("#pv-expense").value.trim(), wage_payable: $("#pv-wage").value.trim(), social_payable: $("#pv-social").value.trim(), housing_payable: $("#pv-housing").value.trim() }, "计提凭证");
    if ($("#pv-social")) $("#pv-social").onclick = runVoucher("/payroll/social-pay", { date: $("#pv-date").value, social_payable: $("#pv-social").value.trim(), housing_payable: $("#pv-housing").value.trim(), personal_payable: $("#pv-personal").value.trim(), bank_account: $("#pv-bank").value.trim() }, "社保缴纳凭证");
    if ($("#pv-pay")) $("#pv-pay").onclick = runVoucher("/payroll/pay", { date: $("#pv-date").value, payable_account: $("#pv-wage").value.trim(), bank_account: $("#pv-bank").value.trim(), tax_account: $("#pv-tax").value.trim(), social_account: $("#pv-personal").value.trim() }, "发放凭证");
  }
}

function openPayrollEditor(main, row, period, employees) {
  const isEdit = !!row;
  const r = row || { employee: "", dept: "", gross: "", social: "", housing: "", deduction: "", additional: "", social_co: "", housing_co: "", memo: "" };
  const empKnown = r.employee && employees.some((e) => e.code === r.employee);
  // 只渲染一份选项：已知员工在列表里（编辑时）不再额外加裸编码项，避免重复
  const empOpts = employees.map((e) => `<option value="${esc(e.code)}" ${r.employee === e.code ? "selected" : ""}>${esc(e.code)} ${esc(e.name)}</option>`).join("")
    + (r.employee && !empKnown ? `<option value="${esc(r.employee)}" selected>${esc(r.employee)}（档案外）</option>` : "");
  const mask = modal(`
    <h3>${isEdit ? `编辑工资行（${esc(empNameIn(employees, r.employee) || r.employee)}）` : "新增工资行"} · ${esc(period)}</h3>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>员工 *</label>${employees.length ? `<select id="pw-emp"><option value="">选择职员…</option>${empOpts}</select>` : `<input id="pw-emp" value="${esc(r.employee)}" placeholder="职员编码" />`}</div>
      <div><label>部门</label><input id="pw-dept" value="${esc(r.dept)}" style="width:110px" /></div>
    </div>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>应发工资 *</label><input id="pw-gross" value="${esc(r.gross)}" style="width:110px;text-align:right" /></div>
      <div><label>社保(个人)</label><input id="pw-social" value="${esc(r.social)}" style="width:100px;text-align:right" /></div>
      <div><label>公积金(个人)</label><input id="pw-housing" value="${esc(r.housing)}" style="width:100px;text-align:right" /></div>
      <div><label>其他扣除</label><input id="pw-ded" value="${esc(r.deduction)}" style="width:100px;text-align:right" /></div>
      <div><label>专项附加扣除</label><input id="pw-add" value="${esc(r.additional)}" style="width:110px;text-align:right" /></div>
    </div>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>单位社保</label><input id="pw-sco" value="${esc(r.social_co)}" style="width:100px;text-align:right" /></div>
      <div><label>单位公积金</label><input id="pw-hco" value="${esc(r.housing_co)}" style="width:100px;text-align:right" /></div>
      <div><label>备注</label><input id="pw-memo" value="${esc(r.memo)}" style="width:220px" /></div>
    </div>
    <p class="muted" style="font-size:12px">个税按累计预扣预缴法自动计算，实发 = 应发 − 社保 − 公积金 − 其他扣除 − 个税。</p>
    <div class="foot">
      <button class="btn ghost" id="pw-cancel">取消</button>
      <button class="btn primary" id="pw-save">计算并保存</button>
    </div>
  `);
  $("#pw-cancel", mask).addEventListener("click", closeModal);
  $("#pw-save", mask).addEventListener("click", async () => {
    const empSel = $("#pw-emp", mask);
    const employee = (empSel.value || "").trim();
    if (!employee) { toast("员工必填", "err"); return; }
    const body = {
      employee,
      dept: $("#pw-dept", mask).value.trim(),
      gross: $("#pw-gross", mask).value.trim() || "0",
      social: $("#pw-social", mask).value.trim() || "0",
      housing: $("#pw-housing", mask).value.trim() || "0",
      deduction: $("#pw-ded", mask).value.trim() || "0",
      additional: $("#pw-add", mask).value.trim() || "0",
      social_co: $("#pw-sco", mask).value.trim() || "0",
      housing_co: $("#pw-hco", mask).value.trim() || "0",
      memo: $("#pw-memo", mask).value.trim(),
    };
    try {
      const out = await api(`/payroll?period=${ymm(period)}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast(`已保存：个税 ${fmt(out.tax)}，实发 ${fmt(out.net)}`, "ok");
      closeModal(); viewPayroll(main);
    } catch (e) { toast(e.message, "err"); }
  });
}
function empNameIn(employees, code) { const e = (employees || []).find((x) => x.code === code); return e ? e.name : ""; }

// ===========================================================================
// 费用报销（草稿 → 提交 → 审批 → 支付 → 生成凭证）
// ===========================================================================
async function viewClaims(main) {
  await ensureAccounts();
  const period = state.clmPeriod || state.current || today().slice(0, 7);
  const status = state.clmStatus == null ? "" : state.clmStatus;
  main.innerHTML = `
    <h2>费用报销</h2>
    <div class="toolbar">
      <button class="btn ghost sm" id="cl-prev">◀ 上期</button>
      <label>期间 <input id="cl-period" value="${esc(period)}" style="width:90px" /></label>
      <button class="btn ghost sm" id="cl-next">下期 ▶</button>
      <label>状态 <select id="cl-status">${CLAIM_STATUS.map((s) => `<option value="${s.code}" ${status === s.code ? "selected" : ""}>${s.label}</option>`).join("")}</select></label>
      <button class="btn" id="cl-refresh">刷新</button>
      <div class="spacer"></div>
      ${can("export") ? `<button class="btn ghost sm" id="cl-export">导出 CSV</button>` : ""}
      ${can("voucher_new") ? `<button class="btn primary" id="cl-new">新增报销单</button>` : ""}
    </div>
    <div id="cl-body" class="muted">加载中…</div>`;
  const switchTo = (p, s) => { state.clmPeriod = p; state.clmStatus = s; viewClaims(main); };
  $("#cl-prev").onclick = () => switchTo(prevPeriod(period), status);
  $("#cl-next").onclick = () => switchTo(nextPeriod(period), status);
  $("#cl-refresh").onclick = () => switchTo($("#cl-period").value.trim() || period, $("#cl-status").value);
  if ($("#cl-export")) $("#cl-export").onclick = () => {
    const qs = new URLSearchParams({ period, status: $("#cl-status").value });
    window.open(`/api/export/claims?${qs.toString()}`, "_blank");
  };
  if ($("#cl-new")) $("#cl-new").addEventListener("click", () => openClaimEditor(main, null, period));

  const body = $("#cl-body");
  let rows;
  try {
    const q = `period=${ymm(period)}${status ? `&status=${status}` : ""}`;
    rows = await api(`/claims?${q}`);
  } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; return; }
  body.className = "";
  const badge = (s) => {
    const map = { draft: ["tag", "草稿"], submitted: ["tag warn", "待审批"], approved: ["tag ok", "已批准"], rejected: ["tag err", "已驳回"], paid: ["tag ok", "已付款"] };
    const [cls, label] = map[s] || ["tag", s];
    return `<span class="${cls}">${label}</span>`;
  };
  // 状态流转动作（与桌面端 actions() 一致）
  const actionsOf = (r) => {
    switch (r.status) {
      case "draft": return [["提交", "submitted"]];
      case "submitted": return [["审批通过", "approved"], ["驳回", "rejected"]];
      case "approved": return [["支付", "paid"]];
      case "rejected": return [["退回草稿", "draft"]];
      default: return [];
    }
  };
  const total = rows.reduce((s, r) => s + (parseFloat(r.amount) || 0), 0);
  body.innerHTML = `
    <div class="panel" style="padding:0;overflow:auto;max-height:62vh">
      <table class="grid">
        <thead><tr><th>单号</th><th>日期</th><th>申请人</th><th>部门</th><th>事由</th><th class="num">金额</th><th>状态</th><th>凭证</th><th></th></tr></thead>
        <tbody>
          ${rows.length ? rows.map((r) => `
            <tr>
              <td><a href="#" data-view="${r.id}"><b>${esc(r.no)}</b></a></td>
              <td>${esc(r.biz_date)}</td>
              <td>${esc(r.applicant)}</td>
              <td>${esc(r.dept) || "—"}</td>
              <td>${esc(r.reason)}</td>
              <td class="num">${fmt(r.amount)}</td>
              <td>${badge(r.status)}</td>
              <td>${r.voucher_id ? `<a href="#" data-voucher="${r.voucher_id}">#${r.voucher_id}</a>` : "—"}</td>
              <td class="row-actions">
                ${can("voucher_new") && ["draft", "rejected"].includes(r.status) && !r.voucher_id ? `<button class="btn sm ghost" data-edit="${r.id}">改</button><button class="btn sm ghost" data-del="${r.id}">删</button>` : ""}
                ${can("voucher_new") ? actionsOf(r).map(([label, to]) => `<button class="btn sm ghost" data-trans="${r.id}" data-to="${to}">${label}</button>`).join("") : ""}
                ${can("voucher_new") && r.status === "paid" && !r.voucher_id ? `<button class="btn sm primary" data-vgen="${r.id}">生成凭证</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="9" class="muted" style="text-align:center;padding:18px">${period} 无报销单</td></tr>`}
        </tbody>
      </table>
    </div>
    ${rows.length ? `<div class="muted" style="margin-top:10px">本期报销金额合计 <b>${fmt(total.toFixed(2))}</b></div>` : ""}`;
  const reload = () => viewClaims(main);
  $all("[data-view]", body).forEach((a) => a.addEventListener("click", (e) => {
    e.preventDefault();
    const r = rows.find((x) => x.id === parseInt(a.dataset.view, 10));
    if (r) showClaimDetail(r);
  }));
  $all("[data-voucher]", body).forEach((a) => a.addEventListener("click", (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.voucher, 10)); }));
  $all("[data-edit]", body).forEach((b) => b.addEventListener("click", () => {
    const r = rows.find((x) => x.id === parseInt(b.dataset.edit, 10));
    if (r) openClaimEditor(main, r, period);
  }));
  $all("[data-del]", body).forEach((b) => b.addEventListener("click", async () => {
    if (!(await confirmDialog("删除后不可恢复（已生成凭证的单据需先删除凭证），确定删除？", true))) return;
    try { await api(`/claims/${b.dataset.del}`, { method: "DELETE" }); toast("已删除", "ok"); reload(); }
    catch (e) { toast(e.message, "err"); }
  }));
  $all("[data-trans]", body).forEach((b) => b.addEventListener("click", async () => {
    const to = b.dataset.to;
    try { await api(`/claims/${b.dataset.trans}/transition`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: to }) }); toast("状态已更新", "ok"); reload(); }
    catch (e) { toast(e.message, "err"); }
  }));
  $all("[data-vgen]", body).forEach((b) => b.addEventListener("click", () => {
    const r = rows.find((x) => x.id === parseInt(b.dataset.vgen, 10));
    if (!r) return;
    const mask = modal(`
      <h3>生成报销凭证 · ${esc(r.no)}</h3>
      <div class="field"><label>贷方支付科目（如 100201 银行存款）*</label><input id="cv-pay" value="100201" /></div>
      <p class="muted" style="font-size:12px">借方按明细行的费用科目拆分；明细合计必须等于单据金额。</p>
      <div class="foot">
        <button class="btn ghost" id="cv-cancel">取消</button>
        <button class="btn primary" id="cv-ok">生成</button>
      </div>`);
    $("#cv-cancel", mask).onclick = closeModal;
    $("#cv-ok", mask).onclick = async () => {
      const pay = $("#cv-pay", mask).value.trim();
      if (!pay) { toast("请填写支付科目", "err"); return; }
      try {
        const out = await api(`/claims/${r.id}/voucher`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ pay_account: pay }) });
        toast(`已生成凭证 #${out.id}`, "ok"); closeModal(); reload();
      } catch (e) { toast(e.message, "err"); }
    };
  }));
}

function showClaimDetail(c) {
  const stLabel = (CLAIM_STATUS.find((s) => s.code === c.status) || {}).label || c.status;
  const mask = modal(`
    <h3>报销单 ${esc(c.no)}</h3>
    <div class="muted" style="line-height:1.9;font-size:13px">
      日期：${esc(c.biz_date)} · 申请人：${esc(c.applicant)}${c.dept ? ` · 部门：${esc(c.dept)}` : ""}<br />
      事由：${esc(c.reason)}<br />
      金额：<b>${fmt(c.amount)}</b> · 状态：${esc(stLabel)}${c.approver ? ` · 审批人：${esc(c.approver)}${c.approved_at ? `（${esc(c.approved_at)}）` : ""}` : ""}${c.payer ? ` · 付款人：${esc(c.payer)}${c.paid_at ? `（${esc(c.paid_at)}）` : ""}` : ""}
    </div>
    <div class="panel" style="padding:0;margin-top:10px;overflow:auto">
      <table class="grid"><thead><tr><th>费用科目</th><th class="num">金额</th><th>备注</th></tr></thead>
      <tbody>${(c.items || []).map((i) => `<tr><td>${esc(i.expense_account)}</td><td class="num">${fmt(i.amount)}</td><td class="muted">${esc(i.memo)}</td></tr>`).join("") || `<tr><td colspan="3" class="muted" style="text-align:center">无明细</td></tr>`}</tbody></table>
    </div>
    <div class="foot"><button class="btn ghost" id="cd-close">关闭</button></div>`);
  $("#cd-close", mask).addEventListener("click", closeModal);
}

function openClaimEditor(main, claim, period) {
  const isEdit = !!claim;
  const c = claim ? JSON.parse(JSON.stringify(claim)) : {
    biz_date: today(), applicant: "", dept: "", reason: "", amount: "",
    items: [{ expense_account: "", amount: "", memo: "" }],
  };
  const mask = modal(`
    <h3>${isEdit ? `编辑报销单 ${esc(claim.no)}` : "新增报销单"} · ${esc(period)}</h3>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>业务日期 *</label><input id="cm-date" type="date" value="${esc(c.biz_date)}" style="width:150px" /></div>
      <div><label>申请人 *</label><input id="cm-applicant" value="${esc(c.applicant)}" style="width:110px" /></div>
      <div><label>部门</label><input id="cm-dept" value="${esc(c.dept)}" style="width:110px" /></div>
      <div><label>单据金额 *</label><input id="cm-amount" value="${esc(c.amount)}" style="width:110px;text-align:right" /></div>
    </div>
    <div class="field"><label>事由 *</label><input id="cm-reason" value="${esc(c.reason)}" style="width:100%" /></div>
    <div class="field">
      <label>费用明细（借方科目 + 金额，合计须等于单据金额）</label>
      <table class="grid" id="cm-items"><thead><tr><th>费用科目</th><th>金额</th><th>备注</th><th></th></tr></thead><tbody></tbody></table>
      <button class="btn ghost sm" id="cm-add" style="margin-top:6px">添加明细行</button>
    </div>
    <div class="foot">
      <button class="btn ghost" id="cm-cancel">取消</button>
      <button class="btn primary" id="cm-save">保存草稿</button>
    </div>
  `, true);
  const tbody = $("#cm-items tbody", mask);
  function renderRows() {
    tbody.innerHTML = c.items.map((i, k) => `
      <tr>
        <td>${accountOptions()}</td>
        <td><input class="ci-amt" data-i="${k}" value="${esc(i.amount)}" style="width:110px;text-align:right" /></td>
        <td><input class="ci-memo" data-i="${k}" value="${esc(i.memo)}" style="width:100%" /></td>
        <td><button class="btn ghost sm" data-rm="${k}">×</button></td>
      </tr>`).join("");
    $all("select.acct-sel", tbody).forEach((sel, k) => { sel.value = c.items[k].expense_account; sel.onchange = () => c.items[k].expense_account = sel.value; });
    $all(".ci-amt", tbody).forEach((inp) => inp.oninput = () => c.items[+inp.dataset.i].amount = inp.value.trim());
    $all(".ci-memo", tbody).forEach((inp) => inp.oninput = () => c.items[+inp.dataset.i].memo = inp.value);
    $all("[data-rm]", tbody).forEach((b) => b.onclick = () => { c.items.splice(+b.dataset.rm, 1); renderRows(); });
  }
  renderRows();
  $("#cm-add", mask).onclick = () => { c.items.push({ expense_account: "", amount: "", memo: "" }); renderRows(); };
  $("#cm-cancel", mask).addEventListener("click", closeModal);
  $("#cm-save", mask).addEventListener("click", async () => {
    const applicant = $("#cm-applicant", mask).value.trim();
    const reason = $("#cm-reason", mask).value.trim();
    if (!applicant) { toast("申请人必填", "err"); return; }
    if (!reason) { toast("事由必填", "err"); return; }
    if (c.items.some((i) => !i.expense_account)) { toast("每条明细都要选费用科目", "err"); return; }
    const body = {
      period: ymm(period),
      biz_date: $("#cm-date", mask).value,
      applicant, reason,
      dept: $("#cm-dept", mask).value.trim(),
      amount: $("#cm-amount", mask).value.trim() || "0",
      items: c.items.map((i) => ({ expense_account: i.expense_account, amount: i.amount || "0", memo: i.memo })),
    };
    try {
      if (isEdit) await api(`/claims/${claim.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      else {
        const out = await api("/claims", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast(`已创建草稿 ${out.no}`, "ok");
        closeModal(); viewClaims(main); return;
      }
      toast("已保存", "ok"); closeModal(); viewClaims(main);
    } catch (e) { toast(e.message, "err"); }
  });
}

// 启动：先尝试恢复已有会话。
// - /me 成功：已进入某账套，直接进应用
// - /me 失败但 /books 成功：已登录平台但未选账套 → 账套选择页
// - 都失败：显示登录页
(async function boot() {
  try {
    const me = await api("/me");
    session.user = me;
    await afterLogin();
  } catch (e1) {
    try {
      const b = await api("/books");
      showBookPicker(b);
    } catch (e2) {
      showLogin();
    }
  }
})();
