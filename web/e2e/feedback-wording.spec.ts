import { test, expect, type Page } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

const CHECK = /^Check and continue$|^检查并继续$/;
const packages = ["libnvidia-container1", "libnvidia-container-tools", "nvidia-container-toolkit-base", "nvidia-container-toolkit"];
const run = { name: "feedback-driver.run", mimeType: "application/octet-stream", buffer: Buffer.from('#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04"\n' + "x".repeat(4096)) };
const deb = (name: string) => ({ name: `${name}.deb`, mimeType: "application/octet-stream", buffer: Buffer.from(`NVM_TEST_DEB\nPackage: ${name}\nVersion: 1.20.1-1\nArchitecture: amd64\n`) });
const unreadable = { state: "unreadable", users: [], errors: [], checked_at: 1792209600000, read_reason: { key: "reason.io_error", params: {} }, waited_ms: 30000 };

async function start(page: Page, language: string) {
  await setScenario(page.request, "upload_files");
  await page.goto("/"); await signIn(page); await page.goto("/#/update");
  await page.locator("#lang-control").selectOption(language);
}

async function upload(page: Page) {
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog.upload-dialog[open]");
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  return dialog;
}

test.describe("feedback wording", () => {
  for (const language of ["zh", "en"]) {
    test(`unreadable snapshots stay concise while live checks retain their explanation in ${language}`, async ({ page }) => {
      await start(page, language);
      let release = (): void => {};
      const gate = new Promise<void>(done => { release = done; });
      let plans = 0;
      let result: object = unreadable;
      await page.route("**/api/gpu-users", route => route.fulfill({ json: unreadable }));
      await page.route("**/api/plans", async route => {
        plans++;
        if (plans === 1) await gate;
        await route.fulfill({ json: { result: "blocked", failures: [], gpu: result } }).catch(() => {});
      });
      try {
        await page.selectOption("#version-select", "580.178.04");
        await page.locator(".actions").getByRole("button", { name: CHECK }).click();
        const inline = page.locator(".update-check-status");
        await expect(inline.locator(".gpu-list-headline")).toHaveText(language === "zh"
          ? "无法读取显卡使用状态，按被使用处理。" : "The GPU usage state could not be read; it is treated as in use.");
        release();
        const dialog = page.locator("dialog[open]");
        await expect(dialog).toBeVisible();
        const phrase = language === "zh" ? "无法读取显卡使用状态" : "GPU usage state could not be read";
        expect.soft(await dialog.evaluate((node, text) => (node.textContent ?? "").split(text).length - 1, phrase)).toBe(1);
        await expect.soft(dialog.locator(".gpu-list-headline")).toHaveText(language === "zh" ? "按被使用处理。" : "Treated as in use.");
        await expect.soft(dialog.locator(".gpu-snapshot-time")).toHaveCount(0);
        await expect(dialog).toContainText(language === "zh" ? "驱动文件未改动。" : "The driver file was not changed.");
        await expect(dialog).toContainText(language === "zh" ? "原因：" : "Reason:");

        result = { state: "busy", checked_at: unreadable.checked_at, errors: [], users: [{ id: "docker:worker", name: "gpu-worker", type: "docker", usage: "in_use" }] };
        await dialog.getByRole("button", { name: /^Check again$|^重新检查$/ }).click();
        await expect(page.locator("dialog[open] .gpu-user-name")).toHaveText("gpu-worker");
        await expect(page.locator("dialog[open] .gpu-snapshot-time")).toHaveCount(1);
        result = { state: "busy", checked_at: unreadable.checked_at, errors: [], users: [] };
        await page.locator("dialog[open]").getByRole("button", { name: /^Check again$|^重新检查$/ }).click();
        await expect(page.locator("dialog[open] .gpu-list-headline")).toContainText(language === "zh"
          ? "没有找到申请显卡的应用" : "No app requesting the GPU was found");
        await expect(page.locator("dialog[open] .gpu-user-name")).toHaveCount(0);
        await expect.soft(page.locator("dialog[open] .gpu-snapshot-time")).toHaveCount(0);
        expect(plans).toBe(3);
      } finally { release(); }
    });

    test(`upload checks keep one visible progress sentence and their normal action in ${language}`, async ({ page }) => {
      await start(page, language);
      const dialog = await upload(page);
      await dialog.locator("#upload-files").setInputFiles([run, ...packages.map(deb)]);
      await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
      await expect(dialog.locator("#upload-files")).toBeEnabled();
      const live = dialog.locator('[aria-live="polite"]');
      const original = await live.elementHandle();
      const priorAnnouncement = await live.textContent();
      let release = (): void => {};
      const gate = new Promise<void>(done => { release = done; });
      let plans = 0;
      await page.route("**/api/plans", async route => {
        plans++; await gate;
        await route.fulfill({ status: 409, json: { error: "plan.changed", params: { changed: ["gpus"] } } }).catch(() => {});
      });
      try {
        const check = dialog.locator(".upload-footer .btn-primary");
        await check.click(); await expect.poll(() => plans).toBe(1);
        const checking = language === "zh" ? "正在检查…" : "Checking…";
        await expect(dialog.locator("#upload-continue-reason")).toHaveText(checking);
        expect.soft(await check.textContent()).toBe(language === "zh" ? "检查并继续" : "Check and continue");
        await expect(check).toHaveAttribute("aria-disabled", "true");
        expect.soft(await dialog.locator("p,button").evaluateAll((nodes, text) => nodes.filter(node => node.getClientRects().length > 0 && node.textContent === text).length, checking)).toBe(1);
        await expect(live).toHaveText(priorAnnouncement ?? "");
        expect(await live.evaluate((node, previous) => node === previous && node.isConnected, original)).toBe(true);
        await check.click({ force: true });
        expect(plans).toBe(1);
        await expect(dialog.locator(".upload-footer-actions").getByRole("button", { name: /^Close$|^关闭$/ })).toBeDisabled();
        release();
        await expect(dialog.locator("#upload-files")).toBeEnabled();
        await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
        await expect(live).toContainText(language === "zh" ? "检查失败，请查看原因后重试。" : "The check failed. Read the reason and retry.");
      } finally { release(); }
    });

    test(`unused-file cards show each identity once without weakening rejection details in ${language}`, async ({ page }) => {
      await start(page, language);
      const dialog = await upload(page);
      let puts = 0;
      page.on("request", request => { if (request.method() === "PUT" && request.url().includes("/api/uploads/")) puts++; });
      await dialog.locator("#upload-files").evaluate(node => {
        const file = new File(["fixture"], "oversized-feedback.run", { type: "application/octet-stream" });
        Object.defineProperty(file, "size", { value: 1024 ** 3 + 1 });
        const transfer = new DataTransfer(); transfer.items.add(file);
        (node as HTMLInputElement).files = transfer.files;
        node.dispatchEvent(new Event("change", { bubbles: true }));
      });
      const oversize = dialog.locator(".upload-unused-file").filter({ hasText: "oversized-feedback.run" });
      await expect(oversize.locator("p")).toContainText(language === "zh" ? "超过 1 GiB 上限" : "exceeds the 1 GiB limit");
      expect.soft(await oversize.evaluate(node => (node.textContent ?? "").split("oversized-feedback.run").length - 1)).toBe(1);
      expect(puts).toBe(0);
      await expect(dialog.locator("#upload-files")).toBeEnabled();

      await page.route("**/api/uploads/*/files*", route => route.fulfill({ status: 500, json: { error: "internal", params: { reason: { key: "reason.io_error", params: {} } } } }));
      await dialog.locator("#upload-files").setInputFiles({ ...deb(packages[0]!), name: "identity-preserved.deb" });
      const generic = dialog.locator(".upload-unused-file").filter({ hasText: "identity-preserved.deb" });
      await expect(generic.locator("strong")).toHaveText("identity-preserved.deb");
      await expect(generic.locator("p")).toContainText(language === "zh" ? "内部错误" : "Internal error");
      expect(await generic.evaluate(node => (node.textContent ?? "").split("identity-preserved.deb").length - 1)).toBe(1);
      await expect(dialog.locator("#upload-files")).toBeEnabled();

      await page.route("**/api/uploads/*/files*", route => route.fulfill({ status: 400, json: { error: "upload.rejected", params: { reason: "deb_arch", file_name: "server-reported.deb" } } }));
      await dialog.locator("#upload-files").setInputFiles({ ...deb(packages[0]!), name: "selected-file.deb" });
      const different = dialog.locator(".upload-unused-file").filter({ hasText: "selected-file.deb" });
      await expect(different.locator("strong")).toHaveText("selected-file.deb");
      await expect(different.locator("p")).toContainText("server-reported.deb");
      expect(puts).toBe(2);
    });
  }
});
