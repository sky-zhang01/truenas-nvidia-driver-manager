import { test } from "node:test";
import assert from "node:assert/strict";
import { installDom, FakeElement, FakeText } from "./dom-shim.js";
import { appendInline, expandText } from "../src/dom.js";
import { armUpdateCheckStatus, gpuBlockNode } from "../src/views/plan.js";
import { gpuUsersNode } from "../src/gpu-users.js";
installDom();
const walk = (node: FakeElement | FakeText): FakeElement[] => node instanceof FakeText ? [] : [node, ...node.children.flatMap(walk)];

test("unreadable plan snapshots keep the conservative decision without repeating the title", () => {
  for (const lang of ["zh", "en"] as const) {
    const snapshot = { state: "unreadable", users: [], errors: [], checked_at: 1792209600000, read_reason: { key: "reason.permission_denied" } } as const;
    const block = gpuBlockNode({ lang } as never, snapshot as never) as unknown as FakeElement;
    const headline = walk(block).find(n => n.className.split(/\s+/).includes("gpu-list-headline"));
    assert.equal(headline?.textContent, lang === "zh" ? "按被使用处理。" : "Treated as in use.");
    for (const options of [{}, { snapshot: true, headline: "" }]) {
      const standalone = gpuUsersNode(snapshot as never, lang, options) as unknown as FakeElement;
      assert.ok(standalone.textContent.includes(expandText("gpu.unreadable", {}, lang)));
    }
  }
});

test("snapshot times describe a displayed list, including an unreadable state with known users", () => {
  for (const state of ["busy", "unreadable"] as const) {
    for (const users of [[], [{ name: "worker", type: "docker" }]]) {
      const block = gpuBlockNode({ lang: "en" } as never, { state, users, errors: [], checked_at: 1792209600000 } as never) as unknown as FakeElement;
      const times = walk(block).filter(n => n.className.split(/\s+/).includes("gpu-snapshot-time"));
      assert.equal(times.length, users.length > 0 ? 1 : 0);
    }
  }
});

test("HTTPS links open a safe new tab and internal links keep their navigation", () => {
  const target = new FakeElement("div");
  appendInline(target as never, [
    { kind: "link", text: "NVIDIA", href: "https://www.nvidia.com/" },
    { kind: "link", text: "Drivers", href: "#/drivers" },
  ]);
  const [external, internal] = target.children as FakeElement[];
  assert.equal(external?.getAttribute("target"), "_blank");
  assert.equal(external?.getAttribute("rel"), "noopener noreferrer");
  assert.equal(internal?.getAttribute("target"), null);
  assert.equal(internal?.getAttribute("rel"), null);
});

test("usage labels render in both languages while older entries still render", () => {
  for (const lang of ["zh", "en"] as const) {
    const node = gpuBlockNode({ lang } as never, { state: "busy", users: [
      { name: "using", type: "docker", usage: "in_use" },
      { name: "idle", type: "docker", usage: "requested" },
      { name: "maybe", type: "incus", usage: "unknown" },
      { name: "older", type: "docker" },
    ], errors: [] } as never);
    for (const key of ["gpu.badge.in_use", "gpu.badge.requested", "gpu.badge.unknown"]) {
      assert.ok(node.textContent?.includes(expandText(key, {}, lang)));
      assert.notEqual(expandText(key, {}, lang), "UNKNOWN");
    }
    assert.ok(node.textContent?.includes("older"));
  }
});

