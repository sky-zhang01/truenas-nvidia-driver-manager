import { launch, open, shot, blur, sleep, OPEN, CHECK, openUpload, runFile, debFile, PACKAGES, token } from "./lib.mjs";
const b = await launch();
const P = PACKAGES;
const sel = (d, files) => d.locator("#upload-files").setInputFiles(files);
const text = async (d) => (await d.innerText()).replace(/\n+/g, " | ");
// partial
{
  const { page, ctx } = await open(b, "upload_files", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await sel(d, [debFile(P[1], { name: "libnvidia-container-tools_1.20.1-1_amd64.deb", pad: 412_000 }), debFile(P[0], { name: "libnvidia-container1_1.20.1-1_amd64.deb", pad: 120_000 })]);
  await d.locator(".upload-slot-ready").nth(1).waitFor(); await sleep(600);
  await shot(page, "upload-dialog-partial");
  console.log("PARTIAL", (await text(d)).slice(0, 1200));
  await ctx.close();
}
// row errors, real
{
  const { page, ctx } = await open(b, "upload_files", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await sel(d, [debFile(P[0], { arch: "arm64" }), debFile("nvidia-container-toolkit-dev", { name: "dev.deb" }), { name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("hello") }, { name: "broken.run", mimeType: "application/octet-stream", buffer: Buffer.from("not an installer at all\n") }]);
  await sleep(1500);
  console.log("ERR", (await text(d)).slice(0, 1800));
  await d.locator(".upload-body").evaluate((n) => { n.scrollTop = 0; });
  await shot(page, "upload-dialog-row-errors-top");
  await d.locator(".upload-body").evaluate((n) => { n.scrollTop = n.scrollHeight; });
  await sleep(200);
  await shot(page, "upload-dialog-row-errors");
  await ctx.close();
}
// error codes via route
{
  const { page, ctx } = await open(b, "upload_files", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await page.route("**/api/uploads/*/files*", async (route) => {
    const u = new URL(route.request().url()); const name = u.searchParams.get("name");
    const rej = (status, reason, extra) => route.fulfill({ status, json: { error: "upload.rejected", params: { reason, file_name: name, ...extra } } });
    if (name.endsWith(".run")) return rej(400, "run_not_linux_x86_64", { kind: "run", package: null });
    if (name.startsWith(P[0])) return rej(400, "deb_version_format", { kind: "deb", package: P[0] });
    if (name.startsWith(P[1])) return rej(500, "write_failed", { kind: "deb", package: P[1] });
    if (name.startsWith(P[2])) return route.abort("failed");
    return route.continue();
  });
  await sel(d, [runFile(), ...P.map((p) => debFile(p, { name: `${p}_1.20.1-1_amd64.deb` }))]);
  await sleep(2000);
  console.log("CODES", (await text(d)).slice(0, 2200));
  await d.locator(".upload-body").evaluate((n) => { n.scrollTop = 0; });
  await shot(page, "upload-dialog-row-errors-codes-top");
  await d.locator(".upload-body").evaluate((n) => { n.scrollTop = n.scrollHeight; });
  await sleep(200);
  await shot(page, "upload-dialog-row-errors-codes");
  await ctx.close();
}
// not_installer on run row
{
  const { page, ctx } = await open(b, "upload_files", "/update");
  const d = await openUpload(page);
  await page.route("**/api/uploads/*/files*", (route) => { const name = new URL(route.request().url()).searchParams.get("name"); return route.fulfill({ status: 400, json: { error: "upload.rejected", params: { reason: "run_not_shebang", file_name: name, kind: "run", package: null } } }); });
  await sel(d, [runFile()]); await sleep(1000);
  await shot(page, "upload-dialog-row-errors-not-installer");
  console.log("NOTINST", (await text(d)).slice(300, 1000));
  await ctx.close();
}
await b.close();
