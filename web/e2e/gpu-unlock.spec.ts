import { test, expect, type Page } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

const TOOLKIT = ["libnvidia-container1", "libnvidia-container-tools", "nvidia-container-toolkit-base", "nvidia-container-toolkit"];
const installer = (version: string) => ({
  name: `NVIDIA-Linux-x86_64-${version}.run`,
  mimeType: "application/octet-stream",
  buffer: Buffer.from(`#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 ${version}"\n${"x".repeat(4096)}`),
});
const toolkitDeb = (name: string) => ({
  name: `${name}.deb`,
  mimeType: "application/octet-stream",
  buffer: Buffer.from(`NVM_TEST_DEB\nPackage: ${name}\nVersion: 1.20.1-1\nArchitecture: amd64\n`),
});

const STABLE = "NVIDIA GeForce RTX 3060";
const EXPERIMENTAL = "NVIDIA GeForce RTX 4090";
const RANGE = {
  zh: "解锁支持范围：RTX 30 系列稳定；RTX 40 系列、RTX 20 系列实验性；RTX 50 系列不支持。能不能解锁，程序会按你的显卡自动判断。",
  en: "Unlock support: the RTX 30 series is stable, the RTX 40 and RTX 20 series are experimental, and the RTX 50 series is not supported. The program decides automatically which of your cards can be unlocked.",
} as const;
const MODEL_TEXT = /RTX ?(3060|4090|2080|5090)|0000:/;
const gpuBox = (page: Page, address: string) => page.locator(`[id="gpu-${address}"]`);

async function open(page: Page, language: string, path: string, scenario = "gpu_unlock"): Promise<void> {
  await setScenario(page.request, scenario);
  await page.goto("/"); await signIn(page); await page.goto(path);
  await page.locator("#lang-control").selectOption(language);
}

