import { test, expect, type APIRequestContext, type Locator, type Page } from "./local-test.js";
import { BASE, setScenario, signIn } from "./helpers.js";

const CLOSED_OPTIONS = ["580.178.04", "575.64.05", "570.211.01", "565.77", "550.163.01", "470.256.02"];
const OPEN_OPTIONS = ["615.71.09", "610.57.04", "595.104.02", "590.48.01", "580.178.04"];
const ALL_OPTIONS = [...OPEN_OPTIONS, ...CLOSED_OPTIONS.slice(1)];

async function apiToken(page: Page): Promise<string> {
  return page.evaluate(() => sessionStorage.getItem("nvm.token") ?? "");
}

async function openSession(page: Page, request: APIRequestContext, scenario: string): Promise<void> {
  await setScenario(request, scenario);
  await page.goto("/");
  await signIn(page);
}

async function openUpdate(page: Page, request: APIRequestContext, scenario: string): Promise<void> {
  await openSession(page, request, scenario);
  await page.goto("/#/update");
  await expect(page.locator("#version-select")).toBeVisible();
}

const NAV_HOME = ".app-nav a[href='#/home']";
const CHECK = /^Check and continue$|^检查并继续$/;
const OPEN_UPLOAD = /Upload files manually…|手动上传文件…/;

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Count the plan requests this page starts. */
function countPlans(page: Page): string[] {
  const posts: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "POST" && req.url().endsWith("/api/plans")) posts.push(req.url());
  });
  return posts;
}

/** A plausible installer header: `#!`, `Linux-x86_64` and a readable label. */
function runFile(version: string): { name: string; mimeType: string; buffer: Buffer } {
  const header = `#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 ${version}"\n`;
  return {
    name: `NVIDIA-Linux-x86_64-${version}.run`,
    mimeType: "application/octet-stream",
    buffer: Buffer.from(header + "x".repeat(4096)),
  };
}

async function openUploadDialog(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: OPEN_UPLOAD }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  return dialog;
}

function options(page: Page): Promise<string[]> {
  return page.locator("#version-select option").allTextContents();
}

test("a Pascal host lists its buildable proprietary branches and unspecified 550", async ({ page, request }) => {
  await openUpdate(page, request, "pascal");
  expect(await options(page)).toEqual(CLOSED_OPTIONS);
  await expect(page.locator(".gpu-notes")).toContainText(/GTX 1080/);
  await expect(page.locator(".gpu-notes")).toContainText("580");
});

test("a Kepler host offers exact 470 and discloses its limits before confirmation", async ({ page, request }) => {
  await openUpdate(page, request, "kepler");
  expect(await options(page)).toEqual(["470.256.02"]);
  await expect(page.locator(".gpu-notes")).toContainText("470");
  await expect(page.locator(".conclusion")).toHaveCount(0);
  const update = page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ });
  await expect(update).toBeDisabled();
  await page.selectOption("#version-select", "470.256.02");
  await expect(update).toBeEnabled();
  const planned = page.waitForResponse((response) => response.url().endsWith("/api/plans") && response.request().method() === "POST");
  await update.click();
  const plan = await (await planned).json();
  expect(plan.dialog.confirmation.original).toBe("save_available");
  expect(plan.dialog.confirmation.buttons).toEqual([
    { button: "save", enabled: true }, { button: "skip", enabled: true },
  ]);
  expect(plan.dialog.confirmation.warnings).toContainEqual({ key: "confirm.community_patch", params: {} });
  const confirm = page.locator("dialog[open]");
  await expect(confirm).toContainText(/Debian community compatibility patches|来自 Debian 的社区兼容补丁/);
  await expect(confirm).toContainText(/IBT enabled|启用 IBT/);
  await expect(confirm.locator("input[type=checkbox]")).toHaveCount(0);
  await confirm.getByRole("button", { name: /Back to selection|返回选择/ }).click();
});

test("two counted GPUs show a checkbox group and an empty list until one is picked", async ({
  page,
  request,
}) => {
  await openUpdate(page, request, "mixed_1b80_2b85");
  const group = page.locator("fieldset.gpu-select");
  await expect(group).toBeVisible();
  await expect(group.locator("input[type=checkbox]")).toHaveCount(2);
  expect(await options(page)).toEqual([]);
  const update = page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ });
  await expect(update).toBeDisabled();
  await expect(update).toHaveAttribute("aria-describedby", "gpu-select");

  await group.locator("input[type=checkbox]").nth(0).check();
  expect(await options(page)).toEqual(CLOSED_OPTIONS);

  await group.locator("input[type=checkbox]").nth(1).check();
  expect(await options(page)).toEqual([]);
  await expect(page.locator(".conclusion")).toContainText(/No version can drive|没有能驱动所选显卡的版本/);
  await expect(page.locator(".gpu-notes .notice")).toHaveCount(2);
  await group.locator("input[type=checkbox]").nth(0).uncheck();
  expect(await options(page)).toEqual(OPEN_OPTIONS);
  await expect(page.locator(".gpu-notes")).toContainText(/is supported from driver 580|从 580 驱动起支持/);
});

test("the upload dialog shares the GPU choice and blocks the conflict", async ({ page, request }) => {
  await openUpdate(page, request, "mixed_1b80_2b85");
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  const boxes = dialog.locator("fieldset.gpu-select input[type=checkbox]");
  await expect(boxes).toHaveCount(2);
  await boxes.nth(0).check();
  await boxes.nth(1).check();
  await expect(dialog.locator(".conclusion")).toContainText(/No version can drive|没有能驱动所选显卡的版本/);
  const uploadUpdate = dialog.getByRole("button", { name: /^Check and continue$|^检查并继续$/ });
  await expect(uploadUpdate).toHaveAttribute("aria-disabled", "true");
  await uploadUpdate.focus();
  await expect(uploadUpdate).toBeFocused();
  await expect(uploadUpdate).toHaveAttribute("aria-describedby", "upload-continue-reason");
});

