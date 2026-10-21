import { launch, open, shot, blur, sleep, OPEN, CHECK, openUpload, runFile, debFile, PACKAGES } from "./lib.mjs";
const b = await launch();
// empty
{
  const { page, ctx } = await open(b, "upload_files", "/update");
  await page.locator("#version-select").waitFor();
  await page.route("**/api/uploads/toolkit-version", (r) => r.fulfill({ json: { version: "1.20.1-1", fallback: false } }));
  await page.reload(); await page.locator("#lang-control").selectOption("zh");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  console.log((await d.innerText()).replace(/\n+/g, " | "));
  await shot(page, "upload-dialog-empty");
  // expand details
  const det = d.locator("details");
  console.log("details:", await det.count());
  for (let i = 0; i < await det.count(); i++) await det.nth(i).locator("summary").click();
  await sleep(300);
  await d.locator(".upload-body").evaluate((n) => { n.scrollTop = n.scrollHeight; });
  await sleep(200);
  await shot(page, "upload-dialog-empty-details");
  await ctx.close();
}
// fallback toolkit version
{
  const { page, ctx } = await open(b, "upload_files", "/");
  await page.route("**/api/uploads/toolkit-version", (r) => r.fulfill({ json: { version: "1.20.1-1", fallback: true } }));
  await page.goto("/#/update"); await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await shot(page, "upload-dialog-empty-fallback");
  console.log("FALLBACK:", (await d.innerText()).replace(/\n+/g, " | ").slice(0, 700));
  await ctx.close();
}
await b.close();
