import { test, expect, type Page } from "./local-test.js";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { setScenario, signIn } from "./helpers.js";

const CHECK = /^Check this version and continue$|^用这个版本检查并继续$/;
const CANCEL = /Cancel waiting|取消等待/;
const blocked = { result: "blocked", failures: [], gpu: { state: "busy", users: [], errors: [], waited_ms: 30000 } };
async function open(page: Page, language: string) {
  await page.setViewportSize({ width: 1280, height: 900 });
  await setScenario(page.request, "drivers_rows");
  await page.goto("/"); await signIn(page); await page.goto("/#/drivers");
  await page.locator("#lang-control").selectOption(language);
  const row = page.locator("table.drivers tbody tr").filter({ hasText: "580.178.04" });
  const update = row.getByRole("button", { name: CHECK });
  await expect(update).toBeEnabled();
  return { row, update };
}

for (const language of ["zh", "en"]) {
  test(`an active driver check locks redraw controls and keeps cancellation attached in ${language}`, async ({ page }) => {
    const { update } = await open(page, language);
    let release = (): void => {};
    const gate = new Promise<void>((done) => { release = done; });
    let posts = 0;
    await page.route("**/api/plans", async (route) => { posts++; await gate; await route.fulfill({ json: blocked }).catch(() => {}); });
    const activeButton = await update.elementHandle();
    try {
      await update.click(); await expect.poll(() => posts).toBe(1);
      const cancel = page.getByRole("button", { name: CANCEL });
      await expect(cancel).toBeVisible();
      const sorts = page.locator("table.drivers thead button");
      expect.soft(await sorts.evaluateAll((nodes) => nodes.map((node) => (node as HTMLButtonElement).disabled))).toEqual(Array(7).fill(true));
      expect.soft(await page.locator("#select-all").isDisabled()).toBe(true);
      expect.soft(await page.getByRole("switch").isDisabled()).toBe(true);
      expect.soft(await page.getByRole("button", { name: /^Delete selected$|^删除所选$/ }).isDisabled()).toBe(true);
      expect.soft(await page.locator("table.drivers").getByRole("button", { name: /^Delete$|^删除$/ }).evaluateAll((nodes) => nodes.every((node) => (node as HTMLButtonElement).disabled))).toBe(true);
      // A real pointer attempt on a disabled sort control must not replace the active row.
      await sorts.first().click({ force: true });
      expect.soft(await activeButton!.evaluate((node) => node.isConnected)).toBe(true);
      await expect.soft(cancel).toBeVisible();
      if (await cancel.count() === 0) return;
      if (process.env["NVM_REVIEW3_SHOTS"] === "1" && language === "zh") {
        const output = resolve(process.cwd(), "../.implementation/release-fixes/review3-shots");
        mkdirSync(output, { recursive: true });
        await cancel.scrollIntoViewIfNeeded();
        await page.screenshot({ path: resolve(output, "driver-check-controls-zh.png"), fullPage: true });
      }
      await cancel.click();
      await expect(update).toBeFocused();
      expect(await activeButton!.evaluate((node) => node.isConnected)).toBe(true);
      await expect(update).toBeEnabled();
      await expect(sorts.first()).toBeEnabled();
      await expect(page.getByRole("switch")).toBeEnabled();
      await expect(page.locator("#select-all")).toBeEnabled();
      expect(posts).toBe(1);
      if (process.env["NVM_REVIEW3_SHOTS"] === "1" && language === "zh") {
        await update.scrollIntoViewIfNeeded();
        await page.screenshot({ path: resolve(process.cwd(), "../.implementation/release-fixes/review3-shots/driver-check-cancelled-zh.png"), fullPage: true });
      }
    } finally { release(); }
  });

  test(`an in-flight keep write settles before a driver plan can start in ${language}`, async ({ page }) => {
    const { update } = await open(page, language);
    let releaseKeep = (): void => {}, releasePlan = (): void => {};
    const keepGate = new Promise<void>((done) => { releaseKeep = done; });
    const planGate = new Promise<void>((done) => { releasePlan = done; });
    let keeps = 0, plans = 0;
    await page.route("**/api/drivers/*/keep", async (route) => { keeps++; await keepGate; await route.continue().catch(() => {}); });
    await page.route("**/api/plans", async (route) => { plans++; await planGate; await route.fulfill({ json: blocked }).catch(() => {}); });
    try {
      await page.getByRole("switch").click();
      await expect.poll(() => keeps).toBe(1);
      expect.soft(await update.isDisabled()).toBe(true);
      await update.click({ force: true });
      await page.waitForTimeout(100);
      expect.soft(plans).toBe(0);
      if (plans > 0) return;
      releaseKeep();
      await expect(page.getByRole("switch")).not.toBeChecked();
      await expect(update).toBeEnabled();
      await update.click(); await expect.poll(() => plans).toBe(1);
      const cancel = page.getByRole("button", { name: CANCEL });
      await expect(cancel).toBeVisible(); await cancel.click();
      await expect(update).toBeFocused();
      expect(await update.evaluate((node) => node.isConnected)).toBe(true);
      await expect(update).toBeEnabled();
    } finally { releaseKeep(); releasePlan(); }
  });
}


