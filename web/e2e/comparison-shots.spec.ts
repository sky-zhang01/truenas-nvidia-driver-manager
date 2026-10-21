import { test } from "./local-test.js";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";

// Explicit screenshot job; normal regression runs do not regenerate review artifacts.
test.describe("approved comparison scenes", () => {
  test.afterEach(async () => { const { cleanup } = await import("./feedback-scenes/lib.mjs"); await cleanup(); });
  test.skip(process.env["NVM_COPY_SHOTS"] !== "1", "Set NVM_COPY_SHOTS=1 for the comparison capture job");
  test("update-entry", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/update-entry.mjs");
  });
  test("gpu-live-states", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/gpu-live-states.mjs");
  });
  test("drivers-and-confirmation", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/drivers-and-confirmation.mjs");
  });
  test("gpu-narrow", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/gpu-narrow.mjs");
  });
  test("task-states", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/task-states.mjs");
  });
  test("upload-empty", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/upload-empty.mjs");
  });
  test("upload-errors", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/upload-errors.mjs");
  });
  test("upload-transfer", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/upload-transfer.mjs");
  });
  test("upload-versions", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/upload-versions.mjs");
  });
  test("upload-plans", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/upload-plans.mjs");
  });
  test("upload-read-failure", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/upload-read-failure.mjs");
  });
  test("upload-wait-and-unreadable", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/upload-wait-and-unreadable.mjs");
  });
  test("confirmation-and-upload-retry", async () => {
    test.setTimeout(180_000);
    mkdirSync(resolve(process.cwd(), process.env["NVM_COPY_SCREENSHOT_DIR"] ?? "../.implementation/release-fixes/shots"), { recursive: true });
    await import("./feedback-scenes/confirmation-retry.mjs");
  });
});
