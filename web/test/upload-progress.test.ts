import { test } from "node:test";
import assert from "node:assert/strict";
import { putUploadFile } from "../src/api.js";

test("upload completion announces reading even when the browser emits no intermediate progress", async () => {
  const previous = globalThis.XMLHttpRequest;
  class Request {
    status = 200;
    responseText = JSON.stringify({ file: {}, group: {} });
    uploadHandlers = new Map<string, () => void>();
    handlers = new Map<string, () => void>();
    upload = { addEventListener: (name: string, fn: () => void) => { this.uploadHandlers.set(name, fn); } };
    open(): void {}
    setRequestHeader(): void {}
    addEventListener(name: string, fn: () => void): void { this.handlers.set(name, fn); }
    send(): void { this.uploadHandlers.get("load")?.(); this.handlers.get("load")?.(); }
  }
  globalThis.XMLHttpRequest = Request as unknown as typeof XMLHttpRequest;
  try {
    const progress: number[] = [];
    const file = new File(["driver"], "driver.run");
    await putUploadFile("group", file.name, file, (sent) => progress.push(sent));
    assert.deepEqual(progress, [file.size]);
  } finally { globalThis.XMLHttpRequest = previous; }
});
