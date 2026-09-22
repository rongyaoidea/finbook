const { test, expect } = require("@playwright/test");

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

async function postVoucher(page, { date, rows }) {
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click("#new-v");
  await page.fill("#v-date", date);
  const entryRows = page.locator("#v-entries tbody tr:has(select.acct-sel)");
  for (let i = 0; i < rows.length; i++) {
    await entryRows.nth(i).locator("select.acct-sel").selectOption(rows[i].code);
    await entryRows.nth(i).locator(".e-sum").fill(rows[i].summary);
    if (rows[i].debit) await entryRows.nth(i).locator(".e-d").fill(rows[i].debit);
    if (rows[i].credit) await entryRows.nth(i).locator(".e-c").fill(rows[i].credit);
  }
  await page.click("#v-save");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });
}

test("固定资产：建卡→计提折旧→台账出数", async ({ page }) => {
  await newBook(page, `E2E固资${Date.now()}`);
  await page.click('.nav-item[data-view="assets"]');
  await page.click("#as-new");
  await page.fill("#ae-code", "SB0001");
  await page.fill("#ae-name", "测试设备");
  await page.fill("#ae-orig", "12000");
  await page.click("#ae-save");
  await expect(page.locator("#as-table")).toContainText("SB0001", { timeout: 10_000 });

  await page.click("#as-accrue");
  await page.click("#cf-ok");
  await expect(page.locator("#as-summary")).toContainText("本期已计提记录 1 条", { timeout: 10_000 });
  // 12000 × 95% / 36 = 316.67
  await expect(page.locator("#as-table")).toContainText("316.67");
  await expect(page.locator("#as-table")).toContainText("11,683.33");
});

test("银行对账：导入对账单→自动勾对→余额调节表一致", async ({ page }) => {
  await newBook(page, `E2E银行${Date.now()}`);
  await postVoucher(page, {
    date: "2026-01-10",
    rows: [
      { code: "100201", summary: "收到货款", debit: "500" },
      { code: "1001", summary: "收到货款", credit: "500" },
    ],
  });

  await page.click('.nav-item[data-view="bank"]');
  await page.fill("#bk-acct", "100201");
  await page.click("#bk-load");
  await page.click("#bk-import");
  await page.fill("#bi-text", "2026-01-10,收到货款,SN100,500.00,0.00,500.00");
  await page.click("#bi-ok");
  await expect(page.locator("#bk-sum")).toContainText("银行流水 1 条", { timeout: 10_000 });

  await page.click("#bk-auto");
  await expect(page.locator("#bk-sum")).toContainText("已勾 1", { timeout: 10_000 });
  await expect(page.locator("#bk-recon")).toContainText("调节后一致");
});

test("审核环节：启用后未审核不能记账，审核后可记账", async ({ page }) => {
  await newBook(page, `E2E审核${Date.now()}`);
  await page.click('.nav-item[data-view="options"]');
  await page.check("#op-audit");
  await page.click("#op-save");

  await postVoucher(page, {
    date: "2026-01-12",
    rows: [
      { code: "1001", summary: "借备用金", debit: "100" },
      { code: "2001", summary: "借备用金", credit: "100" },
    ],
  });

  const list = await (await page.request.get("/api/vouchers?period=202601")).json();
  const id = list[0].id;

  let resp = await page.request.post(`/api/vouchers/${id}/post`);
  expect(resp.status(), "未审核直接记账应被拒").toBe(400);

  await page.locator("#v-table tbody [data-edit]").first().click();
  await page.click("#v-audit");
  await expect(page.locator("#v-table tbody")).toContainText("已审核", { timeout: 10_000 });

  resp = await page.request.post(`/api/vouchers/${id}/post`);
  expect(resp.status(), "审核后记账应成功").toBe(200);

  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#v-table tbody")).toContainText("已记账", { timeout: 10_000 });
});