test("a device outside the table is named by its device id", async ({ page, request }) => {
  await openUpdate(page, request, "mixed_1b07_2684");
  const group = page.locator("fieldset.gpu-select");
  await expect(group).toContainText("10de:1b07");
  await group.locator("input[type=checkbox]").nth(0).check();
  expect(await options(page)).toEqual(CLOSED_OPTIONS);
  await expect(page.locator(".gpu-notes")).toContainText("10de:1b07");
  await expect(page.locator(".gpu-notes")).toContainText("580");
  await group.locator("input[type=checkbox]").nth(1).check();
  expect(await options(page)).toEqual(CLOSED_OPTIONS.slice(0, -1));
  await group.locator("input[type=checkbox]").nth(0).uncheck();
  expect(await options(page)).toEqual(OPEN_OPTIONS);
  await expect(page.locator(".gpu-notes .notice")).toHaveCount(1);
  await expect(page.locator(".gpu-notes")).toContainText("580");
});

test("a GPU bound to vfio-pci is not counted", async ({ page, request }) => {
  await openUpdate(page, request, "vfio_1b80_2b85");
  await expect(page.locator("fieldset.gpu-select")).toHaveCount(0);
  expect(await options(page)).toEqual(OPEN_OPTIONS);
  await expect(page.locator(".gpu-notes .notice")).toHaveCount(1);
  await expect(page.locator(".gpu-notes")).toContainText(/is supported from driver 580|从 580 驱动起支持/);
});

test("no counted GPU lists all ten candidates, including 470 and the open exclusions", async ({ page, request }) => {
  await openUpdate(page, request, "fresh");
  expect(await options(page)).toEqual(ALL_OPTIONS);
  const response = await page.request.get(`${BASE}/api/versions`, {
    headers: { "X-NVM-Token": await apiToken(page) },
  });
  expect(response.ok()).toBeTruthy();
  const body = await response.json();
  expect(body.items).toEqual(ALL_OPTIONS.map((version) => ({ version })));
  expect(body.open_unbuildable).toEqual(CLOSED_OPTIONS.slice(1, -1));
  await expect(page.locator(".gpu-notes .notice")).toHaveCount(0);
});

test("all GPUs bound to vfio leave the module type unspecified", async ({ page, request }) => {
  await openUpdate(page, request, "vfio_1b80_2b85");
  const res = await request.post(`${BASE}/__fake/driver`, {
    data: { address: "0000:02:00.0", driver: "vfio-pci" },
  });
  expect(res.ok()).toBeTruthy();
  await page.reload();
  await expect(page.locator("#version-select")).toBeVisible();
  expect(await options(page)).toEqual(ALL_OPTIONS);
  await expect(page.locator("fieldset.gpu-select")).toHaveCount(0);
  await expect(page.locator(".gpu-notes .notice")).toHaveCount(0);
});

test("kernel 6.12.96 with a Blackwell GPU drops the earlier branches", async ({ page, request }) => {
  await openUpdate(page, request, "blackwell_612_96");
  const list = await options(page);
  expect(list).not.toContain("565.77");
  expect(list).not.toContain("550.163.01");
  expect(list.length).toBeGreaterThan(5);
  await expect(page.locator(".gpu-notes")).toContainText(
    /is supported from driver 570|从 570 驱动起支持/,
  );
});

test("the update link next to the list points at the NVIDIA website", async ({ page, request }) => {
  await openUpdate(page, request, "pascal");
  // The single NVIDIA website link sits beside the action buttons and opens a new tab.
  const link = page
    .locator("a[href='https://www.nvidia.com/en-us/drivers/unix/#linux64']")
    .filter({ hasText: /NVIDIA website|NVIDIA 官网/ });
  await expect(link).toHaveCount(1);
  await expect(link).toHaveText(/NVIDIA website|NVIDIA 官网/);
  await expect(link).toHaveAttribute("target", "_blank");
  await expect(link).toHaveAttribute("rel", "noopener noreferrer");
});

