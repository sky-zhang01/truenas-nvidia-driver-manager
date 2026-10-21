import { test } from "node:test";
import assert from "node:assert/strict";
import { FakeElement, FakeText } from "./dom-shim.js";
import { renderSetup } from "../src/views/setup.js";

test("setup names all inputs, explains App logs and keeps the administrator read-only", () => {
  (globalThis as unknown as { document: unknown }).document = {
    createElement: (tag: string) => new FakeElement(tag),
    createTextNode: (value: string) => new FakeText(value),
  };
  for (const lang of ["en", "zh"] as const) {
    const container = new FakeElement("main");
    renderSetup({ lang, login: { setup: { username: "operator", valid_hours: 24 } } } as never, container as never);
    const walk = (node: FakeElement): FakeElement[] => [node, ...node.children.flatMap(c => c instanceof FakeElement ? walk(c) : [])];
    const all = walk(container);
    const inputs = all.filter(n => n.tagName === "INPUT");
    assert.equal(inputs.length, 4);
    for (const input of inputs) assert.ok(all.some(n => n.tagName === "LABEL" && n.getAttribute("for") === input.id));
    assert.equal(inputs.find(n => n.id === "setup-user")?.getAttribute("readonly"), "");
    assert.equal(inputs.find(n => n.id === "setup-code")?.getAttribute("autocomplete"), "off");
    assert.match(container.textContent, /SETUP CODE/);
    assert.match(container.textContent, lang === "en" ? /View Logs/ : /查看日志/);
    assert.equal(all.find(n => n.tagName === "OL")?.children.length, 3);
    assert.equal(all.find(n => n.tagName === "DETAILS")?.getAttribute("open"), null);
    assert.equal(all.filter(n => n.getAttribute("role") === "alert").length, 2);
  }
});

class InputElement extends FakeElement {
  value = "";
  disabled = false;
  focused = false;
  private listeners = new Map<string, EventListener>();
  override addEventListener(): void;
  override addEventListener(type: string, listener: EventListener): void;
  override addEventListener(type?: string, listener?: EventListener): void {
    if (type && listener) this.listeners.set(type, listener);
  }
  override focus(): void { this.focused = true; }
  submit(): void { this.listeners.get("submit")?.(new Event("submit")); }
}

function setupForm() {
  (globalThis as unknown as { document: unknown }).document = {
    createElement: (tag: string) => new InputElement(tag),
    createTextNode: (value: string) => new FakeText(value),
  };
  const container = new InputElement("main");
  const routes: string[] = [];
  const app = {
    lang: "en", login: { setup: { username: "admin" } },
    refreshLogin: async () => ({}), clearLogoutDone: () => {}, clearSessionExpired: () => {},
    clearLogoutError: () => {}, go: (route: string) => routes.push(route),
  };
  renderSetup(app as never, container as never);
  const walk = (node: FakeElement): InputElement[] => [node as InputElement, ...node.children.flatMap(c => c instanceof FakeElement ? walk(c) : [])];
  const all = walk(container);
  const field = (id: string) => all.find(n => n.id === id)!;
  field("setup-code").value = "ABCDEFGHJKMNPQRS";
  field("setup-password").value = "my strong password";
  field("setup-repeat").value = "my strong password";
  const form = all.find(n => n.tagName === "FORM")!;
  return { container, field, form, routes, submit: all.find(n => n.tagName === "BUTTON")! };
}

const settled = () => new Promise(resolve => setTimeout(resolve, 0));

test("a wrong or renewed code clears and focuses only the code, retaining both passwords", async () => {
  for (const renewed of [false, true]) {
    const { field, form, submit } = setupForm();
    globalThis.fetch = async () => new Response(JSON.stringify({ error: "setup.code_invalid", params: { code_renewed: renewed, show_help: true } }), { status: 403 });
    form.submit();
    assert.equal(field("setup-code").value, "");
    assert.equal(submit.disabled, true);
    await settled();
    assert.equal(field("setup-code").focused, true);
    assert.equal(field("setup-code").getAttribute("aria-invalid"), "true");
    assert.equal(field("setup-password").value, "my strong password");
    assert.equal(field("setup-repeat").value, "my strong password");
    assert.match(field("setup-code-error").textContent, renewed ? /expired/ : /is incorrect/);
    assert.match(field("setup-code-error").textContent, /old code no longer works/);
    assert.equal(submit.disabled, false);
  }
});

test("mismatched passwords focus the repeat field before any setup request", () => {
  const { field, form } = setupForm();
  let requests = 0;
  globalThis.fetch = async () => { requests += 1; return new Response(); };
  field("setup-repeat").value = "different";
  form.submit();
  assert.equal(requests, 0);
  assert.equal(field("setup-repeat").focused, true);
  assert.match(field("setup-error").textContent, /do not match/);
});

test("setup prevents repeated requests, remembers the session token and opens Home", async () => {
  const { form, field, routes } = setupForm();
  let requests = 0;
  const values = new Map<string, string>();
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: { getItem: (k: string) => values.get(k) ?? null, setItem: (k: string, v: string) => values.set(k, v) } });
  globalThis.fetch = async (_path, init) => {
    requests += 1;
    assert.deepEqual(JSON.parse(init?.body as string), { code: "ABCDEFGHJKMNPQRS", new_password: "my strong password" });
    return new Response(JSON.stringify({ token: "setup-session-token" }));
  };
  form.submit(); form.submit();
  await settled();
  assert.equal(requests, 1);
  assert.equal(values.get("nvm.token"), "setup-session-token");
  assert.deepEqual(routes, ["#/home"]);
  assert.equal(field("setup-password").value, "");
  assert.equal(field("setup-repeat").value, "");
});

test("rate limiting displays retry seconds while leaving passwords available", async () => {
  const { field, form } = setupForm();
  globalThis.fetch = async () => new Response(JSON.stringify({ error: "login.rate_limited", params: { retry_after_s: 30 } }), { status: 429 });
  form.submit(); await settled();
  assert.match(field("setup-error").textContent, /30/);
  assert.equal(field("setup-password").value, "my strong password");
});

test("setup maps the password.rule response through a fixed rule list and focuses the password", async () => {
  const cases = [
    ["password.rule.too_short", /at least 8 characters/],
    ["password.rule.too_long", /1024 bytes/],
    ["password.rule.whitespace", /only of whitespace/],
    ["password.rule.placeholder", /start with/],
    ["setup.submit", /request is not valid/],
    [null, /request is not valid/],
  ] as const;
  for (const [rule, expected] of cases) {
    const { field, form } = setupForm();
    globalThis.fetch = async () => new Response(JSON.stringify({ error: "password.rule", params: { rule } }), { status: 400 });
    form.submit(); await settled();
    assert.match(field("setup-error").textContent, expected);
    assert.equal(field("setup-error").textContent.includes("UNKNOWN"), false);
    assert.equal(field("setup-password").focused, true);
    assert.equal(field("setup-password").value, "my strong password");
    assert.equal(field("setup-repeat").value, "my strong password");
    assert.equal(field("setup-code").value, "");
  }
});
