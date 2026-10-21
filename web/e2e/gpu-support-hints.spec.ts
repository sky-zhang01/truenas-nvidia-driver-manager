import { test, expect, type Page } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

const GPU = "NVIDIA GeForce RTX 5090";
const OPEN = { address: "0000:01:00.0", device: "2b85", name: GPU, modules: "open_only", first_branch: 570, usable_from_branch: 580, too_new: [], too_old: ["550.163.01"] };
const catalog = { items: ["580.178.04", "570.211.01", "550.163.01"].map(version => ({ version })), open_unbuildable: ["570.211.01", "550.163.01"], unreadable: [], gpus: [OPEN] };
const sentence = (language: string, name: string, branch: number) => language === "zh"
  ? `${name} 从 ${branch} 驱动起支持。` : `${name} is supported from driver ${branch}.`;

async function open(page: Page, language: string, body: unknown) {
  await setScenario(page.request, "fresh");
  await page.route("**/api/versions", route => route.fulfill({ json: body }));
  await page.goto("/"); await signIn(page); await page.goto("/#/update");
  await page.locator("#lang-control").selectOption(language);
  await expect(page.locator("#version-select")).toBeVisible();
}

for (const language of ["zh", "en"]) {
  test(`an open GPU presents the backend usable branch as one sentence in ${language}`, async ({ page }) => {
    await open(page, language, catalog);
    await expect(page.locator(".gpu-notes .notice")).toHaveCount(1);
    await expect(page.locator(".gpu-notes .notice > p")).toHaveText(sentence(language, GPU, 580));
    expect(await page.locator("#version-select option").allTextContents()).toEqual(["580.178.04"]);
    await expect(page.locator(".gpu-notes")).not.toContainText(/570|550|missing|缺少/);
  });

  test(`a closed GPU keeps its backend lower bound despite open exclusions in ${language}`, async ({ page }) => {
    const gpu = { ...OPEN, name: "NVIDIA GeForce GTX 1080", device: "1b80", modules: "closed_only", first_branch: 470, usable_from_branch: 470, too_old: ["450.80.02"] };
    await open(page, language, { ...catalog, items: ["580.178.04", "550.163.01", "470.256.02", "450.80.02"].map(version => ({ version })), open_unbuildable: ["550.163.01", "470.256.02"], gpus: [gpu] });
    await expect(page.locator(".gpu-notes .notice")).toHaveCount(1);
    await expect(page.locator(".gpu-notes .notice > p")).toHaveText(sentence(language, gpu.name, 470));
    expect(await page.locator("#version-select option").allTextContents()).toEqual(["580.178.04", "550.163.01", "470.256.02"]);
  });

  test(`an absent usable branch never invents a support hint in ${language}`, async ({ page }) => {
    const { usable_from_branch: omitted, ...gpu } = OPEN;
    void omitted;
    await open(page, language, { ...catalog, gpus: [gpu] });
    await expect(page.locator(".gpu-notes .notice")).toHaveCount(0);
    expect(await page.locator("#version-select option").allTextContents()).toEqual(["580.178.04"]);
  });

  test(`a catalog with no actually excluded lower version needs no support hint in ${language}`, async ({ page }) => {
    await open(page, language, { ...catalog, items: [{ version: "580.178.04" }], open_unbuildable: [], gpus: [{ ...OPEN, too_old: [] }] });
    await expect(page.locator(".gpu-notes .notice")).toHaveCount(0);
    expect(await page.locator("#version-select option").allTextContents()).toEqual(["580.178.04"]);
  });

  test(`a plan rejection continues to use the hardware first branch in ${language}`, async ({ page }) => {
    await open(page, language, catalog);
    await page.route("**/api/plans", route => route.fulfill({ status: 409, json: { error: "plan.source_invalid.gpu_unsupported", params: { gpus: [{ device: OPEN.device, name: GPU, first_branch: 570, usable_from_branch: 580 }] } } }));
    await page.selectOption("#version-select", "580.178.04");
    await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
    await expect(page.locator(".update-error")).toContainText(sentence(language, GPU, 570));
    await expect(page.locator(".update-error")).not.toContainText(sentence(language, GPU, 580));
  });
}
