import { test, expect, type Locator, type Page } from "./local-test.js";
import { BASE, setScenario, signIn } from "./helpers.js";

const CHECK = /^Check and continue$|^检查并继续$/;
const ROW_CHECK = /Check this version and continue|用这个版本检查并继续|换回此版本|Restore this version/;
const packages = ["libnvidia-container1", "libnvidia-container-tools", "nvidia-container-toolkit-base", "nvidia-container-toolkit"];
const run = { name: "driver.run", mimeType: "application/octet-stream", buffer: Buffer.from('#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04"\n' + "x".repeat(4096)) };
const deb = (name: string) => ({ name: `${name}.deb`, mimeType: "application/octet-stream", buffer: Buffer.from(`NVM_TEST_DEB\nPackage: ${name}\nVersion: 1.20.1-1\nArchitecture: amd64\n`) });
const blocked = { result: "blocked", failures: [], gpu: { state: "busy", users: [], errors: [], waited_ms: 30000 } };

test.beforeEach(async ({ page }) => {
  await page.route("**/*", async (route) => {
    if (new URL(route.request().url()).hostname === "127.0.0.1") await route.continue();
    else await route.abort();
  });
});

async function start(page: Page, language: string, scenario = "upload_files", path = "update") {
  await setScenario(page.request, scenario);
  await page.goto("/"); await signIn(page); await page.goto(`/#/${path}`);
  await page.locator("#lang-control").selectOption(language);
}
async function upload(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog.upload-dialog[open]");
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  return dialog;
}
async function ready(dialog: Locator) {
  await dialog.locator("#upload-files").setInputFiles([run, ...packages.map(deb)]);
  await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
  await expect(dialog.locator("#upload-files")).toBeEnabled();
}