import { mkdirSync, mkdtempSync, rmSync, truncateSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

test("a plan change after the dialog opened uses the page-open wording", async ({
  page,
  request,
}) => {
  await openUpdate(page, request, "pascal");
  await page.selectOption("#version-select", "580.178.04");
  await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
  const confirm = page.locator("dialog[open]");
  await expect(confirm.locator(".dialog-title")).toBeVisible();
  // Close the dialog, hand the selected GPU to vfio-pci, and ask plan again: the counted GPU set
  // changed after the page was opened.
  await confirm.getByRole("button", { name: /Back to selection|返回选择/ }).click();
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  const res = await request.post(`${BASE}/__fake/driver`, {
    data: { address: "0000:01:00.0", driver: "vfio-pci" },
  });
  expect(res.ok()).toBeTruthy();
  await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
  const error = page.locator(".update-error");
  await expect(error).toContainText("after the page was opened");
  await expect(error).toContainText("Selected GPUs");
  await expect(error).not.toContainText("confirmed in the dialog");

  await expect(page.locator("fieldset.gpu-select")).toHaveCount(0);
});

for (const [scenario, version, moduleType, gpus] of [
  ["pascal", "575.64.05", "proprietary", [{ address: "0000:01:00.0", device: "1b80" }]],
  ["vfio_1b80_2b85", "580.178.04", "open", [{ address: "0000:02:00.0", device: "2b85" }]],
  ["fresh", "550.163.01", null, []],
] as const) {
  test(`submitting ${scenario} ${version} preserves the module choice and focuses the task status`, async ({ page, request }) => {
    await openUpdate(page, request, scenario);
    await page.selectOption("#version-select", version);
    await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
    const confirm = page.locator("dialog[open]");
    await expect(confirm).toContainText(version);
    await confirm.getByRole("button", { name: /Save original driver and start|保存原版驱动并开始/ }).click();
    await expect(page.locator("dialog[open]")).toHaveCount(0);
    await expect(page).toHaveURL(/#\/tasks\//);
    await expect(page.locator(".task-state")).toBeFocused();
    const taskId = page.url().split("#/tasks/")[1];
    const response = await page.request.get(`${BASE}/api/tasks/${taskId}`, {
      headers: { "X-NVM-Token": await apiToken(page) },
    });
    expect(response.ok()).toBeTruthy();
    const detail = await response.json();
    expect(detail.plan.version).toBe(version);
    expect(detail.plan.kernel_module_type).toBe(moduleType);
    expect(detail.plan.gpus).toEqual(gpus);
  });
}

test("a file over 1 GiB is refused before any upload request", async ({ page, request }) => {
  await openUpdate(page, request, "fresh");
  const puts: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "PUT" && req.url().includes("/api/uploads/")) puts.push(req.url());
  });
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  const dir = mkdtempSync(join(tmpdir(), "nvm-e2e-"));
  const big = join(dir, "NVIDIA-Linux-x86_64-580.178.04.run");
  writeFileSync(big, "");
  truncateSync(big, 1024 ** 3 + 1);
  try {
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    await dialog.locator("#upload-files").setInputFiles(big);

    await expect(dialog.locator(".upload-slot-error, .upload-unused")).toContainText("exceeds the 1 GiB limit");
    expect(puts).toEqual([]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a .run without Linux-x86_64 is rejected with its own message", async ({ page, request }) => {
  await openUpdate(page, request, "fresh");
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  // A 10 MB file whose header is not a Linux x86_64 installer.
  const body = Buffer.alloc(10 * 1024 * 1024, 0x41);
  body.write("#!/bin/sh\nlabel=\"something 580.178.04\"\n");
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles({
    name: "bad-header.run",
    mimeType: "application/octet-stream",
    buffer: body,
  });

  await expect(dialog.locator("#upload-slot-run")).toContainText("not a Linux x86_64 driver");
  await expect(dialog.locator("#upload-slot-run")).toContainText("Linux 64-bit");
});

test("a toolkit below the security floor is shown in the dialog", async ({ page, request }) => {
  await openUpdate(page, request, "upload_below_floor");
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog.locator(".upload-summary")).toContainText("too old");
  await expect(dialog.locator(".upload-summary")).toContainText("1.19.0");
});

test("several candidates are radios and none takes a manual version", async ({ page, request }) => {
  await openUpdate(page, request, "upload_multi");
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  let dialog = page.locator("dialog[open]");
  await expect(dialog.locator("input[name='upload-version']")).toHaveCount(2);
  await expect(dialog.locator("#upload-version-input")).toHaveCount(0);
  await dialog.locator(".upload-footer").getByRole("button", { name: /Close|关闭/ }).click();

  await openUpdate(page, request, "upload_manual");
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  dialog = page.locator("dialog[open]");
  const input = dialog.locator("#upload-version-input");
  await expect(input).toBeVisible();
  await input.fill("5.x");
  await expect(dialog.locator("#upload-version-invalid")).toContainText(
    "two to four groups of digits",
  );
  await expect(dialog.getByRole("button", { name: /^Check and continue$|^检查并继续$/ })).toHaveAttribute("aria-disabled", "true");
  await input.fill("580.178.04");
  await expect(dialog.locator("#upload-version-invalid")).toHaveText("");
  await expect(dialog.getByRole("button", { name: /^Check and continue$|^检查并继续$/ })).toHaveAttribute("aria-disabled", "false");
});

test("back and cancel leave the dialog without a submit", async ({ page, request }) => {
  await openUpdate(page, request, "pascal");
  const posts: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "POST" && req.url().endsWith("/api/tasks")) posts.push(req.url());
  });
  await page.selectOption("#version-select", "580.178.04");
  await page.getByRole("button", { name: /^Check and continue$|^检查并继续$/ }).click();
  const confirm = page.locator("dialog[open]");
  // The plan dialog is created after the plan answer arrives, so wait for the dialog itself before
  // reading its buttons.
  await expect(confirm).toBeVisible();
  await expect(confirm.locator("#confirm-how")).toContainText(
    /Downloads the installer from the NVIDIA website|从 NVIDIA 官网下载安装包/,
  );

  await expect(confirm.getByRole("button", { name: /Save original driver and start|保存原版驱动并开始/ })).toBeVisible();
  await expect(confirm.getByRole("button", { name: /Start without saving|不保存原版驱动并开始/ })).toBeVisible();
  await expect(confirm.getByRole("button")).toHaveCount(3);
  await confirm.getByRole("button", { name: /Back to selection|返回选择/ }).click();
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  expect(posts).toEqual([]);
});

test("a late 401 of the update read does not end the session", async ({ page, request }) => {
  await openSession(page, request, "fresh");
  await page.goto("/#/home");
  await expect(page.locator(NAV_HOME)).toBeVisible();

  let reads = 0;
  await page.route("**/api/versions", async (route) => {
    reads += 1;
    await delay(1200);
    await route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ error: "auth.required", params: {} }),
    });
  });

  // Hash navigation keeps the document (and the in-flight read) alive.
  await page.locator(".app-nav a[href='#/update']").click();
  await page.locator(NAV_HOME).click();
  await page.waitForTimeout(2500);

  expect(reads).toBe(1);
  // The 401 answered the page that was left, so this session is still signed in.
  await expect(page.locator("#login-user")).toHaveCount(0);
});

