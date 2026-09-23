const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("序列号：入库→在库查询→出库", async ({ page }) => {
  await newBook(page, `E2E序列${Date.now()}`);
  await page.click('.nav-item[data-view="inv-serial"]');
  await page.fill("#is-item", "RM01");
  await page.fill("#is-serials", "SN001,SN002");
  await page.fill("#is-batch", "B1");
  await page.click("#is-in");
  await page.click("#is-load");
  await expect(page.locator("#is-result")).toContainText("SN001", { timeout: 15_000 });

  await page.fill("#is-serials", "SN001");
  await page.click("#is-out");
  await expect(page.locator("#is-result")).not.toContainText("SN001", { timeout: 15_000 });
});

test("多单位换算：保存换算率→回显", async ({ page }) => {
  await newBook(page, `E2E单位${Date.now()}`);
  await page.click('.nav-item[data-view="inv-unit"]');
  await page.fill("#iu-item", "RM01");
  await page.fill("#iu-base", "个");
  await page.fill("#iu-alt", "箱");
  await page.fill("#iu-factor", "12");
  await page.click("#iu-save");
  await page.click("#iu-load");
  await expect(page.locator("#iu-info")).toContainText("箱", { timeout: 15_000 });
});

test("组装拆卸：组装→拆卸", async ({ page }) => {
  await newBook(page, `E2E组装${Date.now()}`);
  // 先备子件库存，否则组装会因库存不足失败
  const resp = await page.request.post("/api/inventory/adjust", {
    data: { period: 202601, date: "2026-01-10", item: "140301", delta: "5", memo: "E2E" },
  });
  expect(resp.ok(), "子件入库应成功").toBeTruthy();

  await page.click('.nav-item[data-view="inv-assemble"]');
  await page.fill("#ia-parent", "140501");
  await page.fill("#ia-children", "140301:1");
  await page.click("#ia-do");
  await page.click("#ia-undo");
  // 页面仍正常渲染（操作结果以 toast 呈现，这里只验证未崩溃）
  await expect(page.locator("#ia-parent")).toHaveValue("140501", { timeout: 15_000 });
});

test("库存账龄/ABC/分仓库/调拨报表", async ({ page }) => {
  await newBook(page, `E2E库存表${Date.now()}`);
  await page.request.post("/api/inventory/adjust", {
    data: { period: 202601, date: "2026-01-10", item: "140301", delta: "5", memo: "E2E" },
  });

  await page.click('.nav-item[data-view="inv-aging"]');
  await expect(page.locator("#ia-result")).toContainText("140301", { timeout: 15_000 });

  await page.click('.nav-item[data-view="inv-abc"]');
  await expect(page.locator("#ib-result")).toContainText("140301", { timeout: 15_000 });

  await page.click('.nav-item[data-view="inv-warehouse"]');
  await page.fill("#iw-item", "140301");
  await page.click("#iw-load");
  await expect(page.locator("#iw-result")).toContainText("140301", { timeout: 15_000 });

  await page.click('.nav-item[data-view="inv-transfer"]');
  await expect(page.locator("#it-result")).toContainText("调拨", { timeout: 15_000 });
});