for (const language of ["zh", "en"]) {
  test(`one confirmation dismiss control survives a rejected submission in ${language}`, async ({ page }) => {
    await start(page, language);
    await page.selectOption("#version-select", "580.178.04");
    const trigger = page.locator(".actions").getByRole("button", { name: CHECK });
    await trigger.click();
    const dialog = page.locator("dialog[open]");
    const back = dialog.getByRole("button", { name: /Back to selection|返回选择/ });
    await expect(back).toBeVisible();
    const handle = await back.elementHandle();
    // Count hidden controls too: merging the buttons must remove the second element.
    await expect(dialog.locator("button").filter({ hasText: /^(Back to selection|返回选择|Close|关闭)$/ })).toHaveCount(1);
    await page.route("**/api/tasks", (route) => route.fulfill({ status: 409, json: { error: "plan.changed", params: { changed: ["gpus"] } } }));
    await dialog.locator("button.btn-primary").click();
    const close = dialog.getByRole("button", { name: /^Close$|^关闭$/ });
    await expect(close).toBeFocused();
    expect(await close.evaluate((node, old) => node === old, handle)).toBe(true);
    let cancelled = 0;
    page.on("request", (request) => { if (request.method() === "POST" && /\/api\/plans\/[^/]+\/cancel$/.test(request.url())) cancelled++; });
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(trigger).toBeFocused();
    expect(cancelled).toBe(1);
  });

  test(`upload cancellation stays in the app, restores focus, and aborts only on confirmation in ${language}`, async ({ page }) => {
    await start(page, language);
    const dialog = await upload(page);
    await dialog.locator("#upload-files").setInputFiles(deb(packages[0]));
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(1);
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    const token = await page.evaluate(() => sessionStorage.getItem("nvm.token") ?? "");
    const readGroup = async () => {
      const response = await page.request.get(`${BASE}/api/uploads/current`, { headers: { "X-NVM-Token": token } });
      expect(response.ok()).toBe(true); return response.json();
    };
    const received = await readGroup();
    let release = (): void => {};
    const pending = new Promise<void>((done) => { release = done; });
    let puts = 0; let aborted = 0; let native = 0;
    page.on("dialog", (message) => { native++; void message.dismiss(); });
    page.on("requestfailed", (request) => { if (request.method() === "PUT") aborted++; });
    await page.route("**/api/uploads/*/files*", async (route) => {
      puts++; await pending; await route.continue().catch(() => {});
    });
    try {
      await dialog.locator("#upload-files").setInputFiles([run, deb(packages[1])]);
      await expect.poll(() => puts).toBe(1);
      const cancel = dialog.locator(".upload-footer").getByRole("button", { name: /^Cancel upload$|^取消上传$/ });
      await cancel.click();
      expect(native).toBe(0);
      await expect(dialog.locator(".upload-cancel-confirm")).toHaveAccessibleName(language === "zh"
        ? "取消后，正在上传的文件会被删除，已收到的保留。"
        : "If you cancel, the file being uploaded is deleted. Files already received are kept.");
      expect(await dialog.locator(".upload-cancel-confirm button").allTextContents()).toEqual(language === "zh"
        ? ["继续上传", "确认取消上传"] : ["Continue uploading", "Confirm cancel"]);
      const keep = page.getByRole("button", { name: /^Continue uploading$|^继续上传$/ });
      await expect(keep).toBeVisible();
      await expect(keep).toBeFocused();
      await page.keyboard.press("Escape");
      await expect(keep).toHaveCount(0);
      await expect(dialog).toBeVisible();
      await expect(cancel).toBeFocused();
      expect(aborted).toBe(0);
      await cancel.click();
      await keep.click();
      await expect(cancel).toBeFocused();
      expect(aborted).toBe(0);
      await cancel.click();
      // The confirmation action differs from the trigger that opens it.
      await dialog.locator(".upload-cancel-confirm").getByRole("button", { name: /^Confirm cancel$|^确认取消上传$/ }).click();
      await expect(dialog).toHaveCount(0);
      await expect.poll(() => aborted).toBe(1);
      expect(puts).toBe(1);
      release();
      const reopened = await upload(page);
      await expect(reopened.locator(".upload-slot-ready")).toHaveCount(1);
      const retained = await readGroup();
      expect(retained.group_id).toBe(received.group_id);
      expect(retained.debs).toEqual(received.debs);
      expect(retained.run).toBeNull();
    } finally { release(); }
  });

  test(`upload GPU recheck submits the same upload plan again in ${language}`, async ({ page }) => {
    await start(page, language);
    const dialog = await upload(page); await ready(dialog);
    const posts: unknown[] = [];
    let repeatedUploads = 0;
    page.on("request", (request) => { if (request.method() === "PUT") repeatedUploads++; });
    await page.route("**/api/plans", (route) => {
      posts.push(route.request().postDataJSON());
      if (posts.length === 1) return route.fulfill({ json: blocked });
      if (posts.length === 2) return route.fulfill({ json: { ...blocked, gpu: { ...blocked.gpu, state: "unreadable", read_reason: { key: "reason.permission_denied", params: {} } } } });
      return route.continue();
    });
    await dialog.getByRole("button", { name: CHECK }).click();
    let result = page.locator("dialog[open]").last();
    await expect(result.getByRole("button", { name: /Check again|重新检查/ })).toBeVisible();
    await result.getByRole("button", { name: /Check again|重新检查/ }).click();
    await expect.poll(() => posts.length).toBe(2);
    result = page.locator("dialog[open]").last();
    await expect(result.locator(".dialog-title")).toHaveText(language === "zh" ? "无法更新：无法读取显卡使用状态" : "Cannot update: GPU usage state could not be read");
    await result.getByRole("button", { name: /Check again|重新检查/ }).click();
    await expect(page.locator("dialog[open]").last().getByRole("button", { name: /Back to selection|返回选择/ })).toBeVisible();
    expect(posts).toHaveLength(3);
    expect(posts[1]).toEqual(posts[0]);
    expect(posts[2]).toEqual(posts[0]);
    expect(posts[0]).toMatchObject({ source: { kind: "upload", version: "580.178.04" } });
    expect(repeatedUploads).toBe(0);
    await page.locator("dialog[open]").last().getByRole("button", { name: /Back to selection|返回选择/ }).click();
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
  });

  test(`driver plans are single flight across rows and can resume after cancellation in ${language}`, async ({ page }) => {
    await start(page, language, "drivers_rows", "drivers");
    const buttons = page.locator("table.drivers tbody button").filter({ hasText: ROW_CHECK });
    const enabled = await buttons.evaluateAll((nodes) => nodes.map((node, i) => !(node as HTMLButtonElement).disabled ? i : -1).filter((i) => i >= 0));
    expect(enabled.length).toBeGreaterThan(1);
    const first = buttons.nth(enabled[0]), second = buttons.nth(enabled[1]);
    let release = (): void => {};
    const pending = new Promise<void>((done) => { release = done; });
    const posts: unknown[] = [];
    await page.route("**/api/plans", async (route) => { posts.push(route.request().postDataJSON()); await pending; await route.fulfill({ json: blocked }).catch(() => {}); });
    await page.clock.install();
    try {
      await first.click(); await expect.poll(() => posts.length).toBe(1);
      await second.evaluate((node: HTMLButtonElement) => node.click());
      await page.waitForTimeout(100);
      expect(posts).toHaveLength(1);
      await page.clock.runFor(1100);
      await page.getByRole("button", { name: /Cancel waiting|取消等待/ }).click();
      await expect(first).toBeFocused();
      await expect(second).toBeEnabled();
      await second.click(); await expect.poll(() => posts.length).toBe(2);
      expect(posts[1]).not.toEqual(posts[0]);
      release();
      await expect(page.locator("dialog[open]")).toHaveCount(1);
    } finally { release(); }
  });

  test(`complete upload keeps one completion sentence and hides the counter in ${language}`, async ({ page }) => {
    await start(page, language);
    const dialog = await upload(page);
    await expect(dialog.locator(".upload-counter")).toBeVisible();
    await ready(dialog);
    await expect(dialog.locator(".upload-complete")).toBeVisible();
    await expect(dialog.locator(".upload-counter")).toBeHidden();
    await expect(dialog.locator(".upload-complete")).toHaveCount(1);
    await dialog.getByRole("button", { name: /^Clear and start again$|^清空重来$/ }).click();
    await expect(dialog.locator(".upload-slot-waiting")).toHaveCount(5);
    await expect(dialog.locator(".upload-counter")).toBeVisible();
  });
}