test("plan waiting polls at one second then every two seconds, deduplicates and stops", async () => {
  const timers = new Map<number, () => void>();
  const intervals = new Map<number, () => void>();
  let next = 0;
  const oldWindow = globalThis.window;
  const oldFetch = globalThis.fetch;
  globalThis.window = {
    setTimeout: (fn: () => void, ms: number): number => { assert.equal(ms, 1000); timers.set(++next, fn); return next; },
    clearTimeout: (id: number): void => { timers.delete(id); },
    setInterval: (fn: () => void, ms: number): number => { assert.equal(ms, 2000); intervals.set(++next, fn); return next; },
    clearInterval: (id: number): void => { intervals.delete(id); },
  } as never;
  let calls = 0;
  let release: (() => void) | undefined;
  globalThis.fetch = (async (url: string) => {
    assert.equal(url, "/api/gpu-users"); calls++;
    if (calls === 3) await new Promise<void>((resolve) => { release = resolve; });
    return { ok: true, json: async () => ({state: "busy", users: [{name: "worker", type: "docker", usage: "in_use"}], errors: []}) };
  }) as never;
  try {
    const parent = new FakeElement("div"); const button = new FakeElement("button"); parent.append(button);
    const stop = armUpdateCheckStatus(button as never, "en");
    timers.values().next().value?.();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(calls, 1);
    const status = walk(parent).find((n) => n.getAttribute("role") === "status"); assert.ok(status);
    assert.match(status.textContent, /Using the GPU now.*worker.*Docker app/);
    const children = [...status.children];
    intervals.values().next().value?.(); await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(status.children, children);
    intervals.values().next().value?.();
    stop(); release?.(); await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(intervals.size, 0); assert.equal(parent.children.length, 1);
  } finally { globalThis.window = oldWindow; globalThis.fetch = oldFetch; }
});

test("waiting snapshot failures keep raw command output out of the live list", async () => {
  const { watchGpuUsers } = await import("../src/gpu-users.js");
  const oldWindow = globalThis.window; const oldFetch = globalThis.fetch;
  let fire: (() => void) | undefined;
  globalThis.window = { setInterval: (fn: () => void): number => { fire = fn; return 1; }, clearInterval: (): void => {} } as never;
  const target = new FakeElement("div");
  let failed = false;
  globalThis.fetch = (async () => failed
    ? { ok: false, status: 500, text: async () => JSON.stringify({error: "internal", params: {reason: {key: "internal.request_failed"}}, raw: "RAW_SECRET_OUTPUT"}) }
    : { ok: true, json: async () => ({ state: "unreadable", users: [], errors: [{type: "docker", reason: {key: "reason.permission_denied"}, raw: "RAW_SECRET_OUTPUT"}], read_reason: {key: "reason.file_missing"} }) }) as never;
  const stop = watchGpuUsers(target as never, "en");
  try {
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.match(target.textContent, /GPU usage state/);
    assert.equal(target.textContent.includes("UNKNOWN"), false);
    assert.equal(target.textContent.includes("RAW_SECRET_OUTPUT"), false);
    failed = true; fire?.(); await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(target.textContent.includes("RAW_SECRET_OUTPUT"), false);
    assert.equal(walk(target).some((node) => node.getAttribute("role") !== null), false);
  } finally { stop(); globalThis.window = oldWindow; globalThis.fetch = oldFetch; }
});


test("timestamp-only refreshes do not rewrite the live list and rows have shape badges", async () => {
  const { watchGpuUsers } = await import("../src/gpu-users.js");
  const oldWindow = globalThis.window; const oldFetch = globalThis.fetch;
  let fire: (() => void) | undefined; let checked = 1;
  globalThis.window = { setInterval: (fn: () => void): number => { fire = fn; return 1; }, clearInterval: (): void => {} } as never;
  globalThis.fetch = (async () => ({ ok: true, json: async () => ({ state: "busy", checked_at: checked++, users: [{id: "docker:a", name: "worker", type: "docker", usage: "in_use"}], errors: [] }) })) as never;
  const target = new FakeElement("div"); const stop = watchGpuUsers(target as never, "en");
  try {
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.match(target.textContent, /●.*Using the GPU now.*worker/);
    assert.ok(target.textContent.includes("Once you stop the apps marked \"Using the GPU now\", the update continues automatically."));
    const children = [...target.children]; fire?.(); await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(target.children, children);
  } finally { stop(); globalThis.window = oldWindow; globalThis.fetch = oldFetch; }
});

