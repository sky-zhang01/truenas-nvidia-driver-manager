// Opt in with NVM_README_SHOTS=1. Uses only the local fake host and real UI.
import { test, expect, type Page } from "@playwright/test";
import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { setScenario, signIn } from "./helpers.js";

const OUTPUT = fileURLToPath(new URL("../../docs/images/", import.meta.url));
const SUCCESS_TASK = "20260921T141320Z-3f9a0c12";
const RUNNING_TASK = "20260928T101530Z-3f9a0c12";

test.use({
  viewport: { width: 1440, height: 900 },
  deviceScaleFactor: 2,
  colorScheme: "light",
  locale: "en-US",
  timezoneId: "UTC",
});

async function capture(page: Page, name: string, lang: string): Promise<void> {
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(() => window.scrollTo(0, 0));
  // The app and sidebar fill the viewport. Measure visible content instead so the
  // exported image keeps the header/navigation but drops the empty bottom area.
  const height = await page.evaluate(() => {
    const nodes = document.querySelectorAll(".app-header, .app-nav a, .app-main > *, .auth-card, .auth-shell > *, dialog[open]");
    return Math.ceil(Math.max(...Array.from(nodes, node => {
      const rect = node.getBoundingClientRect();
      return rect.width && rect.height ? rect.bottom + window.scrollY : 0;
    })) + 24);
  });
  expect(height).toBeGreaterThan(200);
  await page.screenshot({
    path: join(OUTPUT, `${name}-${lang}.png`),
    fullPage: true,
    clip: { x: 0, y: 0, width: 1440, height },
    animations: "disabled",
    caret: "hide",
  });
}

for (const lang of ["en", "zh"] as const) {
  test(`capture README screenshots in ${lang}`, async ({ page, request }) => {
    test.skip(process.env["NVM_README_SHOTS"] !== "1", "Set NVM_README_SHOTS=1 to regenerate README images");
    test.setTimeout(120_000);
    mkdirSync(OUTPUT, { recursive: true });
    // A screenshot run must never load external content, even if a page gains a URL.
    await page.route("**/*", route => new URL(route.request().url()).hostname === "127.0.0.1"
      ? route.continue() : route.abort());
    await page.addInitScript(({ language }) => {
      localStorage.setItem("nvm.lang", language);
      localStorage.setItem("nvm.theme", "focused-light");
    }, { language: lang });

    await setScenario(request, "fresh", { setup: false });
    await page.goto("/");
    await expect(page.locator("#setup-code")).toBeVisible();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-light");
    await page.fill("#setup-code", "7KQ2-M9XD-4TWB-HE6R");
    await capture(page, "setup", lang);

    await setScenario(request, "readme_landing");
    await page.goto("/");
    await signIn(page);
    await expect(page.locator(".readings")).toContainText("580.178.04");
    await expect(page.locator(".gpu-readout .reading-value")).toHaveCount(1);
    await expect(page.locator(".gpu-readout")).toContainText("GeForce RTX 4090");
    await expect(page.locator(".readings")).toContainText("580.173.02");
    await expect(page.locator(".app-main > .hint").first()).toContainText(/Succeeded|成功/);
    await expect(page.locator(".app-header")).toBeVisible();
    await expect(page.locator(".app-nav")).toBeVisible();
    await capture(page, "home-landing", lang);

    await page.goto(`/#/tasks/${SUCCESS_TASK}`);
    await expect(page.locator(".task-state")).toContainText(/Succeeded|成功/);
    await expect(page.locator(".task-terminal")).toContainText("580.178.04");
    await capture(page, "success", lang);

    await page.goto("/#/drivers");
    await expect(page.locator("table.drivers tbody tr")).toHaveCount(2);
    await expect(page.locator("table.drivers")).toContainText(/Original|原版/);
    await expect(page.locator("table.drivers")).toContainText(/In use|正在使用/);
    await capture(page, "drivers", lang);

    await setScenario(request, "readme_update");
    await page.goto("/");
    await signIn(page);
    await page.goto("/#/update");
    await expect(page.locator("#version-select option[value='580.178.04']")).toHaveCount(1);
    await page.selectOption("#version-select", "580.178.04");
    const check = page.getByRole("button", { name: /^(Check and continue|检查并继续)$/ });
    await expect(check).toBeEnabled();
    await capture(page, "update", lang);
    await check.click();
    const dialog = page.locator("dialog[open]");
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole("button", { name: /^(Save original and start|保存原版并开始)$/ })).toBeVisible();
    await capture(page, "plan", lang);
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);

    await setScenario(request, "readme_running");
    await page.goto("/");
    await signIn(page);
    await page.goto(`/#/tasks/${RUNNING_TASK}`);
    await expect(page.locator(".task-steps [aria-current='step']")).toBeVisible();
    await expect(page.locator(".log-tail")).not.toHaveText("");
    await capture(page, "running", lang);
  });
}
