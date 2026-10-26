import { test, expect, type Page } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

const CHECK = /^(Check and continue|检查并继续)$/;
const OTHER = { zh: "其他版本…", en: "Other version…" };
const PCI_REASON = {
  zh: "内核的 pci_resize_resource() 参数个数与此版本驱动源码中的调用不一致",
  en: "the number of arguments of the kernel's pci_resize_resource() does not match the call in this version's driver source",
};

async function open(page: Page, scenario: string, language: "zh" | "en") {
  await setScenario(page.request, scenario);
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/update");
  await page.locator("#lang-control").selectOption(language);
  await expect(page.locator("#version-select option").last()).toHaveText(OTHER[language]);
  await page.locator("#version-select").selectOption("other");
  const input = page.locator("#other-version-input");
  await expect(input).toBeVisible();
  await expect(input).toHaveAttribute("placeholder", language === "zh" ? "例如 580.159.03" : "For example 580.159.03");
  return input;
}

for (const language of ["zh", "en"] as const) {
  test(`an unreadable version list does not offer other versions in ${language}`, async ({ page }) => {
    await setScenario(page.request, "vfio_1b80_2b85");
    await page.route("**/api/versions", route => route.fulfill({ json: {
      items: [], open_unbuildable: [], gpus: [],
      unreadable: [{ object: "catalog", reason: { key: "reason.io_error", params: {} } }],
    } }));
    await page.goto("/");
    await signIn(page);
    await page.goto("/#/update");
    await page.locator("#lang-control").selectOption(language);
    await expect(page.locator("#version-select")).toBeVisible();
    await expect(page.locator("#version-select option[value=other]")).toHaveCount(0);
    await expect(page.locator("#catalog-describe > p.hint")).toHaveText(language === "zh"
      ? "离线或自备文件请用「手动上传文件…」。"
      : 'For offline or your own files, use "Upload files manually…".');
  });

  test(`conflicting GPUs retain their individual hints without other versions in ${language}`, async ({ page }) => {
    await setScenario(page.request, "mixed_1b80_2b85");
    await page.goto("/");
    await signIn(page);
    await page.goto("/#/update");
    await page.locator("#lang-control").selectOption(language);
    const gpus = page.locator("fieldset.gpu-select input[type=checkbox]");
    await expect(gpus).toHaveCount(2);
    await gpus.nth(0).check();
    await gpus.nth(1).check();
    await expect(page.locator("#version-select option[value=other]")).toHaveCount(0);
    await expect(page.locator("#catalog-describe > p.hint")).toHaveCount(0);
    await expect(page.locator(".gpu-notes .notice > p")).toHaveText(language === "zh"
      ? ["NVIDIA GeForce GTX 1080 最高支持 580 驱动。", "NVIDIA GeForce RTX 5090 从 580 驱动起支持。"]
      : ["NVIDIA GeForce GTX 1080 is supported up to driver 580.", "NVIDIA GeForce RTX 5090 is supported from driver 580."]);
    await expect(page.locator(".conclusion > p")).toHaveText(language === "zh"
      ? "没有能驱动所选显卡的版本。" : "No version can drive the selected GPUs.");
  });

  test(`returning to a listed version hides the other-version input in ${language}`, async ({ page }) => {
    const input = await open(page, "vfio_1b80_2b85", language);
    await input.fill("580.159.03");
    await page.locator("#version-select").selectOption("580.178.04");
    await expect(page.locator("#version-select")).toHaveValue("580.178.04");
    await expect(input).toBeHidden();
  });

  test(`an older official version outside the list reaches confirmation in ${language}`, async ({ page }) => {
    const input = await open(page, "vfio_1b80_2b85", language);
    const check = page.locator(".actions").getByRole("button", { name: CHECK });
    await expect(check).toBeDisabled();
    await input.fill("580.159.03");
    await check.click();
    const dialog = page.locator("dialog[open]");
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText("580.159.03");
    await expect(page.locator(".update-error")).toBeEmpty();
  });

  for (const [version, from] of [["580.119.02", "580.126.09"], ["575.64.05", null]] as const) {
    test(`other official version ${version} shows its complete build rejection in ${language}`, async ({ page }) => {
      const input = await open(page, "vfio_1b80_2b85", language);
      await input.fill(version);
      await page.locator(".actions").getByRole("button", { name: CHECK }).click();
      const text = language === "zh"
        ? `版本 ${version} 无法在本机构建：${PCI_REASON.zh}。${from ? `请改用 ${from} 或同分支更新的版本。` : ""}`
        : `Version ${version} cannot be built on this host: ${PCI_REASON.en}.${from ? ` Use ${from} or a later version of the same branch.` : ""}`;
      await expect(page.locator(".update-error [role=alert] > p")).toHaveText(text);
      await expect(page.locator("dialog[open]")).toHaveCount(0);
    });
  }

  test(`a nonexistent official version is rejected before confirmation in ${language}`, async ({ page }) => {
    const input = await open(page, "vfio_1b80_2b85", language);
    await input.fill("580.999.99");
    await page.locator(".actions").getByRole("button", { name: CHECK }).click();
    await expect(page.locator(".update-error [role=alert] > p")).toHaveText(language === "zh"
      ? "NVIDIA 官网没有版本 580.999.99，请核对版本号。"
      : "NVIDIA does not list version 580.999.99. Check the version number.");
    await expect(page.locator("dialog[open]")).toHaveCount(0);
  });

  test(`an unpatched 470 version gives its replacement and joined reasons in ${language}`, async ({ page }) => {
    const input = await open(page, "kepler", language);
    await input.fill("470.239.06");
    await page.locator(".actions").getByRole("button", { name: CHECK }).click();
    await expect(page.locator(".update-error [role=alert] > p")).toHaveText(language === "zh"
      ? "版本 470.239.06 无法在本机构建：内核已移除驱动源码调用的函数 follow_pfn()；内核的 drm_mode_config_funcs 已移除驱动源码使用的成员 output_poll_changed。请改用 470.256.02 或同分支更新的版本。"
      : "Version 470.239.06 cannot be built on this host: the kernel no longer has the function follow_pfn() that the driver source calls; the kernel's drm_mode_config_funcs no longer has the member output_poll_changed that the driver source uses. Use 470.256.02 or a later version of the same branch.");
  });

  test(`an unsupported official version names the version and GPU limit in ${language}`, async ({ page }) => {
    const input = await open(page, "pascal", language);
    await input.fill("590.48.01");
    await page.locator(".actions").getByRole("button", { name: CHECK }).click();
    const error = page.locator(".update-error");
    await expect(error.locator("[role=alert] > p").first()).toHaveText(language === "zh"
      ? "下列显卡不支持版本 590.48.01。"
      : "The following GPUs do not support version 590.48.01.");
    await expect(error).toContainText(language === "zh"
      ? "NVIDIA GeForce GTX 1080 最高支持 580 驱动。"
      : "NVIDIA GeForce GTX 1080 is supported up to driver 580.");
  });

  test(`an official version below 470 shows the version floor in ${language}`, async ({ page }) => {
    const input = await open(page, "vfio_1b80_2b85", language);
    await input.fill("460.91.03");
    await page.locator(".actions").getByRole("button", { name: CHECK }).click();
    await expect(page.locator(".update-error [role=alert] > p")).toHaveText(language === "zh"
      ? "官网版本须为 470 及以上。" : "Official versions must be 470 or later.");
  });

  test(`the upload other-version input updates filenames and links in ${language}`, async ({ page }) => {
    await setScenario(page.request, "pascal");
    await page.goto("/");
    await signIn(page);
    await page.goto("/#/update");
    await page.locator("#lang-control").selectOption(language);
    await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
    const dialog = page.locator("dialog.upload-dialog[open]");
    const select = dialog.locator("#upload-link-version");
    await expect(select.locator("option[value='615.71.09']")).toHaveCount(1);
    await expect(select.locator("option").last()).toHaveText(OTHER[language]);
    await select.selectOption("other");
    const input = dialog.locator("#upload-other-version-input");
    for (const version of ["575.64.05", "580.200.01"]) {
      await input.fill(version);
      await expect(dialog.locator(".upload-run-download")).toHaveAttribute("href",
        `https://download.nvidia.com/XFree86/Linux-x86_64/${version}/NVIDIA-Linux-x86_64-${version}-no-compat32.run`);
      await expect(dialog.locator(".upload-run-filename")).toHaveText(`NVIDIA-Linux-x86_64-${version}-no-compat32.run`);
    }
  });
}
