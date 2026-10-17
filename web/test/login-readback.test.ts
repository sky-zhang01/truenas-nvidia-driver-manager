import { test } from "node:test";
import assert from "node:assert/strict";

import type { LoginInfo } from "../src/api.js";
import { FakeElement, FakeText } from "./dom-shim.js";

const allowed: LoginInfo = {
  logged_in: true,
  must_change_password: false,
  update_blocked: [],
  setup: { problems: [] },
  credentials: "ok",
  show_reset_help: false,
};

const blocked: LoginInfo = {
  ...allowed,
  update_blocked: [{ key: "task_running" }],
};

test("a failed login status read closes the write gate until a successful read", async () => {
  // The module's initial login status is left pending so this test can exercise a separate Shell
  // without rendering a page or relying on a fake host server.
  (globalThis as unknown as { window: unknown }).window = {
    location: { hash: "" },
    localStorage: { getItem: () => "en" },
    addEventListener: () => {},
  };
  globalThis.fetch = async () => new Promise<Response>(() => {});
  const { Shell } = await import("../src/main.js");
  const { loginReadFailed } = await import("../src/views/update.js");
  const shell = new Shell();

  globalThis.fetch = async () => new Response(JSON.stringify(allowed), { status: 200 });
  await shell.refreshLogin();
  assert.equal(shell.signedIn, true);
  assert.equal(loginReadFailed(shell), false);

  const failure = new TypeError("Failed to fetch");
  globalThis.fetch = async () => { throw failure; };
  let caught: unknown;
  try {
    await shell.refreshLogin();
  } catch (err) {
    caught = err;
  }
  assert.equal(caught, failure);
  assert.equal(shell.loginError, failure);
  assert.equal(shell.signedIn, true);
  assert.deepEqual(shell.login.update_blocked, []);
  assert.equal(loginReadFailed(shell), true);

  globalThis.fetch = async () => new Response(JSON.stringify(blocked), { status: 200 });
  await shell.refreshLogin();
  assert.equal(loginReadFailed(shell), false);
  assert.deepEqual(shell.login.update_blocked, [{ key: "task_running" }]);

  globalThis.fetch = async () => new Response(JSON.stringify(allowed), { status: 200 });
  await shell.refreshLogin();
  assert.equal(loginReadFailed(shell), false);
  assert.deepEqual(shell.login.update_blocked, []);
});

test("the latest successful key list decides ssh_enabled when the next sign-in read fails", async () => {
  (globalThis as unknown as { document: unknown }).document = {
    querySelectorAll: () => [],
  };
  (globalThis as unknown as { window: unknown }).window = {
    location: { hash: "" },
    localStorage: { getItem: () => "en" },
    addEventListener: () => {},
  };
  globalThis.fetch = async () => new Promise<Response>(() => {});
  const { Shell } = await import("../src/main.js");
  const shell = new Shell();
  globalThis.fetch = async () =>
    new Response(JSON.stringify({ ...allowed, ssh_enabled: true }), { status: 200 });
  await shell.refreshLogin();
  assert.equal(shell.signedIn, true);
  assert.equal(shell.login.ssh_enabled, true);

  shell.syncSshEnabled(false);
  assert.equal(shell.signedIn, true);
  shell.signOutLocally();
  assert.equal(shell.signedIn, false);
  const failure = new TypeError("Failed to fetch");
  globalThis.fetch = async () => {
    throw failure;
  };
  let caught: unknown;
  try {
    await shell.refreshLogin();
  } catch (err) {
    caught = err;
  }
  assert.equal(caught, failure);
  assert.equal(shell.signedIn, false);
  assert.equal(shell.login.ssh_enabled, false);
  assert.equal(shell.loginError, failure);

  shell.signedIn = true;
  shell.syncSshEnabled(true);
  shell.signOutLocally();
  caught = undefined;
  try {
    await shell.refreshLogin();
  } catch (err) {
    caught = err;
  }
  assert.equal(caught, failure);
  assert.equal(shell.signedIn, false);
  assert.equal(shell.login.ssh_enabled, true);
});

