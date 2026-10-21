import { test, expect, type CDPSession } from "./local-test.js";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { setScenario, signIn } from "./helpers.js";

const packages = ["libnvidia-container1", "libnvidia-container-tools", "nvidia-container-toolkit-base", "nvidia-container-toolkit"];
const run = { name: "announcement-driver.run", mimeType: "application/octet-stream", buffer: Buffer.from('#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04"\n' + "x".repeat(4096)) };
const deb = (name: string) => ({ name: `${name}.deb`, mimeType: "application/octet-stream", buffer: Buffer.from(`NVM_TEST_DEB\nPackage: ${name}\nVersion: 1.20.1-1\nArchitecture: amd64\n`) });

async function liveAccessibility(cdp: CDPSession) {
  const { root } = await cdp.send("DOM.getDocument");
  const { nodeId } = await cdp.send("DOM.querySelector", { nodeId: root.nodeId, selector: 'dialog.upload-dialog[open] [aria-live="polite"]' });
  const { node } = await cdp.send("DOM.describeNode", { nodeId });
  const { nodes } = await cdp.send("Accessibility.getFullAXTree");
  const live = nodes.find((entry) => entry.backendDOMNodeId === node.backendNodeId);
  const byId = new Map(nodes.map((entry) => [entry.nodeId, entry]));
  const descend = (id: string): string[] => {
    const entry = byId.get(id);
    if (!entry || entry.ignored) return [];
    return [entry.name?.value ?? "", ...(entry.childIds ?? []).flatMap(descend)];
  };
  return { ignored: live?.ignored, live: live?.properties?.find((entry) => entry.name === "live")?.value.value, text: live ? descend(live.nodeId).join(" ") : "" };
}

for (const language of ["zh", "en"]) {
  test(`ready uploads keep file and failed-check announcements in the accessibility tree in ${language}`, async ({ page, request }, testInfo) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    await setScenario(request, "upload_files");
    await page.goto("/"); await signIn(page); await page.goto("/#/update");
    await page.locator("#lang-control").selectOption(language);
    await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
    const dialog = page.locator("dialog.upload-dialog[open]");
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    const live = dialog.locator('[aria-live="polite"]');
    await expect(live).toHaveCount(1);
    const originalLive = await live.elementHandle();
    const cdp = await page.context().newCDPSession(page);
    try {
      await dialog.locator("#upload-files").setInputFiles([run, ...packages.map(deb)]);
      await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
      await expect(dialog.locator("#upload-files")).toBeEnabled();
      const counter = language === "zh" ? "已就绪 5 / 5" : "5 of 5 ready";
      await expect(dialog.getByText(counter, { exact: true })).toBeHidden();
      const fileReady = language === "zh" ? `${run.name} 已就绪` : `${run.name} is ready`;
      await expect(live).toContainText(fileReady);
      expect(await live.evaluate((node, original) => node === original && node.isConnected, originalLive)).toBe(true);
      const ready = await liveAccessibility(cdp);
      expect.soft(ready.ignored, "the ready live region remains in Chromium's accessibility tree").toBe(false);
      expect.soft(ready.live).toBe("polite");
      expect.soft(ready.text).toContain(fileReady);
      expect.soft(ready.text).not.toContain(counter);

      await page.route("**/api/plans", (route) => route.fulfill({ status: 409, json: { error: "plan.changed", params: { changed: ["gpus"] } } }));
      await dialog.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
      const failed = language === "zh" ? "检查失败，请查看原因后重试。" : "The check failed. Read the reason and retry.";
      await expect(live).toContainText(failed);
      await expect(dialog.locator("#upload-files")).toBeEnabled();
      await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
      expect(await live.evaluate((node, original) => node === original && node.isConnected, originalLive)).toBe(true);
      const rejected = await liveAccessibility(cdp);
      expect.soft(rejected.ignored).toBe(false);
      expect.soft(rejected.live).toBe("polite");
      expect.soft(rejected.text).toContain(failed);
      expect.soft(rejected.text).not.toContain(counter);
      await testInfo.attach("live-accessibility", { body: JSON.stringify({ ready, rejected }), contentType: "application/json" });
      if (process.env["NVM_REVIEW3_SHOTS"] === "1" && language === "zh") {
        const output = resolve(process.cwd(), "../.implementation/release-fixes/review3-shots");
        mkdirSync(output, { recursive: true });
        await page.screenshot({ path: resolve(output, "upload-check-failed-zh.png") });
      }
      await expect(dialog.getByText(counter, { exact: true })).toBeHidden();
    } finally { await cdp.detach(); }
  });
}
