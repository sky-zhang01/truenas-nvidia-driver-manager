import { launch, open, shot, blur, sleep, CHECK, openUpload } from "./lib.mjs";
const b = await launch();
const text = async (d) => (await d.innerText()).replace(/\n+/g, " | ");
const BLOCK = (users) => ({ result: "blocked", failures: [], gpu: { state: "busy", checked_at: 1792209600000, errors: [], users } });
const USERS = [
  { id: "docker:a", name: "gpu-worker", type: "docker", usage: "in_use" },
  { id: "docker:b", name: "media-idle", type: "docker", usage: "requested" },
  { id: "container:1", name: "compute-instance", type: "incus", usage: "in_use" },
];
// ready
{
  const { page, ctx } = await open(b, "upload_stage", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await shot(page, "upload-dialog-ready");
  console.log("READY", (await text(d)).slice(0, 1500));
  await ctx.close();
}
// ready with another page version selected
{
  const { page, ctx } = await open(b, "upload_stage", "/update");
  await page.locator("#version-select").waitFor();
  await page.selectOption("#version-select", "590.48.01"); await blur(page);
  const d = await openUpload(page);
  await shot(page, "upload-dialog-ready-mismatch");
  console.log("RMIS", (await text(d)).slice(0, 700));
  await ctx.close();
}
// footer (a) no GPU selected
{
  const { page, ctx } = await open(b, "mixed_1b80_2b85", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await shot(page, "upload-dialog-footer-reasons-a-gpu");
  console.log("A", await d.locator(".upload-footer").innerText());
  await ctx.close();
}
// footer (b) blocked
{
  const { page, ctx } = await open(b, "no_link", "/update");
  await page.locator("#version-select, #update-blocked").first().waitFor();
  await sleep(500);
  await page.getByRole("button", { name: /手动上传文件/ }).click({ force: true }).catch((e) => console.log("B click", e.message.slice(0, 100)));
  const d = page.locator("dialog.upload-dialog[open]"); await sleep(800);
  console.log("B count", await d.count());
  if (await d.count()) { await shot(page, "upload-dialog-footer-reasons-b-blocked"); console.log("B", await d.locator(".upload-footer").innerText()); }
  await ctx.close();
}
// footer (c) checking
{
  const { page, ctx } = await open(b, "upload_stage", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  let rel; const gate = new Promise((r) => (rel = r));
  await page.route("**/api/plans", async (route) => { if (route.request().method() !== "POST") return route.continue(); try { const r = await route.fetch(); await gate; await route.fulfill({ response: r }); } catch {} });
  await d.getByRole("button", { name: CHECK }).click(); await sleep(700);
  await shot(page, "upload-dialog-footer-reasons-c-checking");
  console.log("C", await d.locator(".upload-footer").innerText());
  rel(); await ctx.close();
}
// footer (d) read failure
{
  const { page, ctx } = await open(b, "upload_stage", "/");
  await page.route("**/api/uploads/current", (r) => r.fulfill({ status: 500, json: { error: "internal", params: {} } }));
  await page.goto("/#/update"); await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await shot(page, "upload-dialog-footer-reasons-d-readfail");
  console.log("D", await d.locator(".upload-footer").innerText(), "|", await d.locator(".upload-read-retry").count());
  await ctx.close();
}
// confirm upload
{
  const { page, ctx } = await open(b, "upload_stage", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await d.getByRole("button", { name: CHECK }).click();
  const c = page.locator("dialog[open]").last(); await sleep(800);
  await shot(page, "confirm-dialog-upload");
  console.log("CONF", (await page.locator("dialog[open]").last().innerText()).replace(/\n+/g, " | "));
  await page.locator("dialog[open] details summary").last().click().catch(() => {}); await sleep(300);
  await shot(page, "confirm-dialog-upload-details");
  await page.locator("dialog[open]").last().getByRole("button", { name: /返回选择/ }).click(); await sleep(600);
  await shot(page, "confirm-dialog-upload-back");
  console.log("BACK", await page.locator("dialog.upload-dialog[open] .upload-footer").innerText());
  await ctx.close();
}
// blocked dialogs
{
  const { page, ctx } = await open(b, "readme_update", "/update");
  await page.locator("#version-select").waitFor();
  await page.route("**/api/plans", (r) => r.request().method() === "POST" ? r.fulfill({ json: BLOCK(USERS) }) : r.continue());
  await page.selectOption("#version-select", "580.178.04"); await blur(page);
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator("dialog[open]").waitFor(); await sleep(600);
  await shot(page, "gpu-blocked-dialog");
  console.log("BLOCKED", (await page.locator("dialog[open]").innerText()).replace(/\n+/g, " | "));
  await ctx.close();
}
{
  const { page, ctx } = await open(b, "upload_stage", "/update");
  await page.locator("#version-select").waitFor();
  await page.route("**/api/plans", (r) => r.request().method() === "POST" ? r.fulfill({ json: BLOCK(USERS) }) : r.continue());
  const d = await openUpload(page);
  await d.getByRole("button", { name: CHECK }).click(); await sleep(900);
  await shot(page, "gpu-blocked-dialog-upload");
  console.log("BLOCKED-UP", (await page.locator("dialog[open]").last().innerText()).replace(/\n+/g, " | "));
  await ctx.close();
}
// mobile ready
{
  const { page, ctx } = await open(b, "upload_stage", "/update", { width: 390, height: 844 });
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await shot(page, "upload-ready-mobile-390");
  const info = await d.locator(".upload-footer button").evaluateAll((ns) => ns.map((n) => [n.textContent, Math.round(n.getBoundingClientRect().height), Math.round(n.getBoundingClientRect().top)]));
  console.log("MOBILE", JSON.stringify(info), JSON.stringify(await d.locator(".upload-dismiss").evaluate((n) => n.getBoundingClientRect().toJSON())));
  await ctx.close();
}
await b.close();