test("a ready plan answer that lands after the page was left opens no dialog", async ({
  page,
  request,
}) => {
  await openUpdate(page, request, "pascal");
  await page.route("**/api/plans", async (route) => {
    await delay(1200);
    await route.continue();
  });
  await page.selectOption("#version-select", "580.178.04");
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator(NAV_HOME).click();
  await expect(page.locator("#version-select")).toHaveCount(0);
  await page.waitForTimeout(2000);

  await expect(page.locator("dialog[open]")).toHaveCount(0);
});

test("a 401 plan answer that lands after the page was left does not end the session", async ({
  page,
  request,
}) => {
  await openUpdate(page, request, "pascal");
  await page.route("**/api/plans", async (route) => {
    await delay(1200);
    await route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ error: "auth.required", params: {} }),
    });
  });
  await page.selectOption("#version-select", "580.178.04");
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator(NAV_HOME).click();
  await page.waitForTimeout(2200);

  await expect(page.locator("#login-user")).toHaveCount(0);
});

test("a double click on the update button sends one plan request", async ({ page, request }) => {
  await openUpdate(page, request, "pascal");
  const posts = countPlans(page);
  await page.route("**/api/plans", async (route) => {
    await delay(1500);
    await route.continue();
  });
  await page.selectOption("#version-select", "580.178.04");
  await page.getByRole("button", { name: CHECK }).dblclick();
  await page.waitForTimeout(2200);

  expect(posts.length).toBe(1);
});

test("an uploaded installer discloses kernel loading before submission", async ({ page, request }) => {
  await openUpdate(page, request, "upload_stage");
  const dialog = await openUploadDialog(page);
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles(runFile("580.178.04"));

  await expect(dialog.locator(".upload-summary")).toContainText("NVIDIA-Linux-x86_64-580.178.04.run");
  const planned = page.waitForResponse((response) => response.url().endsWith("/api/plans") && response.request().method() === "POST");
  await dialog.getByRole("button", { name: CHECK }).click();
  const plan = await (await planned).json();
  expect(plan.dialog.source.kind).toBe("upload");
  expect(plan.dialog.confirmation.warnings).toContainEqual({ key: "confirm.upload_trust", params: {} });
  const confirm = page.locator("dialog[open]");
  await expect(confirm).toContainText("The uploaded installer is built on this host and loaded into its kernel, and the program does not verify NVIDIA signatures. Use an installer downloaded from the NVIDIA website.");
  await confirm.getByRole("button", { name: /Back to selection|返回选择/ }).click();
  await page.locator("#lang-control").selectOption("zh");
  const chineseUpload = page.locator("dialog.upload-dialog[open]");
  await expect(chineseUpload).toBeVisible();
  await chineseUpload.getByRole("button", { name: CHECK }).click();
  await expect(page.locator("dialog[open]")).toContainText("上传的安装包将在本机构建并加载进内核，程序不验证 NVIDIA 签名。请使用从 NVIDIA 官网下载的安装包。");
});

test("a double click in the upload dialog sends one plan request", async ({ page, request }) => {
  await openUpdate(page, request, "upload_stage");
  const dialog = await openUploadDialog(page);
  const posts = countPlans(page);
  await page.route("**/api/plans", async (route) => {
    await delay(1500);
    await route.continue();
  });
  const check = dialog.getByRole("button", { name: CHECK });
  await expect(check).toHaveAttribute("aria-disabled", "false", { timeout: 10_000 });
  await check.dblclick();
  await page.waitForTimeout(2200);

  expect(posts.length).toBe(1);
});

test("Escape stops the running batch and a reopened dialog uploads again", async ({
  page,
  request,
}) => {
  await openUpdate(page, request, "fresh");
  const dialog = await openUploadDialog(page);
  const puts: string[] = [];
  const failed: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "PUT" && req.url().includes("/api/uploads/")) puts.push(req.url());
  });
  page.on("requestfailed", (req) => {
    if (req.method() === "PUT" && req.url().includes("/api/uploads/")) failed.push(req.url());
  });
  // The first answer is held back, so its request is still running when Escape is pressed; every later
  // request passes through.
  let firstHeld = false;
  await page.route("**/api/uploads/*/files*", async (route) => {
    if (route.request().method() !== "PUT" || firstHeld) {
      await route.continue().catch(() => {});
      return;
    }
    firstHeld = true;
    await delay(2000);
    // The page may have aborted this request in the meantime.
    await route.continue().catch(() => {});
  });

  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04"), runFile("580.178.05")]);

  await expect.poll(() => puts.length, { timeout: 10_000 }).toBe(1);
  await page.keyboard.press("Escape");
  await dialog.locator(".upload-cancel-confirm").getByRole("button", { name: /^Confirm cancel$|^确认取消上传$/ }).click();
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  await page.waitForTimeout(3000);

  expect(puts.length).toBe(1);
  expect(failed.length).toBe(1);

  const reopened = await openUploadDialog(page);
  await expect(reopened.locator("#upload-files")).toBeEnabled();
  await reopened.locator("#upload-files").setInputFiles([runFile("580.178.06")]);

  await expect(reopened.locator(".upload-summary")).toContainText(
    "NVIDIA-Linux-x86_64-580.178.06.run",
    { timeout: 10_000 },
  );
});

