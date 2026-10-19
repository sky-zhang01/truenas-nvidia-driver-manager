import { test } from "node:test";
import assert from "node:assert/strict";

import { clearToken, getHome, postLogin, releaseTask } from "../src/api.js";

interface Seen {
  path: string;
  token: string | null;
  contentType: string | null;
}

function installSessionStorage(): Map<string, string> {
  const store = new Map<string, string>();
  const memory = {
    getItem(key: string): string | null {
      return store.get(key) ?? null;
    },
    setItem(key: string, value: string): void {
      store.set(key, value);
    },
    removeItem(key: string): void {
      store.delete(key);
    },
    clear(): void {
      store.clear();
    },
    key(index: number): string | null {
      return [...store.keys()][index] ?? null;
    },
    get length(): number {
      return store.size;
    },
  };
  Object.defineProperty(globalThis, "sessionStorage", {
    value: memory,
    configurable: true,
  });
  return store;
}

test("a password login stores the request token and later API calls send it", async () => {
  const store = installSessionStorage();
  const seen: Seen[] = [];
  const fetchImpl = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const path =
      typeof input === "string" ? input : input instanceof URL ? input.pathname : input.url;
    const headers = new Headers(init?.headers);
    seen.push({
      path,
      token: headers.get("X-NVM-Token"),
      contentType: headers.get("Content-Type"),
    });
    if (path === "/api/login") {
      return new Response(JSON.stringify({ token: "abc123" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response(JSON.stringify({ kernel: "ok" }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };
  Object.defineProperty(globalThis, "fetch", {
    value: fetchImpl,
    configurable: true,
    writable: true,
  });

  await postLogin("admin", "hunter12");
  assert.equal(store.get("nvm.token"), "abc123");
  assert.equal(seen[0]?.token, null);
  assert.equal(seen[0]?.contentType, "application/json");

  await getHome();
  assert.equal(seen[1]?.path, "/api/home");
  assert.equal(seen[1]?.token, "abc123");

  clearToken();
  assert.equal(store.has("nvm.token"), false);
  await getHome();
  assert.equal(seen[2]?.token, null);
});


test("releasing recovery material posts without a body and carries the session token", async () => {
  const store = installSessionStorage();
  store.set("nvm.token", "release-request-token");
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (input, init): Promise<Response> => {
    assert.equal(input, "/api/tasks/task%2Fid/release");
    assert.equal(init?.method, "POST");
    assert.equal(init?.body, undefined);
    assert.equal(init?.credentials, "same-origin");
    assert.equal(new Headers(init?.headers).get("X-NVM-Token"), "release-request-token");
    return new Response(JSON.stringify({ recovery_material_kept: false, released_by: "later-task" }));
  };
  try {
    const result = await releaseTask("task/id");
    assert.equal(result.recovery_material_kept, false);
    assert.equal(result.released_by, "later-task");
  } finally {
    globalThis.fetch = originalFetch;
  }
});
