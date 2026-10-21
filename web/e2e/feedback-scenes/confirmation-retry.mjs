import { expect } from "@playwright/test";
import { launch, open, shot, CHECK, openUpload } from "./lib.mjs";

const browser = await launch();
{
  const { page, ctx } = await open(browser, "readme_update", "/update");
  await page.selectOption("#version-select", "580.178.04");
  await page.getByRole("button", { name: CHECK }).click();
  const confirmation = page.locator("dialog[open]");
  await expect(confirmation).toBeVisible();
  await page.route("**/api/tasks", route => route.request().method() === "POST"
    ? route.fulfill({ status: 409, json: { error: "plan.changed", params: { changed: ["gpus"] } } })
    : route.continue());
  await confirmation.getByRole("button", { name: "保存原版驱动并开始", exact: true }).click();
  await expect(confirmation.getByRole("button")).toHaveCount(1);
  await expect(confirmation).not.toContainText("plan.changed.item");
  await expect(confirmation.getByRole("button", { name: "关闭", exact: true })).toBeFocused();
  await shot(page, "confirm-rejected-single-close");
  await ctx.close();
}
{
  const { page, ctx } = await open(browser, "upload_stage", "/update");
  const upload = await openUpload(page);
  let plans = 0;
  await page.route("**/api/plans", async route => {
    if (route.request().method() !== "POST") return route.continue();
    plans++;
    if (plans === 1) {
      await route.fulfill({ json: { result: "blocked", failures: [], gpu: { state: "busy", checked_at: 1792209600000, errors: [], users: [{ id: "docker:worker", type: "docker", name: "gpu-worker", usage: "in_use" }] } } });
    } else await route.continue();
  });
  await upload.getByRole("button", { name: CHECK }).click();
  const blocked = page.locator("dialog[open]").last();
  const recheck = blocked.getByRole("button", { name: "重新检查", exact: true });
  await expect(recheck).toBeVisible();
  await shot(page, "upload-recheck-blocked");
  await recheck.click();
  const confirmation = page.locator("dialog[open]").last();
  await expect(confirmation.getByRole("button", { name: "返回选择", exact: true })).toBeVisible();
  expect(plans).toBe(2);
  await shot(page, "upload-recheck-ready");
  await ctx.close();
}
await browser.close();
