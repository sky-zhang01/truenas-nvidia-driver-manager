import { test, expect } from "./local-test.js";
import { setScenario } from "./helpers.js";

test("a Chinese browser sees Chinese, an English browser sees English", async ({ browser, request }) => {
  await setScenario(request, "fresh");

  const zh = await browser.newContext({ locale: "zh-CN" });
  const zhPage = await zh.newPage();
  await zhPage.goto("/");
  await expect(zhPage.locator("html")).toHaveAttribute("lang", "zh-CN");
  await expect(zhPage.getByRole("heading", { level: 2 })).toContainText("登录");
  await zh.close();

  const en = await browser.newContext({ locale: "en-US" });
  const enPage = await en.newPage();
  await enPage.goto("/");
  await expect(enPage.locator("html")).toHaveAttribute("lang", "en");
  await expect(enPage.getByRole("heading", { level: 2 })).toContainText("Sign in");
  await en.close();
});

test("the choice is kept and applies to the current page at once", async ({ page, request }) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  const control = page.locator("#lang-control");
  await control.selectOption("zh");
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-CN");
  await expect(control).toBeFocused();

  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-CN");

  await control.selectOption("en");
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
});

test("values typed but not submitted survive the switch", async ({ page, request }) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await page.fill("#login-user", "admin");
  await page.locator("#lang-control").selectOption("zh");
  await expect(page.locator("#login-user")).toHaveValue("admin");
});