test("local replacements and a return to stock use the shared confirmation and verified readings", async ({ page, request }) => {
  await setScenario(request, "blackwell_612_96");
  await openSession(page, request, "local_replacement");
  const submitted: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "POST" && req.url().endsWith("/api/tasks")) submitted.push(req.url());
  });
  for (const [index, version] of ["580.178.04", "580.173.02"].entries()) {
    await page.goto("/#/drivers");
    const row = page.locator("table.drivers tbody tr").filter({ hasText: version });
    const planned = page.waitForResponse((res) => res.url().endsWith("/api/plans") && res.request().method() === "POST");
    await row.getByRole("button", { name: index === 1 ? /^Restore this version$|^换回此版本$/ : /^Check this version and continue$|^用这个版本检查并继续$/ }).click();
    const plan = await (await planned).json();
    expect(plan.dialog.confirmation.original).toBe("saved");
    expect(plan.dialog.confirmation.buttons).toEqual([{ button: "start", enabled: true }]);
    const confirm = page.locator("dialog[open]");
    await expect(confirm).toContainText(version);
    await expect(confirm.locator(".confirm-original")).toContainText("580.173.02");
    expect(submitted).toHaveLength(index);
    await confirm.getByRole("button", { name: /Start update|开始更新/ }).click();
    await expect(page).toHaveURL(/#\/tasks\//);
    await expect(page.locator(".task-state")).toContainText(/Succeeded|成功/, { timeout: 15_000 });
    const taskId = page.url().split("#/tasks/")[1];
    const response = await page.request.get(`${BASE}/api/tasks/${taskId}`, {
      headers: { "X-NVM-Token": await apiToken(page) },
    });
    const detail = await response.json();
    expect(detail.status).toBe("SUCCEEDED");
    expect(detail.checks.at(-1)).toMatchObject({ pass: "update", expected: version, passed: true,
      refresh_ok: true, version_ok: true, nvidia_smi: { value: version }, proc_version: { value: version } });
    await expect(page.locator(".task-details")).toContainText(version);
  }
  expect(submitted).toHaveLength(2);
});


const RELEASE_MATERIAL = /^Release recovery material$|^释放恢复材料$/;

function retainedTask(status = "INTERRUPTED", kept = true) {
  return {
    task_id: "retained-task", status, phase: "write", created_at: 1792209600000,
    finished_at: null, interrupt_discovered_at: 1792209700000,
    recovery_material_kept: kept, state_note: null, plan: null,
    loaded_version_start: null, loaded_version_end: null, download: null,
    replace: [], checks: [], restore: null, error: null, gpu_users: null,
    version_change: false, notice: null, unit_query: null,
    log: { lines: [], next: 0, reset: false },
  };
}

test("retained task release requires confirmation and renders the confirmed release history", async ({ page, request }) => {
  await openSession(page, request, "fresh");
  await page.route("**/api/tasks/retained-task", (route) => route.fulfill({ json: retainedTask() }));
  let posts = 0;
  let finish: (() => void) | undefined;
  const gate = new Promise<void>((resolve) => { finish = resolve; });
  await page.route("**/api/tasks/retained-task/release", async (route) => {
    posts += 1;
    expect(route.request().method()).toBe("POST");
    expect(route.request().postData()).toBeNull();
    expect(route.request().headers()["x-nvm-token"]).toBe(await apiToken(page));
    await gate;
    await route.fulfill({ json: {
      ...retainedTask("INTERRUPTED", false), released_by: "successful-task", released_at: 1792209800000,
    } });
  });
  await page.goto("/#/tasks/retained-task");
  const button = page.getByRole("button", { name: RELEASE_MATERIAL });
  await expect(button).toBeVisible();
  await button.click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText(/Task records and logs are kept|任务记录和日志保留/);
  expect(posts).toBe(0);
  await dialog.getByRole("button", { name: /Cancel|取消/ }).click();
  expect(posts).toBe(0);
  await expect(button).toBeFocused();
  await button.click();
  const confirm = dialog.getByRole("button", { name: RELEASE_MATERIAL });
  await confirm.click();
  await expect(confirm).toBeDisabled();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeVisible();
  finish?.();
  await expect(dialog).toHaveCount(0);
  await expect(page.locator(".task-recovery")).toContainText(/Recovery material was released|恢复材料已于/);
  await expect(page.locator(".task-recovery")).toContainText("successful-task");
  await expect(button).toHaveCount(0);
  expect(posts).toBe(1);
});

