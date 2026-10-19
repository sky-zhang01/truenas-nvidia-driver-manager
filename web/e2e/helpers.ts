import { expect, type APIRequestContext, type Page } from "@playwright/test";

export const BASE = "http://127.0.0.1:18109";
export const PASSWORD = "changed-password-1";

export async function setupCode(request: APIRequestContext): Promise<string> {
  const response = await request.get(`${BASE}/__fake/setup-code`);
  expect(response.ok()).toBeTruthy();
  return (await response.json() as { code: string }).code;
}

export async function setScenario(request: APIRequestContext, name: string, { setup = true } = {}): Promise<void> {
  const response = await request.post(`${BASE}/__fake/scenario`, { data: { name } });
  expect(response.ok()).toBeTruthy();
  expect(await response.json()).toEqual({ ok: true });
  if (setup) {
    const login = await request.get(`${BASE}/api/login`);
    const state = await login.json() as { credentials?: string };
    if (state.credentials === "setup_required") {
      const created = await request.post(`${BASE}/api/setup`, {
        headers: { Origin: BASE }, data: { code: await setupCode(request), new_password: PASSWORD },
      });
      expect(created.status()).toBe(200);
    }
  }
}

export async function signIn(page: Page, { password = PASSWORD } = {}): Promise<void> {
  await expect(page.locator("#login-user, #setup-code")).toHaveCount(1);
  if (await page.locator("#setup-code").count()) {
    await page.fill("#setup-code", await setupCode(page.request));
    await page.fill("#setup-password", password);
    await page.fill("#setup-repeat", password);
    await page.locator(".setup-form button[type=submit]").click();
  } else {
    await page.fill("#login-user", "admin");
    await page.fill("#login-pass", PASSWORD);
    await page.locator("form.login-form button[type=submit]").click();
    await expect(page.locator("#login-user")).toHaveCount(0);
    if (password !== PASSWORD) {
      await page.goto("/#/password");
      await page.fill("#pass-new", password);
      await page.fill("#pass-repeat", password);
      await page.locator(".password-form button[type=submit]").click();
      await expect(page.locator("#pass-new")).toHaveCount(0);
    }
  }
  await expect(page.locator("#setup-code, #login-user")).toHaveCount(0);
}
