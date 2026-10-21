import { launch, open, shot, blur, sleep, CHECK, token, BASE } from "./lib.mjs";
const b = await launch();
const log = [];
// drivers-size-column
{
  const { page, ctx } = await open(b, "drivers_rows", "/");
  const sizes = [350_000_000, 0, 500, 4_100_000, 980_000];
  await page.route("**/api/drivers", async (route) => {
    if (route.request().method() !== "GET") return route.continue();
    const r = await route.fetch(); const j = await r.json();
    j.rows.forEach((row, i) => { row.size = sizes[i % sizes.length]; });
    await route.fulfill({ response: r, json: j });
  });
  await page.goto("/#/drivers"); await page.locator("table").waitFor(); await sleep(500);
  await shot(page, "drivers-size-column", { fullPage: true });
  console.log((await page.locator("main").innerText()).replace(/\n+/g, " | ").slice(0, 900));
  await ctx.close();
}
// drivers-update-waiting
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
  await shot(page, "drivers-update-waiting", { fullPage: true });
  rel(); await ctx.close();
}
// gpu-wait-mobile-390
{
  const { page, ctx } = await open(b, "gpu_wait", "/update", { width: 390, height: 844 });
  await page.locator("#version-select").waitFor();
  const rel = await hold(page);
  await page.selectOption("#version-select", "580.178.04"); await blur(page);
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator(".update-check-status .gpu-user").first().waitFor({ timeout: 7000 });
  await sleep(300);
  await shot(page, "gpu-wait-mobile-390", { fullPage: true });
  const h = await page.getByRole("button", { name: /取消等待/ }).evaluate((n) => n.getBoundingClientRect().height);
  log.push("取消等待按钮高度(390): " + h);
  rel(); await ctx.close();
}
// confirm-dialog-official
{
  const { page, ctx } = await open(b, "readme_update", "/update");
  await page.locator("#version-select").waitFor();
  const reqs = [];
  page.on("request", (r) => { if (r.method() === "POST" && /\/api\/plans/.test(r.url())) reqs.push(`${r.method()} ${new URL(r.url()).pathname}`); });
  await page.selectOption("#version-select", "580.178.04"); await blur(page);
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator("dialog[open]").waitFor();
  await sleep(500);
  await shot(page, "confirm-dialog-official");
  await page.locator("dialog[open]").getByRole("button", { name: /返回选择/ }).click();
  await sleep(500);
  log.push("返回选择后的请求: " + reqs.join(" ; "));
  // Esc path
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator("dialog[open]").waitFor(); await page.keyboard.press("Escape"); await sleep(500);
  log.push("Esc 后的请求(累计): " + reqs.join(" ; "));
  await ctx.close();
}
console.log(log.join("\n"));
await b.close();
