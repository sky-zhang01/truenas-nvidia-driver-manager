import { test, expect } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

test("every input has a label and the error area is an alert", async ({ page, request }) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  const user = page.locator("#login-user");
  await expect(user).toHaveAttribute("id", "login-user");
  await expect(page.locator("label[for='login-user']")).toHaveCount(1);
  await expect(page.locator("label[for='login-pass']")).toHaveCount(1);

  await page.click("button[type=submit]");
  const alert = page.locator("[role=alert]");
  await expect(alert).toHaveCount(1);
});

test("the settings problems of the top notice area are rendered", async ({ page, request }) => {
  await setScenario(request, "setup_problem");
  await page.goto("/");
  await expect(page.locator(".top-notices .notice")).not.toHaveCount(0);
  await expect(page.locator(".top-notices .sr-only").first()).toHaveText(/Error|错误/);
});

test("the running task and the missing link use the catalog wording", async ({ page, request }) => {
  await setScenario(request, "running_task");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/home");
  await expect(page.locator(".task-state")).toHaveAttribute("role", "status");

  await setScenario(request, "no_link");
  // A new scenario recreates the credentials file, so the old cookie is stale: reload, sign in again
  // with the chosen password, then read home.
  await page.goto("/#/home");
  await page.reload();
  await signIn(page);
  await page.goto("/#/home");
  await expect(page.locator(".app-main")).toContainText(
    /not found|未检测到|Enable the NVIDIA driver|启用 NVIDIA 驱动/,
  );
});

test("the update page ties the select to its descriptions", async ({ page, request }) => {
  await setScenario(request, "pascal");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/update");
  const select = page.locator("#version-select");
  await expect(select).toHaveAttribute("aria-describedby", "catalog-describe");
  await expect(page.locator("#catalog-describe")).toContainText(/latest version of each branch that builds on this host|列表为各分支在本机可构建的最新版本/);
});
