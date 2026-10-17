import { expect, type APIRequestContext, type Page } from "@playwright/test";

export const BASE = "http://127.0.0.1:18109";

export async function setScenario(request: APIRequestContext, name: string): Promise<void> {
  const response = await request.post(`${BASE}/__fake/scenario`, { data: { name } });
  expect(response.ok()).toBeTruthy();
  expect(await response.json()).toEqual({ ok: true });
}

export async function signIn(
  page: Page,
  { changePassword = true, password = "changed-password-1" } = {},
): Promise<void> {
  await page.fill("#login-user", "admin");
  await page.fill("#login-pass", "initial-password-1");
  await page.locator("form.login-form button[type=submit]").click();
  await expect(page.locator("#login-user")).toHaveCount(0);
  if (!changePassword) return;
  await expect(page.locator("#pass-new")).toBeVisible();
  await page.fill("#pass-new", password);
  await page.fill("#pass-repeat", password);
  await page.locator(".password-form button[type=submit]").click();
  await expect(page.locator("#pass-new")).toHaveCount(0);
  await expect(page.locator("#login-user")).toHaveCount(0);
}
