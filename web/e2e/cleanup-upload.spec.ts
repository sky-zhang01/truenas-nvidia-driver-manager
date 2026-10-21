import { test, expect, type APIRequestContext, type Page } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

async function openUpdate(page: Page, request: APIRequestContext): Promise<void> {
  await setScenario(request, "fresh");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/update");
  await expect(page.locator("#version-select")).toBeVisible();
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

async function openUploadDialog(page: Page) {
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  return dialog;
}

const RESTART = /Clear and start again|清空重来/;
const CHECK = /^Check and continue$|^检查并继续$/;

test("a multi-file batch reads upload-group exactly once, after the batch", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  const reads: string[] = [];
  const puts: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "GET" && req.url().endsWith("/api/uploads/current")) reads.push(req.url());
    if (req.method() === "PUT" && req.url().includes("/api/uploads/")) puts.push(req.url());
  });
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04"), runFile("580.178.05"), runFile("580.178.06")]);


  await expect(dialog.locator(".upload-summary")).toContainText(
    "NVIDIA-Linux-x86_64-580.178.06.run",
    { timeout: 10_000 },
  );
  expect(puts.length).toBe(3);
  expect(reads.length).toBe(1);
});

test("file controls stay disabled until the final read settles while cancellation stays available", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  // The dialog's own read already happened; the next one is the batch's final read, which is held open.
  let reads = 0;
  await page.route("**/api/uploads/current", async (route) => {
    reads += 1;
    await new Promise((resolve) => setTimeout(resolve, 1500));
    await route.continue();
  });
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);

  await expect(dialog.locator("#upload-files")).toBeDisabled();
  await expect(dialog.getByRole("button", { name: RESTART })).toBeDisabled();
  await expect(dialog.locator(".upload-footer").getByRole("button", { name: /Cancel upload|取消上传/ })).toBeEnabled();

  await expect(dialog.locator("#upload-files")).toBeEnabled({ timeout: 10_000 });
  expect(reads).toBe(1);
});

test("a failed final read keeps accepted files visible and disables Check until a retry", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  // The dialog's first read succeeds; the batch's final read fails.
  await page.route("**/api/uploads/current", async (route) => {
    await route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({ error: "internal", params: {} }),
    });
  });
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);

  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i, {
    timeout: 10_000,
  });
  await expect(dialog.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "true");

  await expect(dialog.locator(".upload-summary")).toContainText("NVIDIA-Linux-x86_64-580.178.04.run");
  await expect(dialog.locator(".upload-summary .upload-receipt")).toHaveCount(1);
  await expect(dialog.locator("#upload-files")).toBeDisabled();
});

test("a held initial read disables selection and a failed batch read preserves accepted files", async ({ page, request }) => {
  await openUpdate(page, request);
  let reads = 0;
  let releaseInitial = (): void => {};
  const initial = new Promise<void>((resolve) => { releaseInitial = resolve; });
  await page.route("**/api/uploads/current", async (route) => {
    reads += 1;
    if (reads === 1) { await initial; await route.continue(); return; }
    await route.fulfill({ status: 503, json: { error: "internal", params: {} } });
  });
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog.upload-dialog[open]");
  await expect(dialog.locator("#upload-files")).toBeDisabled();
  releaseInitial();
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i);
  await expect(dialog.locator(".upload-receipt")).toHaveCount(1);
  await expect(dialog.locator("#upload-files")).toBeDisabled();
  await expect(dialog.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "true");
});

test("restart reads upload-group once and drops the receipts of the replaced group", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);

  await expect(dialog.locator(".upload-summary")).toContainText("NVIDIA-Linux-x86_64-580.178.04.run", {
    timeout: 10_000,
  });
  const reads: string[] = [];
  page.on("request", (req) => {
    if (req.method() === "GET" && req.url().endsWith("/api/uploads/current")) reads.push(req.url());
  });
  await dialog.getByRole("button", { name: RESTART }).click();
  await expect(dialog.locator(".upload-summary")).not.toContainText(
    "NVIDIA-Linux-x86_64-580.178.04.run",
    { timeout: 10_000 },
  );
  expect(reads.length).toBe(1);
});

test("a failed restart keeps the receipts of the still-current group", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);

  await expect(dialog.locator(".upload-summary")).toContainText("NVIDIA-Linux-x86_64-580.178.04.run", {
    timeout: 10_000,
  });
  await page.route("**/api/uploads", async (route) => {
    if (route.request().method() === "POST") {
      await route.fulfill({
        status: 500,
        contentType: "application/json",
        body: JSON.stringify({ error: "internal", params: {} }),
      });
      return;
    }
    await route.continue();
  });
  await dialog.getByRole("button", { name: RESTART }).click();
  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i, {
    timeout: 10_000,
  });
  // open-upload failed, so the group did not change: the same group's receipt survives.
  await expect(dialog.locator(".upload-summary")).toContainText("NVIDIA-Linux-x86_64-580.178.04.run");
});

test("a file-upload answer that names another group is not adopted", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  await page.route("**/api/uploads/*/files*", async (route) => {
    const original = await route.fetch();
    const body = (await original.json()) as { group: { group_id: string } };
    body.group.group_id = "00000000000000000000000000000000";
    await route.fulfill({ response: original, body: JSON.stringify(body) });
  });
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);

  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i, {
    timeout: 10_000,
  });

  await expect(dialog.locator(".upload-summary")).not.toContainText(
    "NVIDIA-Linux-x86_64-580.178.04.run",
  );
});

test("a failed read preserves its group until retry or explicit clearing", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await expect(dialog.locator(".upload-receipt")).toContainText("580.178.04");
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  let failRead = true;
  let opens = 0;
  page.on("request", (req) => { if (req.method() === "POST" && req.url().endsWith("/api/uploads")) opens++; });
  await page.route("**/api/uploads/current", async (route) => {
    if (failRead) await route.fulfill({ status: 503, json: { error: "internal", params: {} } });
    else await route.continue();
  });
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.05")]);
  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i);
  await expect(dialog.locator(".upload-receipt")).toContainText("580.178.05");
  await expect(dialog.locator("#upload-files")).toBeDisabled();
  expect(opens).toBe(0);
  failRead = false;
  await dialog.locator(".upload-read-retry").click();
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await expect(dialog.locator(".upload-receipt")).toContainText("580.178.05");
  expect(opens).toBe(0);
  failRead = true;
  await dialog.getByRole("button", { name: RESTART }).click();
  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i);
  await expect(dialog.locator(".upload-receipt")).toHaveCount(0);
  await expect(dialog.locator("#upload-files")).toBeDisabled();
  await expect(dialog.getByRole("button", { name: CHECK })).toHaveAttribute("aria-disabled", "true");
  expect(opens).toBe(1);
});
