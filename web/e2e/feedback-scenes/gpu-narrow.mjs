import { launch, open, shot, blur, sleep, CHECK } from "./lib.mjs";
const b = await launch();
async function hold(page) {
  let release; const held = new Promise((r) => (release = r));
  await page.route("**/api/plans", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    try { const resp = await route.fetch(); await held; await route.fulfill({ response: resp }); } catch {}
  });
  return release;
}
{
  const { page, ctx } = await open(b, "gpu_wait", "/drivers");
  await page.locator("table").waitFor();
  const rel = await hold(page);
  await page.getByRole("button", { name: "用这个版本检查并继续" }).first().click();
  await page.locator(".gpu-users-row .gpu-user").first().waitFor({ timeout: 7000 });
  await sleep(300);
  await page.evaluate(() => document.querySelectorAll("*").forEach((n) => { if (n.scrollLeft) n.scrollLeft = 0; }));
  await shot(page, "drivers-update-waiting", { fullPage: true });
  console.log(await page.evaluate(() => { const t = document.querySelector("table"); const p = t.parentElement; return [t.scrollWidth, p.clientWidth, p.scrollWidth]; }));
  rel(); await ctx.close();
}
{
  const { page, ctx } = await open(b, "gpu_wait", "/update", { width: 390, height: 844 });
  await page.locator("#version-select").waitFor();
  const rel = await hold(page);
  await page.selectOption("#version-select", "580.178.04"); await blur(page);
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator(".update-check-status .gpu-user").first().waitFor({ timeout: 7000 });
  await sleep(300);
  await page.locator(".update-check-status").evaluate((n) => { const y = n.getBoundingClientRect().top + scrollY - 120; scrollTo(0, y); });
  await sleep(200);
  await shot(page, "gpu-wait-mobile-390");
  await page.getByRole("button", { name: /取消等待/ }).scrollIntoViewIfNeeded();
  await page.evaluate(() => scrollBy(0, 200)); await sleep(200);
  await shot(page, "gpu-wait-mobile-390-cancel");
  rel(); await ctx.close();
}
await b.close();