test("a failed login status read disables an original driver's keep switch and blocks keep-switch", async () => {
  class InteractiveElement extends FakeElement {
    checked = false;
    disabled = false;
    private listeners = new Map<string, EventListener[]>();

    override addEventListener(): void;
    override addEventListener(type: string, listener: EventListener): void;
    override addEventListener(type?: string, listener?: EventListener): void {
      if (!type || !listener) return;
      this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
    }

    emit(type: string): void {
      for (const listener of this.listeners.get(type) ?? []) listener(new Event(type));
    }

    prepend(...nodes: (FakeElement | FakeText | string)[]): void {
      this.append(...nodes);
    }
  }

  (globalThis as unknown as { document: unknown }).document = {
    createElement: (tag: string) => new InteractiveElement(tag),
    createTextNode: (value: string) => new FakeText(value),
    createDocumentFragment: () => new InteractiveElement("#fragment"),
  };
  (globalThis as unknown as { window: unknown }).window = {
    location: { hash: "" },
    localStorage: { getItem: () => "en" },
    addEventListener: () => {},
  };
  globalThis.fetch = async () => new Promise<Response>(() => {});
  const { Shell } = await import("../src/main.js");
  const { renderDrivers } = await import("../src/views/drivers.js");
  const shell = new Shell();
  globalThis.fetch = async () => new Response(JSON.stringify(allowed), { status: 200 });
  await shell.refreshLogin();

  let row = {
    id: "original-row",
    kind: "original",
    version: "580.178.04",
    fingerprint: null,
    created_at: null,
    size: null,
    target_kernel: "6.12.96",
    status: "available",
    keep: false,
    can_update: true,
    delete_blocked: null,
  };
  let keepCalls = 0;
  let loginFails = true;
  globalThis.fetch = async (input) => {
    const path = String(input);
    if (path === "/api/drivers") {
      return new Response(JSON.stringify({ rows: [row], kernel: "6.12.96", delete_unavailable: null }));
    }
    if (path === "/api/login") {
      if (loginFails) throw new TypeError("Failed to fetch");
      return new Response(JSON.stringify(allowed));
    }
    if (path === "/api/drivers/original-row/keep") {
      keepCalls += 1;
      row = { ...row, keep: true };
      return new Response(JSON.stringify(row));
    }
    throw new Error(`unexpected request: ${path}`);
  };
  const container = new InteractiveElement("div");
  renderDrivers(shell, container as unknown as HTMLElement);
  await new Promise((resolve) => setTimeout(resolve, 0));

  const walk = (node: FakeElement): FakeElement[] => [
    node,
    ...node.children.flatMap((child) => child instanceof FakeElement ? walk(child) : []),
  ];
  const found = walk(container);
  const sw = found.find((node) => node.getAttribute("role") === "switch") as InteractiveElement | undefined;
  assert.ok(sw, "the original driver has a keep switch");
  assert.equal(found.some((node) => node.id === "login-read-error"), true);
  assert.equal(sw.disabled, true);
  assert.equal(sw.getAttribute("aria-describedby"), "login-read-error");
  sw.checked = true;
  sw.emit("change");
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(keepCalls, 0);
  assert.equal(sw.checked, false);

  loginFails = false;
  const recovered = new InteractiveElement("div");
  renderDrivers(shell, recovered as unknown as HTMLElement);
  await new Promise((resolve) => setTimeout(resolve, 0));
  const recoveredSwitch = walk(recovered).find((node) => node.getAttribute("role") === "switch") as InteractiveElement | undefined;
  assert.ok(recoveredSwitch);
  assert.equal(recoveredSwitch.disabled, false);
  assert.equal(recoveredSwitch.getAttribute("aria-describedby"), null);
  recoveredSwitch.checked = true;
  recoveredSwitch.emit("change");
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(keepCalls, 1);
});