for (const language of ["zh", "en"]) {
  test(`the unlock is offered by series and reaches the confirmation in ${language}`, async ({ page }) => {
    await open(page, language, "/#/update");
    const box = page.locator("#gpu-unlock");
    // No GPU selected yet: nothing to unlock.
    await expect(box).toBeDisabled();
    await gpuBox(page, "0000:01:00.0").check();
    await gpuBox(page, "0000:02:00.0").check();
    await page.selectOption("#version-select", "610.43.03");
    await expect(page.locator("#version-select option:checked")).toHaveText(language === "zh" ? "610.43.03 · 支持解锁" : "610.43.03 · supports unlock");
    await expect(box).toBeEnabled();
    await expect(box).not.toBeChecked();
    const area = page.locator("#gpu-unlock-area");
    await expect(area).toContainText(RANGE[language as "zh" | "en"]);
    await expect(area).not.toContainText(MODEL_TEXT);

    await box.check();
    await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
    const dialog = page.locator("dialog[open]");
    await expect(dialog).toContainText(language === "zh"
      ? `显卡全速解锁：${STABLE}、${EXPERIMENTAL}，下次开机生效`
      : `Full-speed GPU unlock: ${STABLE}, ${EXPERIMENTAL}. It takes effect the next time the computer starts.`);
    await expect(dialog).toContainText(language === "zh"
      ? "RTX 40 系列的解锁仍在试验阶段，可能不稳定。"
      : "The unlock for the RTX 40 series is still experimental and may be unstable.");
    await expect(dialog).not.toContainText(/RTX 20 系列的解锁|RTX 20 series is still/);
    await expect(dialog).not.toContainText(/tensor|上游|upstream/);
  });

  test(`the upload dialog offers the same unlock choice in ${language}`, async ({ page }) => {
    await open(page, language, "/#/update");
    await gpuBox(page, "0000:01:00.0").check();
    await page.selectOption("#version-select", "610.43.03");
    await page.locator("#gpu-unlock").check();
    await page.getByRole("button", { name: /^Upload files manually…$|^手动上传文件…$/ }).click();
    const box = page.locator("#upload-gpu-unlock");
    // The page and the dialog share one choice.
    await expect(box).toBeChecked();
    const uploadArea = page.locator("#upload-gpu-unlock-area");
    await expect(uploadArea).toContainText(RANGE[language as "zh" | "en"]);
    await expect(uploadArea).not.toContainText(MODEL_TEXT);
  });

  test(`a choice changed in the upload dialog is the one the page shows and plans in ${language}`, async ({ page }) => {
    await open(page, language, "/#/update");
    await gpuBox(page, "0000:01:00.0").check();
    await page.selectOption("#version-select", "610.43.03");
    const outer = page.locator("#gpu-unlock");
    const upload = page.getByRole("button", { name: /^Upload files manually…$|^手动上传文件…$/ });
    const dialog = page.locator("dialog[open]");
    await upload.click();
    await dialog.locator("#upload-gpu-unlock").check();
    await dialog.locator(".upload-footer").getByRole("button", { name: /^Close$|^关闭$/ }).click();
    await expect(outer).toBeChecked();
    await upload.click();
    await dialog.locator("#upload-gpu-unlock").uncheck();
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(outer).not.toBeChecked();
    const planned = page.waitForRequest((r) => r.url().endsWith("/api/plans") && r.method() === "POST");
    await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
    expect((await planned).postDataJSON().source.gpu_unlock).toBe(false);
  });

  test(`a release without the unlock mark keeps the box off in ${language}`, async ({ page }) => {
    await open(page, language, "/#/update");
    await gpuBox(page, "0000:01:00.0").check();
    await page.selectOption("#version-select", "610.43.03");
    const box = page.locator("#gpu-unlock");
    await box.check();
    await page.selectOption("#version-select", "610.57.04");
    await expect(page.locator("#version-select option:checked")).toHaveText("610.57.04");
    await expect(box).toBeDisabled();
    await expect(box).not.toBeChecked();
    const reason = language === "zh"
      ? "这个版本暂不支持解锁。想解锁，请选标有「支持解锁」的版本。"
      : "This version does not support the unlock yet. To unlock, choose a version marked \"supports unlock\".";
    await expect(page.locator("#gpu-unlock-area")).toContainText(reason);
    await page.getByRole("button", { name: /^Upload files manually…$|^手动上传文件…$/ }).click();
    const uploadBox = page.locator("#upload-gpu-unlock");
    await expect(uploadBox).toBeDisabled();
    await expect(uploadBox).not.toBeChecked();
    await expect(page.locator("#upload-gpu-unlock-area")).toContainText(reason);
    await expect(page.locator("#upload-link-version option[value='610.43.03']")).toHaveText(language === "zh" ? "610.43.03 · 支持解锁" : "610.43.03 · supports unlock");
  });

  test(`an uploaded supported release submits the unlock the page release cannot in ${language}`, async ({ page }) => {
    await open(page, language, "/#/update");
    await gpuBox(page, "0000:01:00.0").check();
    await page.selectOption("#version-select", "610.57.04");
    const pageBox = page.locator("#gpu-unlock");
    await expect(pageBox).toBeDisabled();
    await expect(pageBox).not.toBeChecked();
    await page.getByRole("button", { name: /^Upload files manually…$|^手动上传文件…$/ }).click();
    const dialog = page.locator("dialog[open]");
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    await dialog.locator("#upload-files").setInputFiles([installer("610.43.03"), ...TOOLKIT.map(toolkitDeb)]);
    await expect(dialog.locator(".upload-slot-ready")).toHaveCount(5);
    const box = dialog.locator("#upload-gpu-unlock");
    await expect(box).toBeEnabled();
    await box.check();
    await expect(box).toBeChecked();
    await expect(pageBox).toBeDisabled();
    await expect(pageBox).not.toBeChecked();
    const planned = page.waitForRequest((r) => r.url().endsWith("/api/plans") && r.method() === "POST");
    await dialog.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
    const body = (await planned).postDataJSON() as { source: { kind: string; version: string; gpu_unlock: boolean } };
    expect(body.source.kind).toBe("upload");
    expect(body.source.version).toBe("610.43.03");
    expect(body.source.gpu_unlock).toBe(true);
  });

  test(`an RTX 20 card reaches the confirmation with the RTX 20 series warning in ${language}`, async ({ page }) => {
    await open(page, language, "/#/update", "gpu_unlock_rtx20");
    await page.selectOption("#version-select", "610.43.03");
    const box = page.locator("#gpu-unlock");
    await expect(box).toBeEnabled();
    await box.check();
    await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
    const dialog = page.locator("dialog[open]");
    await expect(dialog).toContainText(language === "zh"
      ? "RTX 20 系列的解锁仍在试验阶段，可能不稳定。"
      : "The unlock for the RTX 20 series is still experimental and may be unstable.");
    await expect(dialog).not.toContainText(/RTX 40 系列的解锁|RTX 40 series is still/);
  });

  test(`an RTX 50 card leaves the unlock off and shows the range in ${language}`, async ({ page }) => {
    await open(page, language, "/#/update", "gpu_unlock_rtx50");
    await page.selectOption("#version-select", "610.43.03");
    await expect(page.locator("#gpu-unlock")).toBeDisabled();
    const area = page.locator("#gpu-unlock-area");
    await expect(area).toContainText(RANGE[language as "zh" | "en"]);
    await expect(area).toContainText(language === "zh" ? "所选显卡都不支持解锁。" : "None of the selected GPUs supports the unlock.");
    await expect(area).not.toContainText(MODEL_TEXT);
  });

  test(`the home page reads this boot's unlock result per GPU model in ${language}`, async ({ page }) => {
    await open(page, language, "/#/");
    const reading = page.locator("#home-gpu-unlock");
    await expect(reading).toContainText(language === "zh" ? `${STABLE} 已解锁` : `${STABLE}: unlocked`);
    await expect(reading).toContainText(language === "zh"
      ? `${EXPERIMENTAL} 需要断电后重新开机。请先关机，切断电源，再开机。`
      : `${EXPERIMENTAL}: needs a full power-off. Shut the computer down, switch the power off, then turn it on again.`);
  });
}
