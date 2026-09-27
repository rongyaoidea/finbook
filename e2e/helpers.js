const { expect } = require("@playwright/test");

// ---- 账套生命周期（E2E 专用数据目录，删干净不会碰到用户数据）----------------
//
// 踩过的坑：账套上限是**每账号 10 个**，撞上后 `POST /api/books` 全部 400，
// 于是所有用例都卡在「等不到主界面 nav」，**1 个真实失败被放大成整个套件失败**
// （实测 2 个真实失败能拖垮 18-31 个用例）。所以清理必须与进程状态无关。
//
// 早先靠模块级 `lastBookKey` 记录"本 worker 上次建的账套"，有两个洞：
// ① Playwright 的 `retries: 1` 会为重试**开新 worker 进程**，新进程变量是 null，
//    第一次尝试建的账套永远没人知道 key → 泄漏；
// ② 挂 `test.afterEach` 要改 16 个 spec 文件，漏一个就漏一个。
//
// 现在的做法：给每个 worker 的账套名加**固定前缀**（取自 TEST_WORKER_INDEX），
// 清理时按前缀扫。不依赖任何进程内状态，重试换进程照样能找回上次的账套。
// 只改这一个文件，16 个 spec 一个都不用动。
const WORKER_TAG = `w${process.env.TEST_WORKER_INDEX || "0"}`;
const BOOK_PREFIX = `E2E${WORKER_TAG}///`;

/// 删掉本 worker 名下所有账套（含上一次尝试泄漏的）
async function purgeOwnBooks(page) {
  let d;
  try {
    d = await (await page.request.get("/api/books")).json();
  } catch (e) {
    return; // 拿不到列表就跳过，不影响用例本身
  }
  for (const b of d.books || []) {
    if ((b.company || "").startsWith(BOOK_PREFIX)) {
      await page.request.delete(`/api/books/${encodeURIComponent(b.key)}`).catch(() => {});
    }
  }
}

/// 登录 → 建账（起始期间 2026-01）→ 停在可用界面
async function newBook(page, company) {
  await page.goto("/");
  await expect(page.locator("#u")).toBeVisible({ timeout: 15_000 });
  await page.fill("#u", "admin");
  await page.fill("#p", "Admin!2026");
  await page.click('#login-form button[type="submit"]');
  // 必须等登录真正完成（账套选择界面出现）再删旧账套：click 只保证点击已发出，
  // 立刻用 page.request 会因还没有会话 cookie 而 401。
  await expect(page.locator("#new-book")).toBeVisible({ timeout: 15_000 });

  await purgeOwnBooks(page);

  const name = `${BOOK_PREFIX}${company}`;
  await page.click("#new-book");
  await page.fill("#cb-company", name);
  await page.fill("#cb-start", "2026-01");
  await page.click("#cb-save");

  await expect(page.locator('.nav-item[data-view="vouchers"]')).toBeVisible({ timeout: 15_000 });
}

/// 审核并记账该期间全部草稿凭证。
///
/// 往来核销 / 账龄 / 催款只认**已记账**分录（全仓 H-3 口径，`settle::open_entries`
/// 取 `status='posted'`），所以凡是要验证核销/报表口径的用例，录完凭证必须记账，
/// 否则列表会是空的——那是正确行为，不是 bug。
///
/// 【为什么先审核再记账】新建账套的审核环节**默认开**（`BookOptions::enable_audit`，
/// 制单/审核/记账三权分离，见 fincore::account::BookOptions 文档），草稿直接记账
/// 会被后端拒（400「该账套启用了审核环节，请先审核凭证再记账」）。
///
/// 这里刻意走**真实生产流程**（审核 → 记账），而不是在 E2E 里把账套的审核关掉：
/// 关掉虽然能跑过，但那样 E2E 就测不到「默认要审核」这条新行为了，等于把回归网
/// 自己剪了。审核环节的默认值本身由 api.rs 的 audit_default_on_new_books 盯住。
async function postAllDrafts(page, period) {
  const r = await page.request.get(
    `/api/vouchers?period=${encodeURIComponent(period)}&status=draft&limit=200`
  );
  if (!r.ok()) throw new Error(`列草稿凭证失败：${r.status()}`);
  const list = await r.json();
  let n = 0;
  for (const v of list || []) {
    // 先审核（默认账套开着审核环节，未审核不能记账）
    const a = await page.request.post(`/api/vouchers/${v.id}/audit`, { data: {} });
    if (!a.ok()) throw new Error(`审核凭证 ${v.id} 失败：${a.status()} ${await a.text()}`);
    // 再记账
    const p = await page.request.post(`/api/vouchers/${v.id}/post`, { data: {} });
    if (!p.ok()) throw new Error(`记账凭证 ${v.id} 失败：${p.status()} ${await p.text()}`);
    n++;
  }
  return n;
}

/// 录一张凭证并保存。rows: [{ code, summary, debit, credit, aux? }]
/// opts.post=true 时保存后立即记账（核销/报表类用例必须）。
async function postVoucher(page, { date, rows, post = false }) {
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click("#new-v");
  await page.fill("#v-date", date);
  // 辅助面板展开会插入明细 tr，用 :has 只匹配分录行
  const entryRows = page.locator("#v-entries tbody tr:has(select.acct-sel)");
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i];
    await entryRows.nth(i).locator("select.acct-sel").selectOption(r.code);
    await entryRows.nth(i).locator(".e-sum").fill(r.summary);
    if (r.debit) await entryRows.nth(i).locator(".e-d").fill(r.debit);
    if (r.credit) await entryRows.nth(i).locator(".e-c").fill(r.credit);
    if (r.aux) {
      await entryRows.nth(i).locator(".e-aux").click();
      for (const [k, v] of Object.entries(r.aux)) {
        await page.fill(`.aux-in[data-k="${k}"]`, v);
      }
    }
  }
  await page.click("#v-save");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });
  if (post) {
    // date 形如 "2026-01-10" → 期间 202601
    await postAllDrafts(page, date.slice(0, 7).replace("-", ""));
  }
}

/// 批量审核该期间全部草稿凭证。
///
/// 新建账套的审核环节**默认开**（`BookOptions::enable_audit`，三权分离），草稿不能
/// 直接记账——`/post` 与 `/batch-post` 都会被 `vouchers::post` 的同一道闸门拒掉。
/// 配套 `postAllDrafts` 使用：先审核全部，再记账全部。
async function auditAllDrafts(page, period) {
  const r = await page.request.get(
    `/api/vouchers?period=${encodeURIComponent(period)}&status=draft&limit=200`
  );
  if (!r.ok()) throw new Error(`列草稿凭证失败：${r.status()}`);
  const list = await r.json();
  for (const v of list || []) {
    const a = await page.request.post(`/api/vouchers/${v.id}/audit`, { data: {} });
    if (!a.ok()) throw new Error(`审核凭证 ${v.id} 失败：${a.status()} ${await a.text()}`);
  }
  return (list || []).length;
}

module.exports = { newBook, postVoucher, postAllDrafts, auditAllDrafts };
