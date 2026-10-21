import { test, expect, type Locator, type Page } from "./local-test.js";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { setScenario, signIn } from "./helpers.js";

const SHOTS = resolve(process.cwd(), "../.implementation/release-fixes/screenshots");
const PACKAGES = ["libnvidia-container1", "libnvidia-container-tools", "nvidia-container-toolkit-base", "nvidia-container-toolkit"];
const run = { name: "driver.run", mimeType: "application/octet-stream", buffer: Buffer.from('#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04"\n' + "x".repeat(4096)) };
const deb = (name: string, version = "1.20.1-1") => ({ name: `${name}.deb`, mimeType: "application/octet-stream", buffer: Buffer.from(`NVM_TEST_DEB\nPackage: ${name}\nVersion: ${version}\nArchitecture: amd64\n`) });
const snapshot = { state: "busy", checked_at: 1792209600000, users: [
  { id: "docker:active", name: "gpu-worker", type: "docker", usage: "in_use" },
  { id: "container:compute", name: "compute-instance", type: "incus", usage: "unknown" },
], errors: [] };

test.beforeEach(async ({ page, request }) => {
  // All test traffic stays on the local fake host, including accidental external links.
  await page.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.hostname === "127.0.0.1") await route.continue();
    else await route.abort();
  });
  await setScenario(request, "upload_files");
  await page.goto("/"); await signIn(page); await page.goto("/#/update");
  await page.locator("#theme-control").selectOption("focused-light");
  mkdirSync(SHOTS, { recursive: true });
});

async function open(page: Page, language: string, width = 1280): Promise<Locator> {
  await page.setViewportSize({ width, height: 900 });
  await page.locator("#lang-control").selectOption(language);
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog.upload-dialog[open]");
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  return dialog;
}

async function boxes(dialog: Locator) {
  return dialog.locator(".upload-footer > .dialog-buttons button").evaluateAll((buttons) => buttons.map((button) => {
    const { x, y, width, height } = button.getBoundingClientRect();
    return { x, y, width, height };
  }));
}

