import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import { setScenario, signIn } from "./helpers.js";

async function openSession(page: Page, request: APIRequestContext): Promise<void> {
  await setScenario(request, "drivers_rows");
  await page.goto("/");
  await signIn(page);
}

async function openDrivers(page: Page, request: APIRequestContext): Promise<void> {
  await openSession(page, request);
  await page.goto("/#/drivers");
  await expect(page.locator("table.drivers")).toBeVisible();
}

const NAV_HOME = ".app-nav a[href='#/home']";
const ROW_CHECK = /Check this version and continue|用这个版本检查并继续/;

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Count the plan requests this page starts. */
function countPlans(page: Page): string[] {
  const posts: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "POST" && req.url().endsWith("/api/plans")) posts.push(req.url());
  });
  return posts;
}

test("the table shows seven sortable columns", async ({ page, request }) => {
  await openDrivers(page, request);
  const heads = page.locator("table.drivers thead th button");
  await expect(heads).toHaveCount(7);
  for (const label of [/Version|版本/, /Source|来源/, /Build date|构建日期/, /Size|大小/, /Target kernel|目标内核/, /Status|状态/, /Keep|保留/]) {
    await expect(page.locator("table.drivers thead")).toContainText(label);
  }
});

test("sorting is announced and reversible", async ({ page, request }) => {
  await openDrivers(page, request);
  const first = page.locator("table.drivers thead th button").first();

  await expect(page.locator("th[aria-sort='ascending']")).toHaveCount(1);
  await first.click();
  await expect(page.locator("th[aria-sort='descending']")).toHaveCount(1);
  await first.click();
  await expect(page.locator("th[aria-sort='ascending']")).toHaveCount(1);
});

test("an in-use row has no delete button", async ({ page, request }) => {
  await openDrivers(page, request);
  const inUseRow = page.locator("table.drivers tbody tr", { hasText: /In use|在用/ }).first();
  await expect(inUseRow).toHaveCount(1);
  await expect(inUseRow.getByRole("button", { name: /^Delete$|^删除$/ })).toHaveCount(0);
  await expect(inUseRow.locator("input[type=checkbox]").first()).toBeDisabled();
});

test("deleting asks for confirmation first", async ({ page, request }) => {
  await openDrivers(page, request);
  const row = page.locator("table.drivers tbody tr").filter({ has: page.getByRole("button", { name: /^Delete$|^删除$/ }) }).first();
  await row.getByRole("button", { name: /^Delete$|^删除$/ }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(".dialog-title")).toHaveText(/Delete local drivers|删除本地驱动/);
  const cancel = dialog.getByRole("button", { name: /^Cancel$|^取消$/ });
  await cancel.click();
  await expect(dialog).toBeHidden();
});

test("the keep switch is a switch and only original rows have one", async ({ page, request }) => {
  await openDrivers(page, request);
  const switches = page.locator("table.drivers tbody input[role=switch]");
  await expect(switches).toHaveCount(1);
  await expect(switches.first()).toBeEnabled();
});

test("a kernel-mismatch row shows kernel-mismatch notice under it", async ({ page, request }) => {
  await openDrivers(page, request);
  const mismatch = page.locator("table.drivers tbody tr.row-mismatch").first();
  await expect(mismatch).toHaveCount(1);
  await expect(mismatch.getByRole("button", { name: /Check this version and continue|用这个版本检查并继续/ })).toBeDisabled();
  await expect(page.locator("table.drivers tbody")).toContainText(/Unavailable: built for kernel|不可用:此驱动为内核/);
});

test("a late 401 of the drivers read does not end the session", async ({ page, request }) => {
  await openSession(page, request);
  await page.goto("/#/home");
  await expect(page.locator(NAV_HOME)).toBeVisible();

  let reads = 0;
  await page.route("**/api/drivers", async (route) => {
    reads += 1;
    await delay(1200);
    await route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ error: "auth.required", params: {} }),
    });
  });

  await page.locator(".app-nav a[href='#/drivers']").click();
  await page.locator(NAV_HOME).click();
  await page.waitForTimeout(2500);

  expect(reads).toBe(1);
  await expect(page.locator("#login-user")).toHaveCount(0);
});

test("a driver-row plan answer that lands after the page was left opens no dialog", async ({
  page,
  request,
}) => {
  await openDrivers(page, request);
  await page.route("**/api/plans", async (route) => {
    await delay(1200);
    await route.continue();
  });
  const rowUpdate = page.locator("table.drivers tbody button:enabled").filter({ hasText: ROW_CHECK });
  const enabled = rowUpdate.first();
  await expect(enabled).toBeEnabled();
  await enabled.click();
  await page.locator(NAV_HOME).click();
  await expect(page.locator("table.drivers")).toHaveCount(0);
  await page.waitForTimeout(2000);

  await expect(page.locator("dialog[open]")).toHaveCount(0);
});

test("a double click on a driver row sends one plan request", async ({ page, request }) => {
  await openDrivers(page, request);
  const posts = countPlans(page);
  await page.route("**/api/plans", async (route) => {
    await delay(1500);
    await route.continue();
  });
  const rowUpdate = page.locator("table.drivers tbody button:enabled").filter({ hasText: ROW_CHECK });
  const enabled = rowUpdate.first();
  await expect(enabled).toBeEnabled();
  await enabled.dblclick();
  await page.waitForTimeout(2200);

  expect(posts.length).toBe(1);
});
