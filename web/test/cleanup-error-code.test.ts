import { test } from "node:test";
import assert from "node:assert/strict";

import { installDom, FakeElement, FakeText } from "./dom-shim.js";
import { renderTask } from "../src/views/tasks.js";

installDom();

/** The interval callback of the page's poller, so a test can drive a re-read. */
let pollTick: (() => Promise<void>) | null = null;
(globalThis as unknown as { window: unknown }).window = {
  setInterval: (fn: () => void): number => {
    pollTick = async (): Promise<void> => {
      fn();
      await new Promise((resolve) => setTimeout(resolve, 0));
      await new Promise((resolve) => setTimeout(resolve, 0));
    };
    return 1;
  },
  clearInterval: (): void => {},
};

const fakeApp = {
  lang: "en" as const,
  addCleanup: (): void => {},
  sessionEnded: (): void => {},
  refreshLogin: async (): Promise<void> => {},
  reportTaskState(): void {},
};

type AnyNode = FakeElement | FakeText;

const isElement = (node: AnyNode): node is FakeElement => node instanceof FakeElement;

function allElements(node: AnyNode): FakeElement[] {
  if (!isElement(node)) return [];
  return [node, ...node.children.flatMap((child) => allElements(child))];
}

const hasClass = (node: FakeElement, name: string): boolean =>
  node.className.split(/\s+/).includes(name);

function errorOf(code: string, params: Record<string, unknown> = {}, raw: string | null = null) {
  return { code, params, command: null, exit: null, raw };
}

function detail(status: string, error: unknown, notice: unknown): unknown {
  return {
    task_id: "audit-task",
    status,
    phase: null,
    plan: null,
    loaded_version_start: null,
    loaded_version_end: null,
    download: null,
    replace: [],
    checks: [],
    restore: null,
    state_read_error: null,
    state_note: null,
    unit_query: null,
    gpu_users: null,
    version_change: false,
    error,
    notice,
    log: { lines: [], next: 0, reset: false },
  };
}

/** Render the task page; later payloads answer the poller's re-reads. */
async function render(payloads: unknown[]): Promise<FakeElement> {
  let call = 0;
  (globalThis as unknown as { fetch: unknown }).fetch = async (): Promise<unknown> => {
    const body = payloads[Math.min(call, payloads.length - 1)];
    call += 1;
    return { ok: true, status: 200, json: async (): Promise<unknown> => body };
  };
  const container = new FakeElement("div");
  renderTask(fakeApp as never, container as unknown as HTMLElement, "audit-task");
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
  return container;
}

const codeSection = (container: FakeElement): FakeElement[] =>
  allElements(container).filter((n) => hasClass(n, "error-code"));

/** The standalone section's code node holds the code as `code`; the code explanation holds it as a bold `strong`. */
const codeMentions = (container: FakeElement, code: string): FakeElement[] =>
  allElements(container).filter(
    (n) => (n.tagName === "CODE" || n.tagName === "STRONG") && n.textContent === code,
  );

/** The six codes whose notice is the known `code.explanation` template. */
const KNOWN = [
  "SAVE_ORIGINAL_FAILED",
  "DOWNLOAD_FAILED",
  "BUILD_FAILED",
  "BUILD_TIMEOUT",
  "WRITE_FAILED",
  "REPLACE_DEADLINE_EXCEEDED",
];

test("a known code.explanation notice of the same code drops the standalone section", async () => {
  for (const code of KNOWN) {
    const container = await render([
      detail("FAILED", errorOf(code), { key: "code.explanation", params: { code } }),
    ]);
    assert.equal(codeSection(container).length, 0, `${code}: standalone section removed`);
    assert.equal(codeMentions(container, code).length, 1, `${code}: named exactly once`);
  }
});

test("another notice, a different original code, an unknown template or no notice keeps the section", async () => {
  const other = await render([
    detail("FAILED", errorOf("GPU_IN_USE"), { key: "notice.gpu_in_use", params: {} }),
  ]);
  assert.equal(codeSection(other).length, 1);
  assert.equal(codeMentions(other, "GPU_IN_USE").length, 1);

  // The rendering rules overwrite `notice.params.code` with the error code, so the original, differing
  // code decides: keep the standalone section.
  const different = await render([
    detail("FAILED", errorOf("BUILD_FAILED"), {
      key: "code.explanation",
      params: { code: "DOWNLOAD_FAILED" },
    }),
  ]);
  assert.equal(codeSection(different).length, 1);
  assert.ok(codeMentions(different, "BUILD_FAILED").length >= 1);

  const unknown = await render([
    detail("FAILED", errorOf("NEW_FAILURE_CODE"), {
      key: "code.explanation",
      params: { code: "NEW_FAILURE_CODE" },
    }),
  ]);
  assert.equal(codeSection(unknown).length, 1);
  // The unknown template's own fallback repeats the code; this rule is about the standalone section,
  // so only its presence is asserted here.
  assert.ok(codeMentions(unknown, "NEW_FAILURE_CODE").length >= 1);

  const none = await render([detail("FAILED", errorOf("DOWNLOAD_FAILED"), null)]);
  assert.equal(codeSection(none).length, 1);
});

test("the failure reason and the raw output survive the deduplication", async () => {
  const container = await render([
    detail(
      "FAILED",
      errorOf(
        "DOWNLOAD_FAILED",
        { reason: { key: "reason.file_missing", params: {} } },
        "curl: (6) Could not resolve host",
      ),
      { key: "code.explanation", params: { code: "DOWNLOAD_FAILED" } },
    ),
  ]);
  assert.equal(codeSection(container).length, 0);
  assert.match(container.textContent, /the file does not exist/);
  const raw = allElements(container).filter((n) => hasClass(n, "raw-output"));
  assert.ok(
    raw.some((n) => n.textContent.includes("Could not resolve host")),
    "the raw command output is kept",
  );
});

test("a notice-only change rebuilds the terminal section", async () => {
  const code = "DOWNLOAD_FAILED";
  // The task stays RUNNING so the poller survives the first read; the terminal section is rendered
  // whenever the detail carries an error.
  const container = await render([
    detail("RUNNING", errorOf(code, { reason: { key: "reason.file_missing", params: {} } }), null),
    detail("RUNNING", errorOf(code, { reason: { key: "reason.file_missing", params: {} } }), {
      key: "code.explanation",
      params: { code },
    }),
  ]);
  assert.equal(codeSection(container).length, 1, "without a notice the section is shown");
  assert.ok(pollTick, "the poller is registered");
  await pollTick();
  assert.equal(codeSection(container).length, 0, "the notice-only change dropped the section");
});