test("retained task release explains refusals and allows a retry without losing history", async ({ page, request }) => {
  const screenshotDir = process.env["NVM_COPY_SHOTS"] === "1" ? process.env["NVM_COPY_SCREENSHOT_DIR"] : undefined;
  if (screenshotDir) {
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.addInitScript(() => {
      localStorage.setItem("nvm.lang", "zh");
      localStorage.setItem("nvm.theme", "focused-light");
    });
  }
  await openSession(page, request, "fresh");
  await page.route("**/api/tasks/retained-task", (route) => route.fulfill({ json: retainedTask() }));
  let error = "release.no_later_success";
  await page.route("**/api/tasks/retained-task/release", (route) => route.fulfill({
    status: 409, json: { error, params: {}, raw: "release diagnostic" },
  }));
  await page.goto("/#/tasks/retained-task");
  await page.getByRole("button", { name: RELEASE_MATERIAL }).click();
  const dialog = page.getByRole("dialog");
  const confirm = dialog.getByRole("button", { name: RELEASE_MATERIAL });
  const refusals = [
    ["release.no_later_success", /no later-created task that completed a driver replacement and passed its checks is available for verification|没有更晚创建、已完成驱动替换并通过检查的任务可供核对/],
    ["release.driver_mismatch", /do not match a later successful task|与更晚的成功任务记录不符/],
    ["release.host_residue", /writable mounts left by the program|仍有程序留下的可写挂载/],
    ["release.task_running", /while a task is running|有任务正在运行/],
    ["release.lock_busy", /another operation is in progress|另一个操作正在进行/],
    ["release.state_unknown", /could not be confirmed|无法确认任务/],
    ["release.record_unreadable", /unreadable or untrusted|不可读或不可信/],
    ["release.recovery_pending", /recovery record copies remain or state records cannot be read|仍有恢复记录副本，或状态记录无法读取/],
    ["release.not_terminal", /has not finished|尚未结束/],
    ["release.write_failed", /could not be recorded|无法记录/],
    ["release.cleanup_failed", /could not be fully removed|未能全部删除/],
  ] as const;
  for (const [key, text] of refusals) {
    error = key;
    await confirm.click();
    await expect(dialog.locator(".dialog-error")).toContainText(text);
    await expect(confirm).toBeEnabled();
    await expect(dialog.locator("pre.raw-output")).toContainText("release diagnostic");
    await expect(dialog.getByRole("alert")).not.toContainText("release diagnostic");
    if (screenshotDir && (key === "release.no_later_success" || key === "release.recovery_pending")) {
      mkdirSync(screenshotDir, { recursive: true });
      await page.evaluate(() => document.fonts.ready);
      await page.screenshot({ path: join(screenshotDir, `${key.replaceAll(".", "-").replaceAll("_", "-")}.png`), fullPage: true });
    }
  }
  await expect(page.locator(".log-tail")).toBeVisible();
});

test("running tasks and tasks without retained material do not offer recovery release", async ({ page, request }) => {
  await openSession(page, request, "fresh");
  let detail = retainedTask("RUNNING");
  await page.route("**/api/tasks/retained-task*", (route) => route.fulfill({ json: detail }));
  await page.goto("/#/tasks/retained-task");
  await expect(page.locator(".task-state")).toContainText(/Running|进行中/);
  await expect(page.getByRole("button", { name: RELEASE_MATERIAL })).toHaveCount(0);
  detail = retainedTask("INTERRUPTED", false);
  await page.reload();
  await expect(page.locator(".task-state")).toContainText(/Interrupted|中断/);
  await expect(page.getByRole("button", { name: RELEASE_MATERIAL })).toHaveCount(0);
});

