import { test, expect, type APIRequestContext, type Page } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

async function openConfirm(page: Page, request: APIRequestContext): Promise<void> {
  await setScenario(request, "pascal");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/update");
  await expect(page.locator("#version-select")).toBeVisible();
  await page.selectOption("#version-select", "580.178.04");
  await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
  const confirm = page.locator("dialog[open]");
  await expect(confirm).toBeVisible();
  return;
}

test("a rejected submit leaves one visible close entry, focused, and submits once", async ({
  page,
  request,
}) => {
  await openConfirm(page, request);
  const confirm = page.locator("dialog[open]");

  await expect(confirm.getByRole("button")).toHaveCount(3);

  const posts: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "POST" && req.url().endsWith("/api/tasks")) posts.push(req.url());
  });
  let reject = true;
  await page.route("**/api/tasks", async (route) => {
    if (route.request().method() !== "POST" || !reject) {
      await route.continue();
      return;
    }
    await route.fulfill({
      status: 409,
      contentType: "application/json",
      body: JSON.stringify({ error: "plan.changed", params: { changed: ["link"] } }),
    });
  });
  await confirm.getByRole("button", { name: /Save original driver and start|保存原版驱动并开始/ }).click();

  await expect(confirm.locator(".dialog-error")).toBeVisible({ timeout: 10_000 });
  await expect(confirm.locator("button:not([hidden])")).toHaveCount(1);
  const close = confirm.getByRole("button", { name: /^Close$|^关闭$/ });
  await expect(close).toBeVisible();
  await expect(close).toBeFocused();
  expect(posts.length).toBe(1);

  reject = false;
  await close.click();
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  expect(posts.length).toBe(1);
});
