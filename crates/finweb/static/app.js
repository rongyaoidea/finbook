// FinBook Web 前端（原生 JS SPA，无框架依赖）
const API = "/api";

let session = { user: null };
let state = { view: "dashboard", periods: [], current: null, accounts: null, users: null };

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
// 登录
// ===========================================================================
async function showLogin() {
  let status = { admin_set: false };
  try { status = await api("/setup/status"); } catch (e) {}
  // 账套列表（多账套时登录页可选）
  let bookOpts = "";
  try {
    const b = await api("/books");
    const list = (b && b.books) || [];
    if (list.length > 1) {
      bookOpts = `<div class="field"><label>账套 / 公司</label>
        <select id="login-book">${list.map((x) => `<option value="${esc(x.key)}">${esc(x.company || x.key)}</option>`).join("")}</select>
      </div>`;
    } else if (list.length === 1) {
      bookOpts = `<input type="hidden" id="login-book" value="${esc(list[0].key)}" />`;
    }
  } catch (e) {}
  const app = document.getElementById("app");
  const banner = status.admin_set
    ? `<div class="banner set">✅ <b>管理员账号已设定</b>。请输入账号口令登录。</div>`
    : `<div class="banner unset">🔧 <b>管理员账号未设定</b> —— 首次成功登录的账号将自动成为系统管理员，请设置您的管理员账号与口令。</div>`;
  app.innerHTML = `
    <div class="login-wrap">
      <div class="login-card">
        <h1>FinBook 财务管理系统</h1>
        <div class="sub">${esc(status.company || "Web 版")}</div>
        ${banner}
        <form id="login-form">
          ${bookOpts}
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
    const sel = $("#login-book");
    const book_key = sel ? sel.value : "";
    try {
      const r = await api("/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username, password, device_id: deviceId(), device_name: deviceName(), book_key }),
      });
      session.user = r.user;
      toast(r.setup ? `已创建管理员账号「${esc(username)}」` : `欢迎，${esc(r.user.display_name)}`, "ok");
      await afterLogin();
      if (r.must_change_pwd) openChangePwd(true);
    } catch (err) {
      $("#login-err").textContent = err.message;
    }
  });
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
const NAV_ITEMS = [
  { id: "dashboard", label: "仪表盘", perm: null },
  { id: "vouchers", label: "记账凭证", perm: "voucher_new" },
  { id: "invoices", label: "发票管理", perm: "report" },
  { id: "imports", label: "数据导入", perm: "voucher_new" },
  { id: "ledger", label: "明细账", perm: "report" },
  { id: "reports", label: "报表中心", perm: "report" },
  { id: "multi-column", label: "多栏账", perm: "report" },
  { id: "summary-table", label: "摘要汇总表", perm: "report" },
  { id: "ratios", label: "财务指标", perm: "report" },
  { id: "mrp", label: "MRP 运算", perm: "account_edit" },
  { id: "routing", label: "工艺路线", perm: "account_edit" },
  { id: "approval", label: "审批中心", perm: "report" },
  { id: "notes", label: "报表附注", perm: "report" },
  { id: "archive", label: "电子档案", perm: "report" },
  { id: "budget-versions", label: "预算版本", perm: "report" },
  { id: "work-report", label: "工序报工", perm: "account_edit" },
  { id: "security", label: "安全中心", perm: "user_manage" },
];

// 视图注册表：id → 渲染函数（函数声明已提升，可在顶层引用）
const VIEWS = {
  "dashboard": viewDashboard,
  "vouchers": viewVouchers,
  "invoices": viewInvoices,
  "imports": viewImports,
  "ledger": viewLedger,
  "reports": viewReports,
  "multi-column": viewMultiColumn,
  "summary-table": viewSummaryTable,
  "ratios": viewRatios,
  "mrp": viewMrp,
  "routing": viewRouting,
  "approval": viewApproval,
  "notes": viewNotes,
  "archive": viewArchive,
  "budget-versions": viewBudgetVersions,
  "work-report": viewWorkReport,
  "security": viewSecurity,
};

let shellBuilt = false;

// 骨架只渲染一次；切换视图只更新 .main，不再重建 topbar/sidebar
function renderShell() {
  const u = session.user;
  const app = document.getElementById("app");
  const nav = NAV_ITEMS.filter((n) => !n.perm || can(n.perm));
  const periodOpts = state.periods.map((p) => `<option value="${p}" ${p === state.current ? "selected" : ""}>${p}</option>`).join("");
  app.innerHTML = `
    <div class="app">
      <div class="topbar">
        <span class="logo">FinBook</span>
        <span class="muted">${esc(u.display_name)}（${esc(u.role_label)}）</span>
        <select id="period-sel" title="会计期间">${periodOpts}</select>
        <span class="grow"></span>
        <button class="btn ghost sm" id="change-pwd">修改口令</button>
        <button class="btn ghost sm" id="logout">退出登录</button>
      </div>
      <div class="sidebar">
        ${nav.map((n) => `<button class="nav-item ${n.id === state.view ? "active" : ""}" data-view="${n.id}">${n.label}</button>`).join("")}
      </div>
      <div class="main" id="main"></div>
    </div>`;

  // 事件委托：nav-item 只在 sidebar 容器上绑一次
  $(".sidebar").addEventListener("click", (e) => {
    const btn = e.target.closest(".nav-item");
    if (!btn) return;
    state.view = btn.dataset.view;
    $all(".nav-item").forEach((b) => b.classList.toggle("active", b.dataset.view === state.view));
    renderMain();
  });
  $("#period-sel").addEventListener("change", async (e) => {
    state.current = e.target.value;
    try { await api("/period", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ymm: ymm(e.target.value) }) }); } catch (err) {}
    renderMain();
  });
  $("#logout").addEventListener("click", logout);
  $("#change-pwd").addEventListener("click", () => openChangePwd(false));
  shellBuilt = true;
}

function render() {
  if (!session.user) { shellBuilt = false; showLogin(); return; }
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
        <option value="draft">草稿</option><option value="audited">已审核</option>
        <option value="posted">已记账</option><option value="void">已作废</option>
      </select>
      <button class="btn ghost sm" id="v-refresh">查询</button>
      <span class="spacer"></span>
      <span class="muted">期间：${esc(state.current || "")}</span>
    </div>
    <div class="panel"><table class="grid" id="v-table"><thead><tr>
      <th>期间</th><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>状态</th><th>制单</th><th></th>
    </tr></thead><tbody><tr><td colspan="9" class="muted">加载中…</td></tr></tbody></table></div>`;
  if (can("voucher_new")) $("#new-v").addEventListener("click", () => openVoucherEditor(null));
  $("#v-refresh").addEventListener("click", () => loadVouchers());
  $("#v-q").addEventListener("keydown", (e) => { if (e.key === "Enter") loadVouchers(); });
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
  try { rows = await api(url); } catch (e) { tb.innerHTML = `<tr><td colspan="9" style="color:var(--err)">${esc(e.message)}</td></tr>`; return; }
  if (!rows.length) { tb.innerHTML = `<tr><td colspan="9" class="muted">暂无凭证</td></tr>`; return; }
  const stMap = { draft: ["草稿", "warn"], audited: ["已审核", ""], posted: ["已记账", "ok"], void: ["已作废", "err"] };
  tb.innerHTML = rows.map((v) => {
    const s = stMap[v.status] || [v.status_label, ""];
    return `<tr>
      <td>${esc(v.period)}</td><td>${esc(v.date)}</td><td>${esc(v.voucher_no)}</td>
      <td>${esc(v.summary)}</td><td class="num">${esc(v.debit_total)}</td><td class="num">${esc(v.credit_total)}</td>
      <td><span class="tag ${s[1]}">${esc(s[0])}</span></td><td>${esc(v.prepared_by)}</td>
      <td class="row-actions"><button class="btn ghost sm" data-edit="${v.id}">打开</button></td>
    </tr>`;
  }).join("");
  $all("[data-edit]").forEach((b) => b.addEventListener("click", () => openVoucherEditor(parseInt(b.dataset.edit, 10))));
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
  const editable = (id === 0) || status === "draft";
  const mask = modal(`
    <h3>记账凭证 ${esc(voucher_no)}</h3>
    <div class="toolbar">
      <label>日期 <input id="v-date" type="date" value="${esc(v.date)}" ${editable ? "" : "disabled"} /></label>
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
      ${can("voucher_audit") && status === "draft" ? `<button class="btn" id="v-audit">审核</button>` : ""}
      ${can("voucher_unaudit") && status === "audited" ? `<button class="btn ghost" id="v-unaudit">反审核</button>` : ""}
      ${can("voucher_post") && status === "audited" ? `<button class="btn" id="v-post">记账</button>` : ""}
      ${can("voucher_delete") && status === "draft" ? `<button class="btn danger" id="v-del">删除</button>` : ""}
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
  if ($("#v-audit", mask)) $("#v-audit", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/audit`, { method: "POST" }); toast("已审核", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-unaudit", mask)) $("#v-unaudit", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/unaudit`, { method: "POST" }); toast("已反审核", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-post", mask)) $("#v-post", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/post`, { method: "POST" }); toast("已记账", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-del", mask)) $("#v-del", mask).onclick = async () => { if (!confirm("确定删除该凭证？")) return; try { await api(`/vouchers/${v.id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
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
        if (!confirm("确定作废该发票？")) return;
        const up = await api(`/invoices/${id}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: "rejected" }) });
        if (up) { toast("已作废", "ok"); renderInvoices(main, await loadInvoices()); }
      } else if (act === "del") {
        if (!confirm("确定删除该发票？")) return;
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
    </div>
    <div class="panel"><table class="grid" id="l-table"><thead><tr>
      <th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th>
    </tr></thead><tbody><tr><td colspan="7" class="muted">请输入科目后查询</td></tr></tbody></table></div>`;
  $("#l-go").addEventListener("click", loadLedger);
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
      ${can("export") ? `<button class="btn ghost sm" id="r-export">导出 CSV</button>` : `<span class="tag warn" title="无导出权限">无导出权限，仅可打印</span>`}
      <button class="btn ghost sm" id="r-print">打印预览</button>
    </div>
    <div class="panel"><table class="grid" id="r-table"><thead><tr>
      <th>科目编码</th><th>科目名称</th><th>方向</th><th class="num">期初</th><th class="num">本期借方</th><th class="num">本期贷方</th><th class="num">期末</th><th class="num">本年累计借方</th><th class="num">本年累计贷方</th>
    </tr></thead><tbody><tr><td colspan="9" class="muted">点击「生成科目余额表」</td></tr></tbody></table></div>`;
  $("#r-go").addEventListener("click", loadTrial);
  $("#r-print").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.open(`/api/reports/trial-balance/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  if (can("export")) $("#r-export").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.location = `/api/reports/trial-balance/export?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`; });
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
// 安全中心（用户管理）
// ===========================================================================
async function viewSecurity(main) {
  main.innerHTML = `
    <h2>安全中心</h2>
    <div class="panel">
      <div style="display:flex;gap:14px;align-items:center;flex-wrap:wrap">
        <button class="btn sm" id="me-pwd">修改我的口令</button>
        ${can("user_manage") ? `<button class="btn sm" id="new-user">新建用户</button>` : ""}
      </div>
    </div>
    ${can("user_manage") ? `<div class="panel"><table class="grid" id="u-table"><thead><tr>
      <th>账号</th><th>姓名</th><th>角色</th><th>绑定设备</th><th>状态</th><th></th>
    </tr></thead><tbody><tr><td colspan="6" class="muted">加载中…</td></tr></tbody></table></div>` : `<div class="panel muted">您没有用户管理权限，仅可修改自己的口令。</div>`}`;
  $("#me-pwd").addEventListener("click", () => openChangePwd(false));
  if (can("user_manage")) { $("#new-user").addEventListener("click", openNewUser); loadUsers(); }
}
async function loadUsers() {
  const tb = $("#u-table tbody");
  let users;
  try { users = await api("/users"); } catch (e) { tb.innerHTML = `<tr><td colspan="6" style="color:var(--err)">${esc(e.message)}</td></tr>`; return; }
  if (!users.length) { tb.innerHTML = `<tr><td colspan="6" class="muted">暂无用户</td></tr>`; return; }
  tb.innerHTML = users.map((u) => {
    // role_label 由后端提供（Role::label），避免前端硬编码与角色扩展脱节
    const roleLabel = u.role_label || u.role;
    const dev = u.device_name ? `<span class="tag">${esc(u.device_name)}</span>` : `<span class="muted">未绑定</span>`;
    const dis = u.disabled ? `<span class="tag err">已停用</span>` : `<span class="tag ok">启用</span>`;
    const me = u.username === session.user.username;
    return `<tr>
      <td>${esc(u.username)}</td><td>${esc(u.display_name)}</td><td>${esc(roleLabel)}</td>
      <td>${dev}</td><td>${dis}</td>
      <td class="row-actions">
        <button class="btn ghost sm" data-reset-pwd="${esc(u.username)}">重置口令</button>
        <button class="btn ghost sm" data-reset-dev="${esc(u.username)}">重置设备</button>
        ${me ? "" : `<button class="btn ghost sm" data-toggle="${esc(u.username)}" data-next="${u.disabled ? "0" : "1"}">${u.disabled ? "启用" : "停用"}</button>
        <button class="btn danger sm" data-del="${esc(u.username)}">删除</button>`}
      </td>
    </tr>`;
  }).join("");
  $all("[data-reset-pwd]").forEach((b) => b.onclick = () => openAdminResetPwd(b.dataset.resetPwd));
  $all("[data-toggle]").forEach((b) => b.onclick = async () => {
    const dis = b.dataset.next === "1";
    if (dis && !confirm(`停用 ${b.dataset.toggle}？其全部会话将被立即下线。`)) return;
    try {
      await api(`/users/${encodeURIComponent(b.dataset.toggle)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ disabled: dis }) });
      toast(dis ? "已停用" : "已启用", "ok"); loadUsers();
    } catch (e) { toast(e.message, "err"); }
  });
  $all("[data-reset-dev]").forEach((b) => b.onclick = async () => { if (!confirm(`重置 ${b.dataset.resetDev} 的设备绑定？该账号可在新设备重新登录。`)) return; try { await api(`/users/${encodeURIComponent(b.dataset.resetDev)}/reset-device`, { method: "POST" }); toast("已重置设备绑定", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
  $all("[data-del]").forEach((b) => b.onclick = async () => { if (!confirm(`删除用户 ${b.dataset.del}？`)) return; try { await api(`/users/${encodeURIComponent(b.dataset.del)}`, { method: "DELETE" }); toast("已删除", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
}

function openNewUser() {
  const roles = [["admin", "系统管理员"], ["supervisor", "财务主管"], ["accountant", "会计"], ["cashier", "出纳"], ["auditor", "审核人"], ["viewer", "只读"]];
  const mask = modal(`
    <h3>新建用户</h3>
    <div class="field"><label>账号</label><input id="nu-u" /></div>
    <div class="field"><label>姓名</label><input id="nu-n" /></div>
    <div class="field"><label>角色</label><select id="nu-r">${roles.map((r) => `<option value="${r[0]}">${r[1]}</option>`).join("")}</select></div>
    <div class="field"><label>初始口令（至少 6 位）</label><input id="nu-p" type="password" /></div>
    <div class="foot"><button class="btn" id="nu-save">创建</button><button class="btn ghost" id="nu-cancel">取消</button></div>`);
  $("#nu-cancel", mask).onclick = closeModal;
  $("#nu-save", mask).onclick = async () => {
    const body = { username: $("#nu-u", mask).value.trim(), display_name: $("#nu-n", mask).value.trim(), password: $("#nu-p", mask).value, role: $("#nu-r", mask).value };
    if (!body.username || body.password.length < 6) { toast("账号必填且口令至少 6 位", "err"); return; }
    try { await api("/users", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已创建用户", "ok"); closeModal(); loadUsers(); } catch (e) { toast(e.message, "err"); }
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
function openChangePwd(forced) {
  const mask = modal(`
    <h3>${forced ? "首次登录：请设置您的管理员口令" : "修改口令"}</h3>
    ${forced ? `<div class="banner unset" style="margin-bottom:12px">为安全起见，建议设置一个强度较高的口令。</div>` : ""}
    <div class="field"><label>原口令</label><input id="cp-o" type="password" ${forced ? "placeholder='首次登录可留空'" : ""} /></div>
    <div class="field"><label>新口令（至少 6 位）</label><input id="cp-n" type="password" /></div>
    <div class="field"><label>确认新口令</label><input id="cp-c" type="password" /></div>
    <div class="foot"><button class="btn" id="cp-save">保存</button><button class="btn ghost" id="cp-cancel">取消</button></div>`);
  if (!forced) $("#cp-cancel", mask).onclick = closeModal;
  $("#cp-save", mask).onclick = async () => {
    const oldp = $("#cp-o", mask).value, np = $("#cp-n", mask).value, cp = $("#cp-c", mask).value;
    if (np.length < 6) { toast("新口令至少 6 位", "err"); return; }
    if (np !== cp) { toast("两次输入不一致", "err"); return; }
    try { await api("/change-password", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ old: oldp, new: np }) }); toast("口令已更新", "ok"); closeModal(); } catch (e) { toast(e.message, "err"); }
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
    </div>
    <div id="mc-result" class="muted">填写条件后点击查询</div>`;
  $("#mc-run").addEventListener("click", async () => {
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
    </div>
    <div id="st-result" class="muted">填写期间后点击查询</div>`;
  $("#st-run").addEventListener("click", async () => {
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
    </div>
    <div id="rt-result" class="muted">填写期间后点击查询</div>`;
  $("#rt-run").addEventListener("click", async () => {
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
      $all("[data-nt-del]").forEach((b) => b.onclick = async () => { if (!confirm("删除该附注？")) return; try { await api(`/reports/notes/${b.dataset.ntDel}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
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
      $all("[data-bv-del]").forEach((b) => b.onclick = async () => { if (!confirm(`删除版本 ${b.dataset.bvDel} 及其全部预算数据？`)) return; try { await api(`/budget/versions/${encodeURIComponent(b.dataset.bvDel)}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
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
      $all("[data-wr-finish]").forEach((b) => b.onclick = async () => { if (!confirm("将该工序标记为完工？")) return; try { await api("/prod/op/finish", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ op_id: parseInt(b.dataset.wrFinish, 10) }) }); toast("已完工", "ok"); loadOps(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#wr-load").addEventListener("click", loadOps);
}

// 启动
showLogin();
