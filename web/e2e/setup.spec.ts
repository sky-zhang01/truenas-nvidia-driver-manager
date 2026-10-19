import { test, expect, type Page, type APIRequestContext } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { BASE, PASSWORD, setScenario, setupCode, signIn } from "./helpers.js";

async function fresh(request: APIRequestContext, page: Page): Promise<void> {
  await setScenario(request, "fresh", { setup: false });
  await page.goto("/");
  await page.locator("#lang-control").selectOption("en");
  await expect(page.getByRole("heading", { name: "Set up the administrator" })).toBeVisible();
}

async function fill(page: Page, code: string): Promise<void> {
  await page.getByLabel("Setup code", { exact: true }).fill(code);
  await page.getByLabel("New password", { exact: true }).fill(PASSWORD);
  await page.getByLabel("Repeat password", { exact: true }).fill(PASSWORD);
}

async function submit(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Create administrator", exact: true }).click();
}

test("first start explains the verified TrueNAS log path and creates an administrator directly", async ({ page, request }) => {
  await fresh(request, page);
  await expect(page.locator(".setup-steps li")).toHaveCount(3);
  await expect(page.locator(".setup-steps")).toContainText("Apps → Installed");
  await expect(page.locator(".setup-steps")).toContainText("Workloads");
  await expect(page.locator(".setup-steps")).toContainText("View Logs");
  await expect(page.getByLabel("Administrator", { exact: true })).toHaveValue("admin");
  await expect(page.getByLabel("Administrator", { exact: true })).toHaveAttribute("readonly");
  await expect(page.locator("details.login-help")).not.toHaveAttribute("open");
  await page.locator("details.login-help summary").click();
  await expect(page.locator("details.login-help")).toContainText("After 24 hours, opening this page or submitting setup");
  await signIn(page);
  await expect(page.locator(".readings")).toBeVisible();
  await expect(page.locator("#pass-new")).toHaveCount(0);
  await expect(page).toHaveURL(/#\/home$/);
  await page.goto("/#/password");
  await expect(page.locator(".ssh-keys-add")).toBeVisible();
});

test("wrong codes preserve passwords, clear and focus the code and show help after three failures", async ({ page, request }) => {
  await fresh(request, page);
  for (let attempt = 0; attempt < 3; attempt += 1) {
    await fill(page, "0000-0000-0000-0000");
    await submit(page);
    await expect(page.locator("#setup-code-error")).toContainText("This code does not match");
    await expect(page.locator("#setup-code")).toHaveValue("");
    await expect(page.locator("#setup-code")).toBeFocused();
    await expect(page.locator("#setup-password")).toHaveValue(PASSWORD);
    await expect(page.locator("#setup-repeat")).toHaveValue(PASSWORD);
  }
  await expect(page.locator("#setup-code-error")).toContainText("old code no longer works");
  await page.locator("#setup-code").fill(await setupCode(request));
  await submit(page);
  await expect(page.locator(".readings")).toBeVisible();
});

test("expired codes renew during submission and explain where to find the replacement", async ({ page, request }) => {
  await fresh(request, page);
  const expired = await setupCode(request);
  expect((await request.post(`${BASE}/__fake/setup-code/expire`)).ok()).toBeTruthy();
  await fill(page, expired);
  await submit(page);
  await expect(page.locator("#setup-code-error")).toContainText("That code has expired. A new code is now in the logs.");
  const renewed = await setupCode(request);
  expect(renewed).not.toBe(expired);
  await page.locator("#setup-code").fill(renewed);
  await submit(page);
  await expect(page.locator(".readings")).toBeVisible();
});

test("a weak password keeps the setup code valid and presents the specific password rule", async ({ page, request }) => {
  await fresh(request, page);
  const code = await setupCode(request);
  await fill(page, code);
  await page.getByLabel("New password", { exact: true }).fill("short");
  await page.getByLabel("Repeat password", { exact: true }).fill("short");
  await submit(page);
  await expect(page.locator("#setup-error")).toContainText("The new password must have at least 8 characters.");
  await expect(page.locator("#setup-password")).toBeFocused();
  await expect(page.locator("#setup-password")).toHaveValue("short");
  await expect(page.locator("#setup-repeat")).toHaveValue("short");
  await expect(page.locator("#setup-code")).toHaveValue("");
  expect(await setupCode(request)).toBe(code);
  await fill(page, code);
  await submit(page);
  await expect(page.locator(".readings")).toBeVisible();
});

test("setup shares rate limiting and names the wait in seconds", async ({ page, request }) => {
  await fresh(request, page);
  for (let attempt = 0; attempt < 5; attempt += 1) {
    await fill(page, "0000-0000-0000-0000");
    await submit(page);
    await expect(page.locator("#setup-code-error")).toContainText("This code does not match");
  }
  await page.locator("#setup-code").fill(await setupCode(request));
  await submit(page);
  await expect(page.locator("#setup-error")).toContainText(/Try again in \d+ seconds/);
  await expect(page.locator("#setup-password")).toHaveValue(PASSWORD);
});

test("an existing account switches a stale setup page to sign-in", async ({ page, request }) => {
  await fresh(request, page);
  const code = await setupCode(request);
  const created = await request.post(`${BASE}/api/setup`, { headers: { Origin: BASE }, data: { code, new_password: PASSWORD } });
  expect(created.status()).toBe(200);
  await fill(page, code);
  await submit(page);
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page.locator(".app-main")).toContainText("An administrator already exists. Please sign in.");
});

