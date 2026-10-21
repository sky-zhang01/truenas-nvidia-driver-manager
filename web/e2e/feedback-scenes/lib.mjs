import { createRequire } from "node:module";
import { resolve } from "node:path";
const R = createRequire(import.meta.url);
export const { chromium } = R("playwright");
export const BASE = "http://127.0.0.1:18109";
export const OUT = process.env.NVM_COPY_SCREENSHOT_DIR ? resolve(process.cwd(), process.env.NVM_COPY_SCREENSHOT_DIR) : new URL("../../../.implementation/release-fixes/shots", import.meta.url).pathname;
export const PASSWORD = "changed-password-1";
export const CHECK = /^检查并继续$/;
export const OPEN = /手动上传文件…/;
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const browsers = new Set();
export async function launch() { const browser = await chromium.launch(); browsers.add(browser); return browser; }
export async function cleanup() { for (const browser of browsers) await browser.close().catch(() => {}); browsers.clear(); }
export async function scenario(name) {
  const r = await fetch(`${BASE}/__fake/scenario`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name }) });
  if (!r.ok) throw new Error("scenario " + name);
  const login = await (await fetch(`${BASE}/api/login`)).json();
  if (login.credentials === "setup_required") {
    const code = (await (await fetch(`${BASE}/__fake/setup-code`)).json()).code;
    const c = await fetch(`${BASE}/api/setup`, { method: "POST", headers: { "content-type": "application/json", Origin: BASE }, body: JSON.stringify({ code, new_password: PASSWORD }) });
    if (c.status !== 200) throw new Error("setup " + c.status);
  }
}
export async function open(browser, name, hash, { width = 1280, height = 900 } = {}) {
  if (name !== "gpu_wait" && name !== "upload_files") await scenario("upload_files"); // clears the Docker GPU users left by gpu_wait
  await scenario(name);
  const ctx = await browser.newContext({ viewport: { width, height }, baseURL: BASE, locale: "zh-CN" });
  await ctx.route("**/*", async (route) => {
    if (new URL(route.request().url()).hostname === "127.0.0.1") await route.continue(); else await route.abort();
  });
  const page = await ctx.newPage();
  await page.goto("/");
  await page.fill("#login-user", "admin");
  await page.fill("#login-pass", PASSWORD);
  await page.locator("form.login-form button[type=submit]").click();
  await page.locator("#login-user").waitFor({ state: "detached" });
  await page.locator("#lang-control").selectOption("zh");
  await page.locator("#theme-control").selectOption("focused-light");
  await page.goto("/#" + hash);
  await page.locator("#lang-control").selectOption("zh");
  return { ctx, page };
}
export const shot = async (page, id, opts = {}) => {
  await page.screenshot({ path: `${OUT}/${id}.png`, ...opts });
  console.log("SCREENSHOT " + id);
};
export async function blur(page) { await page.evaluate(() => document.activeElement && document.activeElement.blur()); }
export async function token(page) { return page.evaluate(() => sessionStorage.getItem("nvm.token")); }
export const PACKAGES = ["libnvidia-container1", "libnvidia-container-tools", "nvidia-container-toolkit-base", "nvidia-container-toolkit"];
export const runFile = (version = "580.178.04", name = "renamed-driver.run", pad = 4096) => ({ name, mimeType: "application/octet-stream", buffer: Buffer.from(`#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 ${version}"\n` + "x".repeat(pad)) });
export const debFile = (pkg, { arch = "amd64", version = "1.20.1-1", name, pad = 0 } = {}) => ({ name: name ?? `${pkg}-renamed.deb`, mimeType: "application/octet-stream", buffer: Buffer.from(`NVM_TEST_DEB\nPackage: ${pkg}\nVersion: ${version}\nArchitecture: ${arch}` + (pad ? " ".repeat(pad) : "\n")) });
export async function openUpload(page) {
  await page.getByRole("button", { name: OPEN }).click();
  const dialog = page.locator("dialog.upload-dialog[open]");
  await dialog.locator("#upload-files").waitFor();
  await sleep(500);
  return dialog;
}
export async function closeUpload(dialog) { await dialog.locator(".upload-footer").getByRole("button", { name: /^关闭$/ }).click(); }
