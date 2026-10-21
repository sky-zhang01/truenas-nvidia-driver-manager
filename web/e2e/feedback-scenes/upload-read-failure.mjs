import { launch, open, shot, sleep, openUpload } from "./lib.mjs";
const b = await launch();
const { page, ctx } = await open(b, "upload_stage", "/");
await page.route("**/api/uploads/current", (r) => r.abort("failed"));
await page.goto("/#/update"); await page.locator("#version-select").waitFor();
const d = await openUpload(page);
await shot(page, "upload-dialog-footer-reasons-d-readfail");
console.log("D", (await d.innerText()).replace(/\n+/g, " | ").slice(0, 400), "|", await d.locator(".upload-footer").innerText());
await b.close();