test("a reset sends a stale sign-in page to setup and revokes the previous password", async ({ page, request }) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await page.locator("#lang-control").selectOption("en");
  expect((await request.post(`${BASE}/__fake/credentials/reset`)).ok()).toBeTruthy();
  await page.fill("#login-user", "admin");
  await page.fill("#login-pass", PASSWORD);
  await page.locator(".login-form button[type=submit]").click();
  await expect(page.locator("#setup-code")).toBeVisible();
  await expect(page.locator(".app-main")).not.toContainText("login.setup_required");
  await signIn(page);
  await expect(page.locator(".readings")).toBeVisible();
});

test("restart invalidates the previous setup code", async ({ page, request }) => {
  await fresh(request, page);
  const previous = await setupCode(request);
  expect((await request.post(`${BASE}/__fake/restart`)).ok()).toBeTruthy();
  const current = await setupCode(request);
  expect(current).not.toBe(previous);
  await fill(page, previous);
  await submit(page);
  await expect(page.locator("#setup-code-error")).toContainText("This code does not match");
  await page.locator("#setup-code").fill(current);
  await submit(page);
  await expect(page.locator(".readings")).toBeVisible();
});

test("the setup form can be completed using the keyboard and has screen-reader labels", async ({ page, request }) => {
  await fresh(request, page);
  const code = page.getByLabel("Setup code", { exact: true });
  await expect(code).toHaveAttribute("autocomplete", "off");
  await expect(code).toHaveAttribute("spellcheck", "false");
  await expect(code).toHaveAttribute("inputmode", "text");
  await expect(page.getByLabel("New password", { exact: true })).toHaveAttribute("aria-describedby", "setup-password-hint setup-error");
  await code.focus();
  await page.keyboard.type(await setupCode(request));
  await page.keyboard.press("Tab");
  await expect(page.getByLabel("Administrator", { exact: true })).toBeFocused();
  await page.keyboard.press("Tab");
  await page.keyboard.type(PASSWORD);
  await page.keyboard.press("Tab");
  await page.keyboard.type(PASSWORD);
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Create administrator" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator(".readings")).toBeVisible();
});

test("the setup page wraps and keeps controls reachable in both languages and all themes", async ({ page, request }) => {
  await fresh(request, page);
  for (const theme of ["focused-light", "focused-dark", "plate-light", "plate-dark"]) {
    await page.locator("#theme-control").selectOption(theme);
    for (const lang of ["en", "zh"]) {
      await page.locator("#lang-control").selectOption(lang);
      for (const width of [320, 1280]) {
        await page.setViewportSize({ width, height: 800 });
        await expect(page.locator("#setup-code")).toBeVisible();
        expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
        await page.locator(".setup-form button").scrollIntoViewIfNeeded();
        await expect(page.locator(".setup-form button")).toBeInViewport();
      }
    }
  }
});

test("the fake control code matches an actual SETUP CODE log message", async ({ page, request }) => {
  await fresh(request, page);
  const code = await setupCode(request);
  const logPath = process.env["NVM_E2E_SERVER_LOG"] ?? "/tmp/nvm-e2e-server.log";
  await expect.poll(async () => {
    const lines = (await readFile(logPath, "utf8")).split("\n");
    return lines.some(line => {
      try {
        const entry = JSON.parse(line) as { event?: string; msg?: string };
        return entry.event === "setup_code_issued" && entry.msg?.includes(`SETUP CODE: ${code}.`);
      } catch { return false; }
    });
  }).toBe(true);
});
