import { test, expect, type Locator, type Page } from "./local-test.js";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { BASE, setScenario, signIn } from "./helpers.js";

const OPEN = /Upload files manually…|手动上传文件…/;
const CHECK = /^Check and continue$|^检查并继续$/;
const PACKAGES = ["libnvidia-container1", "libnvidia-container-tools", "nvidia-container-toolkit-base", "nvidia-container-toolkit"];
const SHOTS = resolve(process.cwd(), process.env["NVM_UPLOAD_SCREENSHOT_DIR"] ?? "../.implementation/upload-redesign/screenshots");
const run = { name: "renamed-driver.run", mimeType: "application/octet-stream", buffer: Buffer.from('#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04"\n' + "x".repeat(4096)) };
function deb(packageName: string, arch = "amd64") {
  return { name: `${packageName}-renamed.deb`, mimeType: "application/octet-stream", buffer: Buffer.from(`NVM_TEST_DEB\nPackage: ${packageName}\nVersion: 1.20.1-1\nArchitecture: ${arch}\n`) };
}
function smallDeb() {
  const file = deb(PACKAGES[1]);
  const buffer = Buffer.alloc(20_804, " ");
  file.buffer.subarray(0, -1).copy(buffer);
  return { ...file, buffer };
}
async function open(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: OPEN }).click();
  const dialog = page.locator("dialog.upload-dialog[open]");
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  return dialog;
}
async function fit(page: Page, dialog: Locator): Promise<void> {
  await expect(dialog.locator("ol.upload-slots > li")).toHaveCount(5);
  expect(await dialog.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await expect(dialog.locator(".upload-footer")).toBeInViewport();
  await expect(dialog.locator(".upload-footer").getByRole("button", { name: CHECK })).toBeInViewport();
}

for (const language of ["en", "zh"]) for (const width of [1280, 390]) {
  test(`footer actions stay right aligned from empty through one receipt in ${language} at ${width}px`, async ({ page, request }) => {
    await page.setViewportSize({ width, height: 900 });
    await setScenario(request, "upload_files");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    await page.locator("#lang-control").selectOption(language);
    const dialog = await open(page);
    const footer = dialog.locator(".upload-footer");
    const check = footer.getByRole("button", { name: CHECK });
    const close = footer.getByRole("button", { name: /^Close$|^关闭$/ });
    const restart = footer.getByRole("button", { name: /^Clear and start again$|^清空重来$/ });
    const edges = () => Promise.all([check, close].map((button) => button.evaluate((node) => {
      const rect = node.getBoundingClientRect(); return { right: rect.right, top: rect.top };
    })));
    await expect(restart).toBeHidden();
    const empty = await edges();
    await dialog.locator("#upload-files").setInputFiles(smallDeb());
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(1);
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    await expect.soft(dialog.locator(".upload-receipt")).toHaveText(`${smallDeb().name} · 21 KB`);
    await expect(restart).toBeVisible();
    const partial = await edges();
    expect.soft(Math.abs(empty[0].right - partial[0].right)).toBeLessThanOrEqual(1);
    expect.soft(Math.abs(empty[1].right - partial[1].right)).toBeLessThanOrEqual(1);
    expect(Math.abs(empty[0].top - empty[1].top)).toBeLessThanOrEqual(1);
    expect.soft(Math.abs(partial[0].top - partial[1].top)).toBeLessThanOrEqual(1);
    const positions = await footer.evaluate((node) => {
      const row = node.querySelector(":scope > .dialog-buttons")!.getBoundingClientRect();
      const buttons = [...node.querySelectorAll(":scope > .dialog-buttons button")].map((button) => button.getBoundingClientRect());
      return { left: row.left, right: row.right, restart: buttons[0].left, check: buttons[2].right };
    });
    expect(Math.abs(positions.restart - positions.left)).toBeLessThanOrEqual(1);
    expect(Math.abs(positions.check - positions.right)).toBeLessThanOrEqual(1);
    await fit(page, dialog);
  });
}

test("five real uploads survive confirmation Back and become task inputs only on Start", async ({ page, request }) => {
  await setScenario(request, "upload_files");
  await page.goto("/"); await signIn(page); await page.goto("/#/update");
  const dialog = await open(page);
  await dialog.locator("#upload-link-version").selectOption("580.178.04");
  const driverLink = dialog.locator(".upload-run-download");
  await expect(driverLink).toHaveAttribute("href", "https://download.nvidia.com/XFree86/Linux-x86_64/580.178.04/NVIDIA-Linux-x86_64-580.178.04-no-compat32.run");
  await expect(driverLink).toHaveAttribute("target", "_blank");
  await expect(driverLink).toHaveAttribute("rel", "noopener noreferrer");
  await dialog.locator("#upload-link-version").selectOption("");
  await expect(driverLink).toHaveAttribute("href", "https://download.nvidia.com/XFree86/Linux-x86_64/");
  const uploads: string[] = [];
  page.on("request", (req) => { if (req.method() === "PUT" && req.url().includes("/api/uploads/")) uploads.push(req.url()); });
  // Deliberately select the large installer first; the UI sends the four small packages first.
  await dialog.locator("#upload-files").setInputFiles([run, ...PACKAGES.map((name) => deb(name))]);
  await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
  expect(uploads).toHaveLength(5);
  expect(uploads.slice(0, 4).every((url) => url.includes(".deb"))).toBe(true);
  expect(uploads[4]).toContain(".run");
  const token = await page.evaluate(() => sessionStorage.getItem("nvm.token") ?? "");
  const readGroup = async () => {
    const response = await page.request.get(`${BASE}/api/uploads/current`, { headers: { "X-NVM-Token": token } });
    expect(response.ok()).toBe(true); return response.json();
  };
  const before = await readGroup();
  await dialog.getByRole("button", { name: CHECK }).click();
  const confirmation = page.locator("dialog[open]");
  await expect(confirmation.locator(".dialog-title")).toBeFocused();
  await expect(confirmation).toContainText(/The 5 uploaded files passed the checks|5 个手动上传的文件已通过检查/);
  await confirmation.getByRole("button", { name: /Back to selection|返回选择/ }).click();
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
  const after = await readGroup();
  expect(after.group_id).toBe(before.group_id);
  expect(after.run.sha256).toBe(before.run.sha256);
  expect(after.debs).toEqual(before.debs);
  await dialog.getByRole("button", { name: CHECK }).click();
  await page.locator("dialog[open]").getByRole("button", { name: /Save original driver and start|保存原版驱动并开始/ }).click();
  await expect(page).toHaveURL(/#\/tasks\//);
  await expect(page.locator(".task-state")).toBeFocused();
});

for (const language of ["en", "zh"]) {
  test(`a rejected Debian version explains how to download again in ${language}`, async ({ page, request }) => {
    await setScenario(request, "upload_files");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    await page.locator("#lang-control").selectOption(language);
    const dialog = await open(page);
    const file = deb(PACKAGES[0]);
    file.buffer = Buffer.from(file.buffer.toString().replace("Version: 1.20.1-1", "Version: invalid"));
    await dialog.locator("#upload-files").setInputFiles(file);
    await expect(dialog.locator(".upload-unused-file")).toHaveCount(1);
    await expect(dialog.locator(".upload-unused-file")).toContainText(language === "en"
      ? `The version of ${file.name} could not be read; download the package again using the link in the checklist.`
      : `无法读取 ${file.name} 的版本号，请从清单中的链接重新下载对应包。`);
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(0);
  });
}

test("upload checklist stays readable in both languages, four themes and desktop or narrow screens", async ({ page, request }) => {
  test.setTimeout(180_000); mkdirSync(SHOTS, { recursive: true });
  for (const language of ["en", "zh"]) {
    await setScenario(request, "upload_rejected");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    // Keep uploads small; the receipt fixture uses the official file sizes from the task.
    const sizes: Record<string, number> = { run: 316_187_987, [PACKAGES[0]]: 1_236_052, [PACKAGES[1]]: 20_804, [PACKAGES[2]]: 5_993_348, [PACKAGES[3]]: 1_501_496 };
    await page.route("**/api/uploads/*/files*", async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      if (body.file) body.file.size = sizes[body.file.package ?? body.file.kind];
      await route.fulfill({ response, json: body });
    });
    await page.locator("#lang-control").selectOption(language);
    const dialog = await open(page);
    for (const state of ["empty", "partial", "rejected", "ready"]) {
      if (state === "partial") {
        await dialog.locator("#upload-files").setInputFiles(deb(PACKAGES[0]));
        await expect(dialog.locator(".upload-slot-ready")).toHaveCount(1);
      }
      if (state === "rejected") {
        await dialog.locator("#upload-files").setInputFiles(deb("nvidia-container-toolkit-dev"));
        await expect(dialog.locator(".upload-unused-file")).toContainText("nvidia-container-toolkit-dev");
        await expect(dialog.locator("#upload-files")).toBeEnabled();
      }
      if (state === "ready") {
        await dialog.locator(".upload-unused-file button").click();
        await dialog.locator("#upload-files").setInputFiles([run, ...PACKAGES.slice(1).map((name) => deb(name))]);
        await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
      }
      for (const theme of ["focused-light", "focused-dark", "plate-light", "plate-dark"]) {
        await page.locator("#theme-control").evaluate((node, value) => {
          const control = node as HTMLSelectElement; control.value = value; control.dispatchEvent(new Event("change", { bubbles: true }));
        }, theme);
        for (const width of [1280, 390]) {
          await page.setViewportSize({ width, height: 900 });
          await fit(page, dialog);
          const check = dialog.getByRole("button", { name: CHECK });
          await expect(check).toHaveAttribute("aria-disabled", state === "ready" ? "false" : "true");
          if (state === "empty") {
            await check.focus(); await expect(check).toBeFocused();
            await check.click({ force: true }); await expect(dialog.locator("#upload-slot-run")).toBeFocused();
          }
          await dialog.locator(".upload-body").evaluate((node) => { node.scrollTop = 0; });
          await page.screenshot({ path: resolve(SHOTS, `upload-${state}-${language}-${theme}-${width}.png`) });
        }
      }
    }
    await dialog.locator(".upload-footer").getByRole("button", { name: /^Close$|^关闭$/ }).click();
  }
});

test("rejected package and architecture fixtures remain outside the five ready slots", async ({ page, request }) => {
  await setScenario(request, "upload_rejected");
  await page.goto("/"); await signIn(page); await page.goto("/#/update");
  const dialog = await open(page);
  await dialog.locator("#upload-files").setInputFiles([deb("nvidia-container-toolkit-dev"), deb("libnvidia-container1", "arm64")]);
  await expect(dialog.locator(".upload-unused-file")).toHaveCount(2);
  await expect(dialog.locator(".upload-unused")).toContainText("is not one of the 4 required packages");
  await expect(dialog.locator(".upload-unused")).toContainText("not amd64");
  await expect(dialog.locator(".upload-slot-ready")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "true");
  await dialog.locator(".upload-unused-file button").first().click();
  await expect(dialog.locator(".upload-unused-file")).toHaveCount(1);
});

for (const language of ["en", "zh"]) {
  test(`a replacement refused at commit stays on its content slot in ${language}`, async ({ page, request }) => {
    await setScenario(request, "upload_files");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    await page.locator("#lang-control").selectOption(language);
    const dialog = await open(page);
    await dialog.locator("#upload-files").setInputFiles([run, ...PACKAGES.map((name) => deb(name))]);
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
    // The Rust read-barrier regression exercises the real commit rejection. Here
    // deliver that response to verify the browser's existing per-file error path.
    await page.route("**/api/uploads/*/files*", async (route) => {
      const isRun = route.request().url().includes(".run");
      await route.fulfill({ status: 409, json: { error: "upload.busy", params: {
        file_name: isRun ? run.name : deb(PACKAGES[0]).name,
        kind: isRun ? "run" : "deb", package: isRun ? null : PACKAGES[0],
      } } });
    });
    await dialog.locator("#upload-files").setInputFiles([run, deb(PACKAGES[0])]);
    const message = language === "en" ? "awaiting confirmation" : "等待确认";
    await expect(dialog.locator("#upload-slot-run .upload-slot-error")).toContainText(message);
    await expect(dialog.locator(`#upload-slot-${PACKAGES[0]} .upload-slot-error`)).toContainText(message);
    await expect(dialog.locator(".upload-unused-file")).toHaveCount(0);
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
    await expect(dialog.locator("#upload-files")).toBeEnabled();
  });

  test(`upload progress, plan handoff and version decisions in ${language}`, async ({ page, request }) => {
    mkdirSync(SHOTS, { recursive: true });
    await setScenario(request, "upload_files");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    await page.locator("#lang-control").selectOption(language);
    await expect(page.locator(".upload-entry-hint")).toBeVisible();
    await page.screenshot({ path: resolve(SHOTS, `entry-${language}.png`) });
    const dialog = await open(page);
    let release = (): void => {};
    const gate = new Promise<void>((resolveGate) => { release = resolveGate; });
    await page.route("**/api/uploads/*/files*", async (route) => {
      if (!route.request().url().includes(".run")) { await route.continue(); return; }
      const response = await route.fetch();
      await gate;
      await route.fulfill({ response });
    });
    await dialog.locator("#upload-files").setInputFiles([run, ...PACKAGES.map((name) => deb(name))]);
    try {
      await expect(dialog.locator(".upload-slot-ready")).toHaveCount(4);
      await expect(dialog.locator("progress")).toBeVisible();
      await expect(dialog.locator(".upload-progress")).toContainText(/5 of 5|5 \/ 5/);
      await expect(dialog.locator("[aria-live], [role=status], [role=alert]")).toHaveCount(1);
      await page.screenshot({ path: resolve(SHOTS, `upload-progress-${language}.png`) });
    } finally { release(); }
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    let releasePlan = (): void => {};
    const planGate = new Promise<void>((resolveGate) => { releasePlan = resolveGate; });
    await page.route("**/api/plans", async (route) => { const response = await route.fetch(); await planGate; await route.fulfill({ response }); });
    await dialog.getByRole("button", { name: CHECK }).click();
    try {
      await expect(dialog).toBeVisible();
      await expect(dialog.locator(".btn-primary")).toHaveText(CHECK);
      await page.screenshot({ path: resolve(SHOTS, `upload-checking-${language}.png`) });
    } finally { releasePlan(); }
    const confirm = page.locator("dialog[open]");
    await expect(confirm.locator(".dialog-title")).toBeFocused();
    await expect(confirm.locator("details")).not.toHaveAttribute("open", "");
    await page.screenshot({ path: resolve(SHOTS, `upload-confirm-${language}.png`) });
    await confirm.getByRole("button", { name: /Back to selection|返回选择/ }).click();
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
    await page.unroute("**/api/plans");
    await dialog.locator(".upload-footer").getByRole("button", { name: /^Close$|^关闭$/ }).click();
    await setScenario(request, "upload_multi");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    const multi = await open(page);
    await expect(multi.locator("input[name=upload-version]:checked")).toHaveCount(0);
    await expect(multi.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "true");
    await page.screenshot({ path: resolve(SHOTS, `upload-version-choice-${language}.png`) });
    await multi.locator(".upload-footer").getByRole("button", { name: /^Close$|^关闭$/ }).click();
    await setScenario(request, "upload_manual");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    const manual = await open(page);
    await expect(manual.locator("#upload-version-input")).toBeVisible();
    await page.screenshot({ path: resolve(SHOTS, `upload-version-manual-${language}.png`) });
    await manual.locator(".upload-footer").getByRole("button", { name: /^Close$|^关闭$/ }).click();
    await setScenario(request, "upload_below_floor");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    const old = await open(page);
    await expect(old.locator(".upload-slot-error")).toHaveCount(1);
    await old.locator(".upload-slot-error").scrollIntoViewIfNeeded();
    await page.screenshot({ path: resolve(SHOTS, `upload-toolkit-old-${language}.png`) });
  });
}

test("a toolkit read from a closed upload dialog cannot end the current session", async ({ page, request }) => {
  await setScenario(request, "upload_files");
  await page.goto("/"); await signIn(page); await page.goto("/#/update");
  let release = (): void => {};
  const gate = new Promise<void>((resolveGate) => { release = resolveGate; });
  await page.route("**/api/uploads/toolkit-version", async (route) => {
    await gate; await route.fulfill({ status: 401, json: { error: "auth.required", params: {} } });
  });
  const dialog = await open(page);
  await dialog.locator(".upload-footer").getByRole("button", { name: /^Close$|^关闭$/ }).click();
  const response = page.waitForResponse((res) => res.url().endsWith("/api/uploads/toolkit-version"));
  release(); await response;
  await page.evaluate(() => new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame())));
  await expect(page.locator("#login-user")).toHaveCount(0);
  await expect(page.locator("#version-select")).toBeVisible();
});