test("removed stable IDs show stopped for four seconds and reduced motion removes immediately", async () => {
  const { watchGpuUsers } = await import("../src/gpu-users.js");
  const oldWindow = globalThis.window; const oldFetch = globalThis.fetch;
  try {
    for (const reduced of [false, true]) {
      let fire: (() => void) | undefined;
      const timers = new Map<number, () => void>(); let next = 0; let present = true;
      globalThis.window = {
        setInterval: (fn: () => void): number => { fire = fn; return 1; }, clearInterval: (): void => {},
        setTimeout: (fn: () => void, ms: number): number => { assert.equal(ms, 4000); timers.set(++next, fn); return next; },
        clearTimeout: (id: number): void => { timers.delete(id); },
        matchMedia: () => ({ matches: reduced }),
      } as never;
      globalThis.fetch = (async () => ({ ok: true, json: async () => ({state: present ? "busy" : "idle", users: present ? [{id:"docker:a",name:"worker",type:"docker",usage:"in_use"}] : [], errors: []}) })) as never;
      const target = new FakeElement("div"); const stop = watchGpuUsers(target as never, "en");
      await new Promise((resolve) => setTimeout(resolve, 0));
      present = false; fire?.(); await new Promise((resolve) => setTimeout(resolve, 0));
      assert.ok(target.textContent.includes("The GPU is free"));
      assert.equal(target.textContent.includes("Stopped"), !reduced);
      if (!reduced) {
        assert.ok(target.textContent.includes("worker stopped using the GPU."));
        timers.values().next().value?.();
        assert.equal(target.textContent.includes("worker"), false);
      } else assert.equal(timers.size, 0);
      stop();
    }
  } finally { globalThis.window = oldWindow; globalThis.fetch = oldFetch; }
});

test("plan requests forward AbortSignal without changing the selected source", async () => {
  const { postPlan } = await import("../src/api.js");
  const oldFetch = globalThis.fetch; const controller = new AbortController();
  const source = { kind: "official", version: "580.178.04", gpus: ["0000:01:00.0"] } as const;
  globalThis.fetch = (async (_url: string, init: RequestInit) => {
    assert.equal(init.signal, controller.signal);
    assert.deepEqual(JSON.parse(init.body as string), { source });
    return {ok: true, json: async () => ({result:"blocked"})};
  }) as never;
  try { await postPlan({ ...source, gpus: [...source.gpus] }, controller.signal); }
  finally { globalThis.fetch = oldFetch; }
});

test("a failed category query is never evidence that its apps stopped", async () => {
  const { watchGpuUsers } = await import("../src/gpu-users.js");
  const oldWindow = globalThis.window; const oldFetch = globalThis.fetch;
  let fire: (() => void) | undefined; let failed = false;
  globalThis.window = { setInterval: (fn: () => void): number => { fire = fn; return 1; }, clearInterval: (): void => {}, setTimeout: (): number => 1, clearTimeout: (): void => {} } as never;
  globalThis.fetch = (async () => ({ ok: true, json: async () => ({ state:"busy", users: [
    ...(!failed ? [{id:"docker:a",name:"docker-worker",type:"docker",usage:"in_use"}] : []),
    {id:"container:1",name:"instance",type:"incus",usage:"unknown"},
  ], errors: failed ? [{type:"docker",reason:{key:"reason.permission_denied"}}] : [] }) })) as never;
  const target = new FakeElement("div"); const stop = watchGpuUsers(target as never, "en");
  try {
    await new Promise((resolve) => setTimeout(resolve, 0)); failed = true; fire?.();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(target.textContent.includes("Stopped"), false);
    assert.match(target.textContent, /permission/);
  } finally { stop(); globalThis.window = oldWindow; globalThis.fetch = oldFetch; }
});
