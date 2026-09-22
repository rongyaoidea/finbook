const { expect } = require("@playwright/test");

/// 登录 → 建账（起始期间 2026-01）→ 停在可用界面
async function newBook(page, company) {
  await page.goto("/");
  await expect(page.locator("#u")).toBeVisible({ timeout: 15_000 });
  await page.fill("#u", "admin");
  await page.fill("#p", "Admin!2026");
  await page.click('#login-form button[type="submit"]');
  await expect(page.locator("#new-book")).toBeVisible({ timeout: 15_000 });
  await page.click("#new-book");
  await page.fill("#cb-company", company);
  await page.fill("#cb-start", "2026-01");
  await page.click("#cb-save");
  await expect(page.locator('.nav-item[data-view="vouchers"]')).toBeVisible({ timeout: 15_000 });
}

/// 录一张凭证并保存（不记账）。rows: [{ code, summary, debit, credit, aux? }]
async function postVoucher(page, { date, rows }) {
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
}

module.exports = { newBook, postVoucher };
