const { test, expect } = require("@playwright/test");

test("辅助/数量凭证、附件上传与数量金额账", async ({ page }) => {
  const company = `E2E辅${Date.now()}`;

  // 登录 + 建账
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

  // 借 140301（存货辅助 + 数量核算）5×20=100 / 贷 1001 100
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click("#new-v");
  await page.fill("#v-date", "2026-01-15");
  const rows = page.locator("#v-entries tbody tr");
  await expect(rows.first()).toBeVisible();
  await rows.nth(0).locator("select.acct-sel").selectOption("140301");
  await rows.nth(0).locator(".e-sum").fill("E2E 入库");
  await rows.nth(0).locator(".e-d").fill("100");
  await rows.nth(0).locator(".e-aux").click();
  await page.fill('.aux-in[data-k="item"]', "RM01");
  await page.fill('.aux-in[data-k="qty"]', "5");
  await page.fill('.aux-in[data-k="price"]', "20");
  await rows.nth(1).locator("select.acct-sel").selectOption("1001");
  await rows.nth(1).locator(".e-sum").fill("E2E 付款");
  await rows.nth(1).locator(".e-c").fill("100");
  await page.click("#v-save");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });

  // 重新打开：上传附件
  await page.locator("#v-table tbody [data-edit]").first().click();
  await page.setInputFiles("#v-attfile", {
    name: "receipt.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("hello attachment"),
  });
  await page.click("#v-attup");
  await expect(page.locator("#v-attach")).toContainText("receipt.txt", { timeout: 10_000 });
  await page.click("#v-close");

  // 数量金额账能出数
  await page.click('.nav-item[data-view="reports"]');
  await page.click("#r-qty");
  await expect(page.locator("#r-qty-table")).toContainText("140301", { timeout: 10_000 });
  await expect(page.locator("#r-qty-table")).toContainText("5");

  // 导出接口在浏览器会话下可用
  const resp = await page.request.get("/api/export/vouchers?period=202601");
  expect(resp.status()).toBe(200);
  const csv = await resp.text();
  expect(csv).toContain("凭证号");
});
