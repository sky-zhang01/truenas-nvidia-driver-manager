import { launch, open, shot, blur, sleep, CHECK, openUpload, runFile, debFile, PACKAGES as P } from "./lib.mjs";
const b = await launch();
const text = async (d) => (await d.innerText()).replace(/\n+/g, " | ");
const scrollTo = (d, sel) => d.locator(sel).first().evaluate((n) => n.scrollIntoView({ block: "center" }));
// toolkit too old
if (process.env.NVM_VERSION_SHOTS_ONLY !== "1") {
  const { page, ctx } = await open(b, "upload_below_floor", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  console.log("BELOW", (await text(d)).slice(0, 1500));
  await scrollTo(d, ".upload-slot-error");
  await sleep(200);
  await shot(page, "upload-dialog-toolkit-problems");
  await ctx.close();
}
// toolkit mismatch
if (process.env.NVM_VERSION_SHOTS_ONLY !== "1") {
  const { page, ctx } = await open(b, "upload_files", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await d.locator("#upload-files").setInputFiles([runFile(), debFile(P[0]), debFile(P[1]), debFile(P[2]), debFile(P[3], { version: "1.19.0-1" })]);
  await sleep(3000);
  console.log("MISMATCH", (await text(d)).slice(0, 1800));
  await d.locator(".upload-body").evaluate((n) => { n.scrollTop = n.scrollHeight; }); await sleep(200);
  await shot(page, "upload-dialog-toolkit-problems-mismatch");
  await d.locator(".upload-body").evaluate((n) => { n.scrollTop = 0; }); await sleep(200);
  await shot(page, "upload-dialog-toolkit-problems-mismatch-top");
  await ctx.close();
}
// version choice
{
  const { page, ctx } = await open(b, "upload_multi", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  console.log("MULTI", (await text(d)).slice(0, 1600));
  await scrollTo(d, "input[name=upload-version]");
  await sleep(200);
  await shot(page, "upload-dialog-version-choice");
  await ctx.close();
}
{
  const { page, ctx } = await open(b, "upload_manual", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await d.locator("#upload-version-input").waitFor();
  await scrollTo(d, "#upload-version-input"); await sleep(200);
  await shot(page, "upload-dialog-version-manual");
  await d.locator("#upload-version-input").fill("580"); await sleep(300);
  await shot(page, "upload-dialog-version-manual-invalid");
  console.log("MANUAL", await d.locator("#upload-version-input").locator("xpath=..").innerText());
  await ctx.close();
}
await b.close();