test("a failed keep write and failed follow-up read retain protection and release controls", async ({ page }) => {
  const { row, update } = await open(page, "en");
  const switches = page.getByRole("switch");
  await expect(switches).toBeChecked();
  const controls = page.locator("table.drivers button, table.drivers input, #select-all").or(page.getByRole("button", { name: "Delete selected", exact: true }));
  const before = await controls.evaluateAll((nodes) => nodes.map((node) => (node as HTMLButtonElement | HTMLInputElement).disabled));
  let writes = 0, reads = 0;
  await page.route("**/api/drivers/*/keep", async (route) => {
    writes++;
    await route.fulfill({ status: 503, json: { error: "internal", params: {} } });
  });
  await page.route("**/api/drivers", async (route) => {
    reads++;
    await route.fulfill({ status: 503, json: { error: "internal", params: {} } });
  });
  await switches.click();
  await expect.poll(() => reads).toBe(1);
  await expect(page.locator(".table-error")).toContainText(/internal error/i);
  await expect(switches).toBeChecked();
  await expect(switches).toBeEnabled();
  const original = page.locator("table.drivers tbody tr").filter({ has: switches });
  await expect(original).toContainText("Kept. Turn off Keep to delete it.");
  await expect(original.getByRole("button", { name: "Delete", exact: true })).toBeDisabled();
  expect(await controls.evaluateAll((nodes) => nodes.map((node) => (node as HTMLButtonElement | HTMLInputElement).disabled))).toEqual(before);
  await expect(row).toContainText("580.178.04");
  let plans = 0;
  await page.route("**/api/plans", async (route) => { plans++; await route.fulfill({ json: blocked }); });
  await update.click();
  await expect.poll(() => plans).toBe(1);
  expect(writes).toBe(1);
});

test("successful deletion survives a failed follow-up read and restores available focus", async ({ page }) => {
  const { row } = await open(page, "en");
  const initialRows = await page.locator("table.drivers tbody tr").count();
  let reads = 0;
  let removed: string[] = [];
  await page.route("**/api/drivers/delete", async (route) => {
    const response = await route.fetch();
    expect(response.ok()).toBe(true);
    removed = ((await response.json()) as { deleted: string[] }).deleted;
    await route.fulfill({ response });
  });
  await page.route("**/api/drivers", async (route) => {
    reads++;
    await route.fulfill({ status: 503, json: { error: "internal", params: {} } });
  });
  await row.getByRole("button", { name: "Delete", exact: true }).click();
  await page.locator("dialog[open]").getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  await expect.poll(() => reads).toBe(1);
  expect(removed).toHaveLength(1);
  await expect(row).toHaveCount(0);
  await expect(page.locator("table.drivers tbody tr")).toHaveCount(initialRows - 1);
  await expect(page.locator(".table-error")).toContainText(/internal error/i);
  await expect(page.locator("#select-all")).toBeEnabled();
  await expect(page.locator("#select-all")).toBeFocused();
  await expect(page.locator("table.drivers thead button").first()).toBeEnabled();
  await expect(page.getByRole("switch")).toBeEnabled();
  await expect(page.getByRole("button", { name: "Delete selected", exact: true })).toBeEnabled();
  const update = page.locator("table.drivers tbody tr").filter({ hasText: "580.173.02" }).filter({ hasText: "Official build" }).getByRole("button", { name: CHECK });
  await expect(update).toBeEnabled();
  let plans = 0;
  await page.route("**/api/plans", async (route) => { plans++; await route.fulfill({ json: blocked }); });
  await update.click();
  await expect.poll(() => plans).toBe(1);
  await expect(row).toHaveCount(0);
});