for (const language of ["zh", "en"]) {
  test(`version instructions appear once and retain input validation in ${language}`, async ({ page }) => {
    await start(page, language, "upload_manual");
    const dialog = await upload(page);
    const instruction = language === "zh"
      ? "无法从安装包读出版本号，请输入本驱动的版本（例如 580.173.02）。"
      : "The version could not be read from the installer. Enter the version of this driver (for example 580.173.02).";
    const count = (text: string) => dialog.evaluate((node, text) => (node.textContent ?? "").split(text).length - 1, text);
    const input = dialog.locator("#upload-version-input");
    const label = dialog.locator('label[for="upload-version-input"]');
    await expect(label).toHaveText(instruction);
    expect.soft(await count(instruction)).toBe(1);
    const reason = language === "zh" ? "请先确认驱动版本。" : "Confirm the driver version first.";
    await expect.soft(dialog.locator("#upload-continue-reason")).toHaveText(reason);
    await expect(dialog.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "true");
    await input.fill("580");
    await expect(input).toHaveAttribute("aria-invalid", "true");
    await expect(input).toHaveAttribute("aria-describedby", "upload-version-invalid");
    await expect(dialog.locator("#upload-version-invalid")).toBeVisible();
    await expect(dialog.locator("#upload-version-invalid")).toHaveText(language === "zh"
      ? "版本号须为 2 到 4 段用点分隔的数字。"
      : "The version must be two to four groups of digits separated by dots.");
    expect.soft(await count(instruction)).toBe(1);
    await expect.soft(dialog.locator("#upload-continue-reason")).toHaveText(reason);
    await expect(dialog.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "true");
    await input.fill("580.178.04");
    await expect(input).toHaveAttribute("aria-invalid", "false");
    await expect(dialog.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "false");
    await dialog.locator(".upload-footer").getByRole("button", { name: /^Close$|^关闭$/ }).click();

    // A fresh scenario keeps the radio-choice case independent from the manual version above.
    await page.context().clearCookies();
    await page.evaluate(() => sessionStorage.clear());
    await start(page, language, "upload_multi");
    const choice = await upload(page);
    const choose = language === "zh"
      ? "安装包的标签里有多个版本号，请选择本驱动的版本。"
      : "The installer label contains several version numbers. Select the version of this driver.";
    await expect(choice.locator("#upload-slot-run legend")).toHaveText(choose);
    expect.soft(await choice.evaluate((node, text) => (node.textContent ?? "").split(text).length - 1, choose)).toBe(1);
    await expect(choice.getByRole("radio")).toHaveCount(2);
    await expect(choice.locator("#upload-continue-reason")).toHaveText(reason);
    await expect(choice.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "true");
    await choice.getByRole("radio").first().check();
    await expect(choice.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "false");
  });
}
