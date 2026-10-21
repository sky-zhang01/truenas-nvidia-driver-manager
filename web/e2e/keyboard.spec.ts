import { test, expect } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

test("the update flow works with the keyboard only", async ({ page, request }) => {
  await setScenario(request, "pascal");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/update");
  await page.locator("a[href='#/update']").focus();
  await page.keyboard.press("Enter");

  const select = page.locator("#version-select");
  await expect(select).toBeVisible();
  await select.focus();
  // Headless Chromium does not move a closed select's selection with ArrowDown while selectedIndex is
  // -1; type-ahead is the keyboard path. The Pascal host lists 580.178.04.
  await select.press("5");
  const update = page.getByRole("button", { name: /Check and continue|检查并继续/ });
  await expect(update).toBeEnabled();
  await update.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("dialog[open]")).toBeVisible();
});

test("a dialog takes focus and returns it on Escape", async ({ page, request }) => {
  await setScenario(request, "mixed_1b80_2b85");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/update");
  const upload = page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ });
  await upload.focus();
  await page.keyboard.press("Enter");
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  await expect(page.locator(".dialog-title")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(upload).toBeFocused();
});

test("the local driver table is reachable with the keyboard", async ({ page, request }) => {
  await setScenario(request, "drivers_rows");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/drivers");
  const table = page.locator("table.drivers");
  await expect(table).toBeVisible();
  const sortButton = page.locator("th button").first();
  await sortButton.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("th[aria-sort='descending']")).toHaveCount(1);
});

test("the sign-in guidance opens and closes with the keyboard", async ({ page, request }) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  const summary = page.locator("details.login-help summary");
  await summary.waitFor({ state: "visible", timeout: 10_000 });

  await page.locator("#login-pass").focus();
  await page.keyboard.press("Tab"); // submit button
  await page.keyboard.press("Tab"); // guidance summary
  await expect(summary).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator("details.login-help")).toHaveAttribute("open");
  await expect(summary).toBeFocused();
  await page.keyboard.press("Space");
  await expect(page.locator("details.login-help")).not.toHaveAttribute("open");
  await page.keyboard.press("Space");
  await expect(page.locator("details.login-help")).toHaveAttribute("open");
});