for (const language of ["zh", "en"]) for (const width of [1280, 390]) {
  test(`upload waiting preserves footer button geometry in ${language} at ${width}px`, async ({ page }, testInfo) => {
    const dialog = await open(page, language, width);
    await dialog.locator("#upload-files").setInputFiles([run, ...PACKAGES.map((name) => deb(name))]);
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
    await page.route("**/api/gpu-users", (route) => route.fulfill({ json: snapshot }));
    let release = (): void => {};
    const pending = new Promise<void>((done) => { release = done; });
    await page.route("**/api/plans", async (route) => {
      await pending;
      await route.fulfill({ json: { result: "blocked", failures: [], gpu: { ...snapshot, waited_ms: 30000 } } });
    });
    await page.clock.install();
    try {
      await dialog.getByRole("button", { name: /^检查并继续$|^Check and continue$/ }).click();
      await expect(dialog.getByRole("button", { name: /^检查并继续$|^Check and continue$/ })).toBeVisible();
      const before = await boxes(dialog);
      await page.clock.runFor(1100);
      const status = dialog.locator(".update-check-status");
      await expect(status.locator(".gpu-user")).toHaveCount(2);
      const after = await boxes(dialog);
      await testInfo.attach("footer-geometry", { body: JSON.stringify({ before, after }), contentType: "application/json" });
      expect(after).toHaveLength(3);
      for (let i = 0; i < before.length; i++) for (const edge of ["x", "y", "width", "height"] as const) {
        expect.soft(Math.abs(before[i][edge] - after[i][edge]), `button ${i} ${edge}`).toBeLessThanOrEqual(1);
      }
      await expect(dialog.locator(".dialog-buttons .update-check-status")).toHaveCount(0);
      await expect(dialog.locator(".upload-body .update-check-status")).toHaveCount(1);
      await expect(dialog.getByRole("button", { name: /Cancel waiting|取消等待/ })).toHaveCount(0);
      expect(await dialog.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      expect(await dialog.locator(".upload-body").evaluate((node) => node.clientHeight)).toBeGreaterThan(200);
      await status.scrollIntoViewIfNeeded();
      await expect(dialog.locator(".upload-footer")).toBeInViewport();
      if (language === "zh") await page.screenshot({ path: resolve(SHOTS, `upload-check-waiting-${width}.png`) });
      await dialog.locator("#upload-slot-run").scrollIntoViewIfNeeded();
      await expect(dialog.locator("#upload-slot-run")).toBeInViewport();
    } finally { release(); }
  });
}

for (const language of ["zh", "en"]) {
  test(`complete uploads with a version problem omit zero missing in ${language}`, async ({ page }) => {
    const dialog = await open(page, language);
    await dialog.locator("#upload-files").setInputFiles([run, ...PACKAGES.map((name, i) => deb(name, i === 0 ? "1.19.0-1" : "1.20.1-1"))]);
    await expect(dialog.locator(".upload-slot-problem")).toHaveCount(1);
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    await expect(dialog.locator("#upload-continue-reason")).toHaveText(language === "zh" ? "有 1 个需要处理。" : "1 need attention.");
    if (language === "zh") await page.screenshot({ path: resolve(SHOTS, "upload-footer-problems.png") });
  });

  test(`upload footer keeps missing-only and mixed problem counts in ${language}`, async ({ page }) => {
    const dialog = await open(page, language);
    await expect(dialog.locator("#upload-continue-reason")).toHaveText(language === "zh" ? "还差 5 个文件。" : "5 files still missing.");
    await dialog.locator("#upload-files").setInputFiles([run, ...PACKAGES.slice(0, 3).map((name, i) => deb(name, i === 0 ? "1.19.0-1" : "1.20.1-1"))]);
    await expect(dialog.locator(".upload-slot-problem")).toHaveCount(1);
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    await expect(dialog.locator("#upload-continue-reason")).toHaveText(language === "zh" ? "有 1 个需要处理，还差 1 个。" : "1 need attention, 1 still missing.");
  });

  test(`browser rejects oversized files without claiming server deletion in ${language}`, async ({ page }) => {
    const dialog = await open(page, language);
    let uploads = 0;
    page.on("request", (request) => { if (request.method() === "PUT" && request.url().includes("/api/uploads/")) uploads++; });
    await dialog.locator("#upload-files").evaluate((node) => {
      const file = new File(["small fixture"], "wrong-download.run", { type: "application/octet-stream" });
      Object.defineProperty(file, "size", { value: 1024 ** 3 + 1 });
      const transfer = new DataTransfer(); transfer.items.add(file);
      (node as HTMLInputElement).files = transfer.files;
      node.dispatchEvent(new Event("change", { bubbles: true }));
    });
    const rejection = dialog.locator(".upload-unused-file p");
    await expect(rejection).toHaveText(language === "zh"
      ? "wrong-download.run 超过 1 GiB 上限，请按清单中的链接重新下载驱动安装包（约 300 MB）。"
      : "wrong-download.run exceeds the 1 GiB limit. Download the driver installer again using the checklist link (about 300 MB).");
    expect(uploads).toBe(0);
    if (language === "zh") {
      await rejection.scrollIntoViewIfNeeded();
      await page.screenshot({ path: resolve(SHOTS, "upload-oversized-rejected.png") });
    }
  });

  test(`server oversized rejection retains deletion wording in ${language}`, async ({ page }) => {
    const dialog = await open(page, language);
    await page.route("**/api/uploads/*/files*", (route) => route.fulfill({ status: 413, json: {
      error: "upload.rejected", params: { reason: "too_large", file_name: run.name },
    } }));
    await dialog.locator("#upload-files").setInputFiles(run);
    await expect(dialog.locator(".upload-unused-file p")).toHaveText(language === "zh"
      ? "上传已停止：driver.run 超过 1 GiB 上限，已写入的部分已删除。"
      : "Upload stopped: driver.run exceeds the 1 GiB limit; the part already written has been deleted.");
  });
}

test("unreadable GPU check result uses the unreadable title", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.locator("#lang-control").selectOption("zh");
  await page.route("**/api/plans", (route) => route.fulfill({ json: { result: "blocked", failures: [], gpu: {
    state: "unreadable", users: [], checked_at: snapshot.checked_at,
    read_reason: { key: "reason.io_error", params: {} }, waited_ms: 30000,
  } } }));
  await page.locator("#version-select").selectOption("580.178.04");
  await page.locator(".actions").getByRole("button", { name: "检查并继续", exact: true }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog.locator(".dialog-title")).toHaveText("无法更新：无法读取显卡使用状态");
  await expect(dialog.locator(".gpu-list-headline")).toHaveText("按被使用处理。");
  await page.screenshot({ path: resolve(SHOTS, "gpu-unreadable-dialog.png") });
});
