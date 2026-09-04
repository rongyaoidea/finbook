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
  { id: "vouchers", label: "记账凭证", perm: "voucher_new", group: "凭证" },
  { id: "imports", label: "数据导入", perm: "voucher_new", group: "凭证" },
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
  "reconcile": viewReconcile,
  "mrp": viewMrp,
  "routing": viewRouting,
  "approval": viewApproval,
  "notes": viewNotes,
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
        <option value="posted">已记账</option><option value="void">已作废</option>
      </select>
      <button class="btn ghost sm" id="v-refresh">查询</button>
      ${can("voucher_edit") ? `<button class="btn ghost sm" id="v-renumber">重排断号</button>` : ""}
      ${can("voucher_post") ? `<button class="btn ghost sm" id="v-batch">批量记账</button>` : ""}
      ${can("report") ? `<button class="btn ghost sm" id="v-printform">凭证套打</button>` : ""}
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

async function openVoucherEditor(id) {
  await ensureAccounts();
  let v = {
    id: 0, period: state.current, date: today(), word: "记", no: 0, attachments: 0, memo: "",
    entries: [{ line: 1, account_code: "", summary: "", debit: "0", credit: "0" }, { line: 2, account_code: "", summary: "", debit: "0", credit: "0" }],
  };
  let status = "draft", voucher_no = "";
  if (id) {
    try { v = await api(`/vouchers/${id}`); status = v.status; voucher_no = v.voucher_no; } catch (e) { toast(e.message, "err"); return; }
  } else {
    try { const n = await api(`/vouchers/next-no?period=${encodeURIComponent(state.current || "")}&word=记`); v.no = n.no; } catch (e) {}
  }
  // 可编辑状态与后端 can_edit() 对齐：未记账（含历史"已审核"）可改；已记账需先反记账
  const editable = (id === 0) || status === "draft" || status === "audited";
  const canPost = status === "draft" || status === "audited";
  const mask = modal(`
    <h3>记账凭证 ${esc(voucher_no)}</h3>
    <div class="toolbar">
      <label>日期 <input id="v-date" type="date" value="${esc(v.date)}" ${editable ? "" : "disabled"} />${editable ? `<button class="btn ghost sm" id="v-today">今天</button>` : ""}</label>
      <span id="v-date-hint" class="muted" style="font-size:12px"></span>
      <label>字 <input id="v-word" value="${esc(v.word)}" style="width:60px" ${editable ? "" : "disabled"} /></label>
      <label>号 <input id="v-no" type="number" value="${v.no}" style="width:70px" ${editable ? "" : "disabled"} /></label>
      <label>附单据 <input id="v-att" type="number" value="${v.attachments}" style="width:60px" ${editable ? "" : "disabled"} /></label>
    </div>
    <table class="grid" id="v-entries">
      <thead><tr><th style="width:40px">行</th><th>科目</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th></th></tr></thead>
      <tbody></tbody>
    </table>
    ${editable ? `<button class="btn ghost sm" id="v-add">+ 增加分录</button>` : ""}
    <div style="margin-top:10px" class="muted">合计：借 <b id="v-dt">0.00</b> 　贷 <b id="v-ct">0.00</b> 　差额 <b id="v-diff">0.00</b></div>
    <div class="foot">
      ${editable ? `<button class="btn" id="v-save">保存</button>` : ""}
      ${can("voucher_post") && canPost ? `<button class="btn primary" id="v-post">记账</button>` : ""}
      ${can("voucher_unpost") && status === "posted" ? `<button class="btn ghost" id="v-unpost">反记账</button>` : ""}
      ${can("voucher_new") && id > 0 && status !== "void" ? `<button class="btn ghost" id="v-reverse">红字冲销</button>` : ""}
      ${can("voucher_delete") && (status === "draft" || status === "audited") ? `<button class="btn danger" id="v-del">删除</button>` : ""}
      <button class="btn ghost" id="v-close">关闭</button>
    </div>
  `, true);

  const tbody = $("#v-entries tbody", mask);
  function renderRows() {
    tbody.innerHTML = v.entries.map((e, i) => `<tr>
      <td>${e.line}</td>
      <td>${accountOptions()}</td>
      <td><input class="e-sum" value="${esc(e.summary)}" style="width:100%" ${editable ? "" : "disabled"} /></td>
      <td class="num"><input class="e-d num" value="${esc(e.debit)}" style="width:110px;text-align:right" ${editable ? "" : "disabled"} /></td>
      <td class="num"><input class="e-c num" value="${esc(e.credit)}" style="width:110px;text-align:right" ${editable ? "" : "disabled"} /></td>
      <td>${editable ? `<button class="btn ghost sm e-del">×</button>` : ""}</td>
    </tr>`).join("");
    $all("select.acct-sel", tbody).forEach((sel, i) => { sel.value = v.entries[i].account_code; sel.onchange = () => v.entries[i].account_code = sel.value; });
    $all(".e-sum", tbody).forEach((inp, i) => inp.oninput = () => v.entries[i].summary = inp.value);
    $all(".e-d", tbody).forEach((inp, i) => inp.oninput = () => { v.entries[i].debit = inp.value; recalc(); });
    $all(".e-c", tbody).forEach((inp, i) => inp.oninput = () => { v.entries[i].credit = inp.value; recalc(); });
    $all(".e-del", tbody).forEach((b, i) => b.onclick = () => { v.entries.splice(i, 1); v.entries.forEach((e, k) => e.line = k + 1); renderRows(); recalc(); });
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
  if (editable) $("#v-add", mask).onclick = () => { v.entries.push({ line: v.entries.length + 1, account_code: "", summary: "", debit: "0", credit: "0" }); renderRows(); };
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
      entries: v.entries.map((e, i) => ({ line: i + 1, account_code: e.account_code, summary: e.summary, debit: String(parseFloat(e.debit) || 0), credit: String(parseFloat(e.credit) || 0) })),
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
  if ($("#v-reverse", mask)) $("#v-reverse", mask).onclick = async () => { if (!(await confirmDialog("生成该凭证的红字冲销凭证（借贷互换、摘要加「冲销」前缀），原凭证保留不动？", true))) return; try { await api(`/vouchers/${v.id}/reverse`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(state.current || ""), date: today() }) }); toast("已生成冲销凭证", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-del", mask)) $("#v-del", mask).onclick = async () => { if (!(await confirmDialog("确定删除该凭证？", true))) return; try { await api(`/vouchers/${v.id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
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
async function viewLedger(main) {
  await ensureAccounts();
  main.innerHTML = `
    <h2>明细账</h2>
    <div class="toolbar">
      <label>科目 <input id="l-code" list="acct-list" placeholder="科目编码，如 1002" style="width:160px" /></label>
      <datalist id="acct-list">${(state.accounts || []).map((a) => `<option value="${esc(a.code)}">${esc(a.name)}</option>`).join("")}</datalist>
      <label>从 <input id="l-from" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label>至 <input id="l-to" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label><input type="checkbox" id="l-children" checked /> 含下级</label>
      <label><input type="checkbox" id="l-posted" /> 仅已记账</label>
      <button class="btn sm" id="l-go">查询</button>
      <button class="btn ghost sm" id="l-print">打印预览</button>
      <button class="btn ghost sm" id="l-printform">账簿套打</button>
    </div>
    <div class="panel"><table class="grid" id="l-table"><thead><tr>
      <th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th>
    </tr></thead><tbody><tr><td colspan="7" class="muted">请输入科目后查询</td></tr></tbody></table></div>`;
  $("#l-go").addEventListener("click", loadLedger);
  $("#l-print").addEventListener("click", () => { const el = $("#l-table").querySelector("table"); printPreview("明细账", el); });
  $("#l-printform").addEventListener("click", () => {
    const code = $("#l-code").value.trim();
    if (!code) { toast("请先输入科目编码", "err"); return; }
    const q = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}&type=detail`;
    window.open(`/api/ledger/print-form?${q}`, "_blank");
  });
}
async function loadLedger() {
  const code = $("#l-code").value.trim();
  if (!code) { toast("请先输入科目编码", "err"); return; }
  const url = `/ledger?code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}`;
  const tb = $("#l-table tbody");
  let rows;
  try { rows = await api(url); } catch (e) { tb.innerHTML = `<tr><td colspan="7" style="color:var(--err)">${esc(e.message)}</td></tr>`; return; }
  if (!rows.length) { tb.innerHTML = `<tr><td colspan="7" class="muted">该科目在所选期间无记录</td></tr>`; return; }
  tb.innerHTML = rows.map((r) => `<tr>
    <td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td>
    <td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td>
    <td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td>
  </tr>`).join("");
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
      <span class="spacer"></span>
      ${can("export") ? `<button class="btn ghost sm" id="r-export">导出 CSV</button>
      <button class="btn ghost sm" id="r-pdf">导出 PDF</button>` : `<span class="tag warn" title="无导出权限">无导出权限，仅可打印</span>`}
      <button class="btn ghost sm" id="r-print">打印预览</button>
    </div>
    <div class="panel"><table class="grid" id="r-table"><thead><tr>
      <th>科目编码</th><th>科目名称</th><th>方向</th><th class="num">期初</th><th class="num">本期借方</th><th class="num">本期贷方</th><th class="num">期末</th><th class="num">本年累计借方</th><th class="num">本年累计贷方</th>
    </tr></thead><tbody><tr><td colspan="9" class="muted">点击「生成科目余额表」</td></tr></tbody></table></div>`;
  $("#r-go").addEventListener("click", loadTrial);
  $("#r-print").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.open(`/api/reports/trial-balance/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  if (can("export")) {
    $("#r-export").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.location = `/api/reports/trial-balance/export?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`; });
    $("#r-pdf").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.location = `/api/reports/trial-balance/pdf?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`; });
  }
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
        const r = await api(`/cost/period-end?period=${encodeURIComponent(p)}&apply=${apply}`);
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
