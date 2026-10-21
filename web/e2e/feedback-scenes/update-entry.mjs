import { launch, open, shot, blur, sleep } from "./lib.mjs";
import { expect } from "@playwright/test";
const b = await launch();
{
  const { page, ctx } = await open(b, "readme_update", "/update");
  await page.locator("#version-select").waitFor();
  await page.selectOption("#version-select", "580.178.04");
  await blur(page); await sleep(300);
  await shot(page, "update-page-entry", { fullPage: true });
  await ctx.close();
}
for (const [id, list] of [["gpu-not-listed-hint", ["575.64.05", "570.211.01"]], ["gpu-not-listed-hint-none", []]]) {
  const { page, ctx } = await open(b, "readme_update", "/");
  await page.route("**/api/versions", async (route) => {
    const r = await route.fetch(); const j = await r.json(); j.open_unbuildable = list;
    // These legacy scenes deliberately allow 550; keep the backend hint contract consistent.
    expect(j.gpus).toHaveLength(1);
    expect(j.gpus[0].device).toBe("2684");
    j.gpus[0].usable_from_branch = 550;
    await route.fulfill({ response: r, json: j });
  });
  await page.goto("/#/update"); await page.locator("#version-select").waitFor();
  await page.selectOption("#version-select", "580.178.04"); await blur(page); await sleep(300);
  expect(await page.locator("#version-select option").allTextContents()).toEqual(
    ["615.71.09", "610.57.04", "595.104.02", "590.48.01", "580.178.04", "575.64.05", "570.211.01", "565.77", "550.163.01"].filter(version => !list.includes(version)),
  );
  await expect(page.locator(".gpu-notes .notice > p")).toHaveText("NVIDIA GeForce RTX 4090 从 550 驱动起支持。");
  await shot(page, id, { fullPage: true });
  await ctx.close();
}
await b.close();
