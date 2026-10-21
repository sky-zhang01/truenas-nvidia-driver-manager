import { launch, open, shot, sleep, openUpload, runFile, debFile, PACKAGES as P } from "./lib.mjs";
const b = await launch();
const text = async (d) => (await d.innerText()).replace(/\n+/g, " | ");
const notes = [];
// uploading with throttle
{
  const { page, ctx } = await open(b, "upload_files", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  const cdp = await ctx.newCDPSession(page);
  await cdp.send("Network.enable");
  await cdp.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: 1_500_000 });
  page.on("dialog", async (dlg) => { notes.push("取消上传确认框(原生 confirm)文字: " + dlg.message()); await dlg.dismiss(); });
  await d.locator("#upload-files").setInputFiles([runFile("580.178.04", "big.run", 16_000_000), ...P.map((p) => debFile(p))]);
  await d.locator("progress").waitFor();
  await sleep(2500);
  await shot(page, "upload-dialog-uploading");
  console.log("UPLOADING", (await text(d)).replace(/^.*?(正在上传)/, "$1").slice(0, 300));
  console.log("FOOTER", await d.locator(".upload-footer").innerText());
  await d.locator(".upload-footer").getByRole("button", { name: "取消上传" }).click(); // application confirmation retains the upload
  await shot(page, "upload-cancel-confirm");
  await d.getByRole("button", { name: "继续上传" }).click();
  await sleep(300);
  await cdp.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await ctx.close();
}
// reading state: the browser has sent every byte, the server has not answered yet.
{
  const { page, ctx } = await open(b, "upload_files", "/update");
  await page.addInitScript(() => {
    const send = XMLHttpRequest.prototype.send; const open = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (m, u, ...r) { this.__u = String(u); return open.call(this, m, u, ...r); };
    XMLHttpRequest.prototype.send = function (body) {
      if (!this.__u.includes(".run") || !body || !body.size) return send.call(this, body);
      this.upload.dispatchEvent(new ProgressEvent("progress", { lengthComputable: true, loaded: body.size, total: body.size }));
      setTimeout(() => send.call(this, body), 6000);
    };
  });
  await page.reload(); await page.locator("#lang-control").selectOption("zh"); await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await d.locator("#upload-files").setInputFiles([runFile("580.178.04", "big.run", 12_000_000), ...P.map((p) => debFile(p))]);
  await d.locator(".upload-progress .hint", { hasText: "正在读取文件" }).waitFor({ timeout: 20000 });
  await sleep(200);
  await shot(page, "upload-dialog-uploading-reading");
  console.log("READING", await d.locator(".upload-progress").innerText());
  await ctx.close();
}
console.log(notes.join("\n"));
await b.close();
