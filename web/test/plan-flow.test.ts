import { test } from "node:test";
import assert from "node:assert/strict";
import { runPlanFlow } from "../src/plan-flow.js";
import type { PlanResponse } from "../src/api.js";

const ready = { result: "ready", plan_id: "checked-plan", dialog: {} } as PlanResponse;
for (const result of [ready, { result: "blocked", failures: [], gpu: null }, { result: "uncomputable", items: [] }] as PlanResponse[]) {
  test(`plan flow dispatches ${result.result} once and always finishes`, async () => {
    const oldFetch = globalThis.fetch;
    const calls: string[] = [];
    const controller = new AbortController();
    globalThis.fetch = (async (url: unknown, init?: RequestInit) => {
      assert.equal(url, "/api/plans");
      assert.equal(init?.signal, controller.signal);
      assert.deepEqual(JSON.parse(String(init?.body)), { source: { kind: "local", row_id: "stored-driver" } });
      return { ok: true, json: async () => result };
    }) as never;
    try {
      await runPlanFlow({
        source: { kind: "local", row_id: "stored-driver" }, signal: controller.signal, owns: () => true,
        ready: () => { calls.push("ready"); }, blocked: () => { calls.push("blocked"); },
        uncomputable: () => { calls.push("uncomputable"); }, failed: () => { calls.push("failed"); },
        finish: () => { calls.push("finish"); },
      });
      assert.deepEqual(calls, [result.result, "finish"]);
    } finally { globalThis.fetch = oldFetch; }
  });
}

test("an upload plan that loses its dialog releases a late ready plan without opening it", async () => {
  const oldFetch = globalThis.fetch;
  const calls: string[] = [];
  globalThis.fetch = (async () => ({ ok: true, json: async () => ready })) as never;
  try {
    await runPlanFlow({
      source: { kind: "upload", group_id: "uploaded-files", version: "580.178.04", gpus: [] }, owns: () => false,
      ready: () => { calls.push("ready"); }, blocked: () => { calls.push("blocked"); },
      uncomputable: () => { calls.push("uncomputable"); }, failed: () => { calls.push("failed"); },
      orphanedReady: (id) => { calls.push(id); }, finish: () => { calls.push("finish"); },
    });
    assert.deepEqual(calls, ["checked-plan", "finish"]);
  } finally { globalThis.fetch = oldFetch; }
});

for (const owns of [true, false]) {
  test(`plan request failures ${owns ? "reach the live owner" : "are discarded after ownership changes"}`, async () => {
    const oldFetch = globalThis.fetch;
    const calls: string[] = [];
    globalThis.fetch = (async () => { throw new Error("request failed"); }) as never;
    try {
      await runPlanFlow({
        source: { kind: "official", version: "580.178.04", gpus: [] }, owns: () => owns,
        ready: () => { calls.push("ready"); }, blocked: () => { calls.push("blocked"); },
        uncomputable: () => { calls.push("uncomputable"); },
        failed: async () => { await Promise.resolve(); calls.push("failed"); },
        finish: () => { calls.push("finish"); },
      });
      assert.deepEqual(calls, owns ? ["failed", "finish"] : ["finish"]);
    } finally { globalThis.fetch = oldFetch; }
  });
}