test("retained task release closes its dialog when the session expires", async ({ page, request }) => {
  await openSession(page, request, "fresh");
  await page.route("**/api/tasks/retained-task", (route) => route.fulfill({ json: retainedTask() }));
  await page.route("**/api/tasks/retained-task/release", (route) => route.fulfill({
    status: 401, json: { error: "auth.required", params: {} },
  }));
  await page.goto("/#/tasks/retained-task");
  await page.getByRole("button", { name: RELEASE_MATERIAL }).click();
  await page.getByRole("dialog").getByRole("button", { name: RELEASE_MATERIAL }).click();
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

for (const [scenario, kernel, version] of [
  ["fresh", "6.12.105-production+truenas", "25.10.7"],
  ["truenas27", "6.18.52-production+truenas", "27.0.0-RC.1"],
]) {
  test(`kernel compiler scene ${scenario} reaches a plan`, async ({ page, request }) => {
    await openSession(page, request, scenario);
    await expect(page.locator("main")).toContainText(version!);
    await page.goto("/#/update");
    await expect(page.locator("#version-select")).toBeVisible();
    await page.selectOption("#version-select", "580.178.04");
    const response = page.waitForResponse((r) => r.url().endsWith("/api/plans") && r.request().method() === "POST");
    await page.getByRole("button", { name: CHECK }).click();
    const plan = await (await response).json();
    expect(plan.dialog.kernel).toBe(kernel);
    expect(plan.dialog.truenas_version).toBe(version);
    await expect(page.locator("dialog[open]")).toBeVisible();
  });
}

test("task GPU diagnostic renders names, PCI addresses and read failures without changing status", async ({ page, request }) => {
  await openSession(page, request, "fresh");
  let reading: unknown = { value: [
    { name: "NVIDIA Example GPU", pci_address: "00000000:01:00.0" },
    { name: "NVIDIA Other GPU", pci_address: "00000000:02:00.0" },
  ] };
  await page.route("**/api/tasks/gpu-diagnostic*", (route) => route.fulfill({ json: {
    ...retainedTask("SUCCEEDED", false), task_id: "gpu-diagnostic", nvidia_smi_gpus: reading,
  } }));
  await page.goto("/#/tasks/gpu-diagnostic");
  const line = page.locator(".task-gpu-diagnostic");
  await expect(line).toContainText("NVIDIA Example GPU (00000000:01:00.0)");
  await expect(line).toContainText("NVIDIA Other GPU (00000000:02:00.0)");
  await expect(line).toContainText(/diagnostic only|仅供诊断/);
  reading = { unknown: { key: "reason.exit_status", params: { code: 9 } } };
  await page.reload();
  await expect(line).toContainText(/Could not read|无法读取/);
  await expect(line).toContainText("9");
  await expect(page.locator(".task-state")).toContainText(/Succeeded|成功/);
  await page.screenshot({ path: test.info().outputPath("diagnostic-desktop.png"), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: test.info().outputPath("diagnostic-mobile.png"), fullPage: true });
});

test("the 27 kernel scene persists and displays the runner GPU diagnostic", async ({ page, request }) => {
  await openSession(page, request, "truenas27_replacement");
  await expect(page.locator("main")).toContainText("27.0.0-RC.1");
  await page.goto("/#/drivers");
  const row = page.locator("table.drivers tbody tr").filter({ hasText: "580.178.04" });
  await row.getByRole("button", { name: /^Check this version and continue$|^用这个版本检查并继续$/ }).click();
  await page.locator("dialog[open]").getByRole("button", { name: /Start update|开始更新/ }).click();
  await expect(page).toHaveURL(/#\/tasks\//);
  await expect(page.locator(".task-state")).toContainText(/Succeeded|成功/, { timeout: 15_000 });
  const taskId = page.url().split("#/tasks/")[1];
  const response = await page.request.get(`${BASE}/api/tasks/${taskId}`, {
    headers: { "X-NVM-Token": await apiToken(page) },
  });
  expect(response.ok()).toBeTruthy();
  const detail = await response.json();
  expect(detail.plan.kernel).toBe("6.18.52-production+truenas");
  expect(detail.plan.truenas_version).toBe("27.0.0-RC.1");
  expect(detail.nvidia_smi_gpus).toEqual({ value: [
    { name: "NVIDIA Example GPU", pci_address: "00000000:01:00.0" },
  ] });
  await expect(page.locator(".task-gpu-diagnostic")).toContainText("NVIDIA Example GPU (00000000:01:00.0)");
  await expect(page.locator(".task-gpu-diagnostic")).toContainText(/diagnostic only|仅供诊断/);
  const state = await (await request.get(`${BASE}/__fake/state`)).json();
  expect(state.commands.some((args: string[]) => args[0] === "midclt" && args[2] === "container.query")).toBeTruthy();
  expect(state.commands.some((args: string[]) => args[0] === "midclt" && args[2] === "virt.instance.query")).toBeFalsy();
});

for (const restore of [false, true]) {
  test(`a module race displays its independent ${restore ? "restore" : "update"} correction stage`, async ({ page, request }) => {
    await openSession(page, request, restore ? "module_race_restore" : "module_race_update");
    await page.goto("/#/drivers");
    const row = page.locator("table.drivers tbody tr").filter({ hasText: "580.178.04" });
    await row.getByRole("button", { name: /^Check this version and continue$|^用这个版本检查并继续$/ }).click();
    await page.locator("dialog[open]").getByRole("button", { name: /Start update|开始更新/ }).click();
    await expect(page).toHaveURL(/#\/tasks\//);
    await expect(page.locator(".task-state")).toContainText(restore ? /Original driver restored|原版.*恢复|已恢复原版/ : /Succeeded|成功/, { timeout: 15_000 });
    const id = page.url().split("#/tasks/")[1];
    const detail = await (await page.request.get(`${BASE}/api/tasks/${id}`, {
      headers: { "X-NVM-Token": await apiToken(page) },
    })).json();
    expect(detail.status).toBe(restore ? "STOCK_RESTORED" : "SUCCEEDED");
    const record = detail.replace.at(-1);
    expect(record.module_correction.trigger).toBe("version_mismatch");
    expect(record.module_correction.reloaded.every((r: { ok: boolean }) => r.ok)).toBe(true);
    await expect(page.locator(".task-steps")).toContainText(/核对已加载的驱动模块|Aligning loaded driver modules/);
    if (restore) await expect(page.locator(".task-steps")).toContainText(/恢复原版驱动|Restoring the original driver/);
    expect(detail.checks.at(-1).passed).toBe(true);
  });
}

for (const source of ["official", "local"] as const) {
  test(`${source} check shows live usage labels before the plan response`, async ({ page, request }) => {
    await openSession(page, request, "gpu_wait");
    await page.goto(source === "official" ? "/#/update" : "/#/drivers");
    let release = (): void => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await page.route("**/api/plans", async (route) => {
      const response = await route.fetch();
      await held;
      await route.fulfill({ response });
    });
    let polls = 0;
    page.on("request", (req) => { if (req.url().endsWith("/api/gpu-users")) polls++; });
    if (source === "official") {
      await page.selectOption("#version-select", "580.178.04");
      await expect(page.locator(".actions a[href^='https://']")).toHaveAttribute("target", "_blank");
      await expect(page.locator(".actions a[href^='https://']")).toHaveAttribute("rel", "noopener noreferrer");
      await expect(page.locator(".upload-help")).toHaveCount(0);
      await page.getByRole("button", { name: CHECK }).click();
    } else {
      await page.getByRole("button", { name: /Check this version and continue|用这个版本检查并继续/ }).first().click();
    }
    const status = page.locator(".update-check-status");
    try {
      await expect(status).toBeVisible({ timeout: 3000 });
      const rows = status.locator("li");
      await expect(rows).toHaveCount(2);
      await expect(rows.nth(0)).toContainText(/Using the GPU now.*gpu-worker|正在使用显卡.*gpu-worker/);
      await expect(rows.nth(1)).toContainText(/Requested only.*media-idle|仅申请.*media-idle/);
      await expect.poll(() => polls).toBeGreaterThanOrEqual(2);
      if (source === "official") {
        await page.screenshot({ path: "../.implementation/gpu-users-ux/wait-desktop.png", fullPage: true });
        await page.setViewportSize({ width: 390, height: 844 });
        await expect(status).toBeVisible();
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
        await page.screenshot({ path: "../.implementation/gpu-users-ux/wait-mobile.png", fullPage: true });
      }
    } finally { release(); }
    await expect(status).toHaveCount(0);
    const ended = polls;
    await page.waitForTimeout(2200);
    expect(polls).toBe(ended);
  });
}

test("cancelled waiting cannot affect an immediate retry, and Check again reuses its source", async ({ page, request }) => {
  await openUpdate(page, request, "gpu_wait");
  await page.selectOption("#version-select", "580.178.04");
  const sources: unknown[] = [];
  const release: (() => void)[] = [];
  await page.route("**/api/plans", async (route) => {
    const index = sources.push(route.request().postDataJSON().source) - 1;
    const response = await route.fetch();
    if (index < 2) await new Promise<void>((resolve) => { release[index] = resolve; });
    try { await route.fulfill({ response }); } catch { /* The first browser request was cancelled. */ }
  });
  const check = page.getByRole("button", { name: CHECK });
  try {
    await check.click();
    await expect(page.getByRole("button", { name: /Cancel waiting|取消等待/ })).toBeVisible();
    await page.getByRole("button", { name: /Cancel waiting|取消等待/ }).click();
    await expect(page.locator(".update-check-status")).toHaveCount(0);
    await expect(check).toBeEnabled();
    await expect(check).toBeFocused();
    await check.click();
    await expect(page.locator(".update-check-status")).toBeVisible();
    release[0]?.();
    await page.waitForTimeout(100);
    await expect(page.locator(".update-check-status")).toBeVisible();
    // A changed selection must not change the same-update recheck request.
    await page.selectOption("#version-select", "615.71.09");
    release[1]?.();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toBeVisible();
    await expect(dialog.locator(".dialog-title")).toBeFocused();
    await expect(dialog.locator(".gpu-snapshot-time")).toContainText(/List taken at|名单取自/);
    await expect(dialog.locator(".gpu-user-list li")).toHaveCount(2);
    await dialog.getByRole("button", { name: /Check again|重新检查/ }).click();
    await expect.poll(() => sources.length).toBe(3);
    expect(sources[0]).toEqual(sources[1]);
    expect(sources[1]).toEqual(sources[2]);
  } finally { for (const done of release) done?.(); }
});

test("a cancelled plan-change refresh cannot clear the next check's GPU selection", async ({ page, request }) => {
  await openUpdate(page, request, "mixed_1b80_2b85");
  const boxes = page.locator("fieldset.gpu-select input");
  await boxes.nth(0).check(); await page.selectOption("#version-select", "580.178.04");
  let versionsWaiting = false; let releaseVersions = (): void => {}; let releasePlan = (): void => {};
  await page.route("**/api/versions", async (route) => {
    const response = await route.fetch(); versionsWaiting = true;
    await new Promise<void>((resolve) => { releaseVersions = resolve; });
    await route.fulfill({ response });
  });
  let posts = 0;
  await page.route("**/api/plans", async (route) => {
    if (++posts === 1) {
      await route.fulfill({status:409, contentType:"application/json", body:JSON.stringify({error:"plan.changed",params:{items:["gpus"]}})});
    } else {
      const response = await route.fetch();
      await new Promise<void>((resolve) => { releasePlan = resolve; });
      await route.fulfill({response});
    }
  });
  try {
    await page.getByRole("button", {name:CHECK}).click();
    await expect.poll(() => versionsWaiting).toBe(true);
    await page.getByRole("button", {name:/Cancel waiting|取消等待/}).click();
    await boxes.nth(0).uncheck(); await boxes.nth(1).check();
    await page.selectOption("#version-select", "615.71.09");
    await page.getByRole("button", {name:CHECK}).click();
    await expect.poll(() => posts).toBe(2);
    releaseVersions(); await page.waitForTimeout(150);
    await expect(boxes.nth(1)).toBeChecked();
    await expect(page.locator("#version-select")).toHaveValue("615.71.09");
  } finally { releaseVersions(); releasePlan(); }
});

test("GPU waiting rows stay readable across four themes, both languages and mobile widths", async ({ page, request }) => {
  test.setTimeout(60_000);
  await openUpdate(page, request, "gpu_wait");
  await page.route("**/api/gpu-users", (route) => route.fulfill({json:{
    state:"busy", checked_at:1792209600000,
    users:[
      {id:"docker:active",name:"gpu-worker",type:"docker",usage:"in_use"},
      {id:"container:1",name:"compute-instance",type:"incus",usage:"unknown"},
      {id:"docker:idle",name:"media-idle",type:"docker",usage:"requested"},
    ],errors:[],
  }}));
  const pending: (() => void)[] = [];
  await page.route("**/api/plans", async (route) => {
    const response = await route.fetch();
    await new Promise<void>((resolve) => { pending.push(resolve); });
    try { await route.fulfill({response}); } catch { /* Cancelled by the language redraw. */ }
  });
  try {
    for (const lang of ["en","zh"]) {
      await page.locator("#lang-control").selectOption(lang);
      await page.selectOption("#version-select", "580.178.04");
      await page.getByRole("button", {name:CHECK}).click();
      const status = page.locator(".update-check-status");
      await expect(status.locator(".gpu-user")).toHaveCount(3);
      expect(await status.locator("[role=status]").count()).toBe(0);
      await expect(status.locator(".gpu-howto")).toHaveCount(2);
      for (const theme of ["focused-light","focused-dark","plate-light","plate-dark"]) {
        await page.locator("#theme-control").selectOption(theme);
        for (const width of [1280,390]) {
          await page.setViewportSize({width,height:900});
          await status.scrollIntoViewIfNeeded();
          expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
          const line = status.locator(".gpu-user-line").first();
          await expect(line).toContainText(lang === "zh" ? "正在使用显卡" : "Using the GPU now");
          await page.screenshot({path:`../.implementation/gpu-users-ux/wait-${lang}-${theme}-${width}.png`,fullPage:true});
        }
      }
      await page.getByRole("button", {name:/Cancel waiting|取消等待/}).click();
      pending.at(-1)?.();
    }
  } finally { for (const done of pending) done(); }
});
