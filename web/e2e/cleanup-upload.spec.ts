import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
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
  await page.getByRole("button", { name: /Upload \.run|上传 \.run/ }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  return dialog;
}

const UPLOAD = /^Upload$|^上传$/;
const RESTART = /Start the upload over|重新开始上传/;
const CLOSE = /^Close$|^关闭$/;
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
  await dialog
    .locator("#upload-files")
    .setInputFiles([runFile("580.178.04"), runFile("580.178.05"), runFile("580.178.06")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();

  await expect(dialog.locator(".upload-summary")).toContainText(
    "NVIDIA-Linux-x86_64-580.178.06.run",
    { timeout: 10_000 },
  );
  expect(puts.length).toBe(3);
  expect(reads.length).toBe(1);
});

test("every control stays disabled until the final read settles", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  // The dialog's own read already happened; the next one is the batch's final read, which is held open.
  let reads = 0;
  await page.route("**/api/uploads/current", async (route) => {
    reads += 1;
    await new Promise((resolve) => setTimeout(resolve, 1500));
    await route.continue();
  });
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
  await expect(dialog.locator("#upload-files")).toBeDisabled();
  await expect(dialog.getByRole("button", { name: UPLOAD })).toBeDisabled();
  await expect(dialog.getByRole("button", { name: RESTART })).toBeDisabled();
  await expect(dialog.getByRole("button", { name: CLOSE })).toBeDisabled();

  await expect(dialog.locator("#upload-files")).toBeEnabled({ timeout: 10_000 });
  expect(reads).toBe(1);
});

test("a failed final read drops the stale summary and keeps Check disabled", async ({ page, request }) => {
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
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i, {
    timeout: 10_000,
  });
  await expect(dialog.getByRole("button", { name: CHECK })).toBeDisabled();

  await expect(dialog.locator(".upload-summary")).not.toContainText(
    "NVIDIA-Linux-x86_64-580.178.04.run",
  );
  await expect(dialog.locator(".upload-summary .summary-line")).toHaveCount(0);
});

test("a late initial read cannot re-adopt the group after the final read failed", async ({
  page,
  request,
}) => {
  await openUpdate(page, request);
  let reads = 0;
  await page.route("**/api/uploads/current", async (route) => {
    reads += 1;
    if (reads === 1) {
      // The dialog's own read: hold it until after the batch.
      await new Promise((resolve) => setTimeout(resolve, 2500));
      await route.continue();
      return;
    }
    await route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({ error: "internal", params: {} }),
    });
  });
  const dialog = await openUploadDialog(page);
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i, {
    timeout: 10_000,
  });
  // Let the held first read land: it must not re-enable the stale group.
  await page.waitForTimeout(3000);
  await expect(dialog.locator(".upload-summary .summary-line")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: CHECK })).toBeDisabled();
});

test("restart reads upload-group once and drops the receipts of the replaced group", async ({ page, request }) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
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
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
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
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i, {
    timeout: 10_000,
  });

  await expect(dialog.locator(".upload-summary")).not.toContainText(
    "NVIDIA-Linux-x86_64-580.178.04.run",
  );
});

test("a normal upload that opens a new group never shows the old group's receipt", async ({
  page,
  request,
}) => {
  await openUpdate(page, request);
  const dialog = await openUploadDialog(page);

  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.04")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
  await expect(dialog.locator(".upload-summary")).toContainText("NVIDIA-Linux-x86_64-580.178.04.run", {
    timeout: 10_000,
  });

  // 2. A second batch stays on group A, but its final read fails: the display    is dropped while the
  // same-group receipt is legitimately kept.
  let reads = 0;
  await page.route("**/api/uploads/current", async (route) => {
    reads += 1;
    await route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({ error: "internal", params: {} }),
    });
  });
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.05")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
  await expect(dialog.locator(".dialog-error")).toContainText(/internal error|内部错误/i, {
    timeout: 10_000,
  });
  await expect(dialog.locator(".upload-summary .summary-line")).toHaveCount(0);

  // 3. With no displayed group the next normal upload opens a NEW group (open-upload);    its
  // file-upload and the final read both fail. Nothing of group A may reappear.
  await page.route("**/api/uploads/*/files*", async (route) => {
    await route.fulfill({
      status: 500,
      contentType: "application/json",
      body: JSON.stringify({ error: "internal", params: {} }),
    });
  });
  await dialog.locator("#upload-files").setInputFiles([runFile("580.178.06")]);
  await dialog.getByRole("button", { name: UPLOAD }).click();
  await expect(dialog.locator(".dialog-error")).toBeVisible({ timeout: 10_000 });
  await expect(dialog.locator(".upload-summary .summary-line")).toHaveCount(0);
  await expect(dialog.locator(".upload-summary")).not.toContainText("was received");
  await expect(dialog.getByRole("button", { name: CHECK })).toBeDisabled();

  expect(reads).toBe(2);
});
