import { test } from "node:test";
import assert from "node:assert/strict";
import { installDom, FakeElement, FakeText, domShim } from "./dom-shim.js";
import { openCheckDialog } from "../src/views/plan.js";
import { renderTask } from "../src/views/tasks.js";

installDom();
const walk = (node: FakeElement | FakeText): FakeElement[] => node instanceof FakeText ? [] : [node, ...node.children.flatMap(walk)];
const expected = {
  zh: { busy: "显卡正被使用", unreadable: "无法读取显卡使用状态", taskUnreadable: "无法读取显卡使用状态", prefix: "无法更新：", task: "更新已停止：" },
  en: { busy: "GPU in use", unreadable: "GPU usage state could not be read", taskUnreadable: "GPU usage state could not be read", prefix: "Cannot update: ", task: "Update stopped: " },
};

for (const lang of ["zh", "en"] as const) for (const state of ["busy", "unreadable"] as const) {
  test(`check popup ${lang} ${state} title matches its GPU state`, () => {
    const doc = domShim();
    const create = doc.createElement.bind(doc);
    const body = new FakeElement("body");
    Object.assign(doc, { body });
    doc.createElement = (tag: string): FakeElement => {
      const node = create(tag);
      if (tag === "dialog") Object.assign(node, { dataset: {}, showModal: (): void => {} });
      return node;
    };
    try {
      openCheckDialog({ lang } as never, [], [], {
        state, users: [], errors: [], read_reason: { key: "reason.permission_denied", params: {} },
      }, new FakeElement("button") as never);
      const title = walk(body).find((node) => node.className === "dialog-title");
      assert.equal(title?.textContent, expected[lang].prefix + expected[lang][state]);
      assert.equal(body.children.length, 1);
      if (state === "unreadable") assert.ok(body.textContent.includes(lang === "zh" ? "无法读取显卡使用状态" : "GPU usage state could not be read"));
    } finally { doc.createElement = create; }
  });

  test(`task stopped ${lang} ${state} title agrees with the shared GPU list`, async () => {
    const oldFetch = globalThis.fetch;
    const oldWindow = globalThis.window;
    const cleanups: (() => void)[] = [];
    globalThis.window = { setInterval: (): number => 1, clearInterval: (): void => {} } as never;
    globalThis.fetch = (async () => ({ ok: true, json: async () => ({
      task_id: "title-check", status: "FAILED", phase: "unload", plan: null,
      loaded_version_start: null, loaded_version_end: null, download: null, replace: [], checks: [], restore: null,
      state_read_error: null, state_note: null, unit_query: null,
      error: { code: "GPU_IN_USE", params: { occupancy: state, read_reason: { key: "reason.permission_denied", params: {} } }, command: null, exit: null, raw: null },
      gpu_users: { users: [], errors: [] }, version_change: false,
      notice: { key: "notice.gpu_in_use", variant: state === "busy" ? "task" : "task_unreadable", params: {} },
      log: { lines: [], next: 0, reset: false },
    }) })) as never;
    try {
      const container = new FakeElement("div");
      renderTask({ lang, addCleanup: (fn: () => void): void => { cleanups.push(fn); }, reportTaskState: (): void => {}, setTaskDot: (): void => {} } as never, container as never, "title-check");
      await new Promise((resolve) => setTimeout(resolve, 0));
      const notice = walk(container).find((node) => node.className === "task-notice");
      assert.ok(notice);
      const title = walk(notice).find((node) => node.className === "notice-title");
      assert.equal(title?.textContent, expected[lang].task + (state === "unreadable" ? expected[lang].taskUnreadable : expected[lang][state]));
      if (state === "unreadable") assert.ok(notice.textContent.includes(lang === "zh" ? "无法读取显卡使用状态" : "GPU usage state could not be read"));
    } finally { cleanups.forEach((fn) => fn()); globalThis.fetch = oldFetch; globalThis.window = oldWindow; }
  });
}
