import { test } from "node:test";
import assert from "node:assert/strict";

import { installDom, domShim, liveRegions, FakeElement, FakeText } from "./dom-shim.js";
import { ApiError, type SetupProblem, type TaskDetail } from "../src/api.js";
import { apiErrorNotice, expandText, renderNotice, type Params } from "../src/dom.js";
import { startupItemText, startupProblemItems, setupHasUsernameProblem, settingProblemNoticeKey } from "../src/views/login.js";
import { logParams } from "../src/views/tasks.js";
import { restoreFailureReason, splitRawOutput } from "../src/rules.js";
import { openConfirmDialog } from "../src/views/plan.js";

installDom();

const asElement = (node: unknown): FakeElement => node as FakeElement;

test("confirmation renders the server's choices and warnings in both languages", () => {
  const doc = domShim();
  const create = doc.createElement;
  const body = new FakeElement("body");
  Object.assign(doc, { body });
  doc.createElement = (tag: string): FakeElement => Object.assign(new FakeElement(tag), {
    dataset: {},
    open: false,
    showModal(): void { this.open = true; },
  });
  const walk = (node: FakeElement | FakeText): FakeElement[] =>
    node instanceof FakeText ? [] : [node, ...node.children.flatMap(walk)];
  try {
    for (const lang of ["en", "zh"] as const) for (const uploadWarning of [false, true]) {
      body.textContent = "";
      openConfirmDialog({ lang } as never, "plan", {
        source: { kind: "official" }, version: "470.256.02", kernel: "6.12.105",
        truenas_version: "25.10", original: { saved: false }, save_available: true,
        confirmation: {
          original: "unavailable",
          buttons: [{ button: "save", enabled: false }, { button: "skip", enabled: true }],
          how: { key: "confirm.how.local", params: {} },
          warnings: [
            { key: "confirm.original.unsavable", params: {} },
            ...(uploadWarning ? [{ key: "confirm.upload_trust", params: {} }] : []),
          ],
        },
      } as never, new FakeElement("button") as never);
      const nodes = walk(body);
      const save = nodes.find((node) => node.tagName === "BUTTON"
        && node.textContent === expandText("confirm.save", {}, lang));
      assert.ok(save);
      assert.equal((save as unknown as { disabled: boolean }).disabled, true);
      assert.equal(save.getAttribute("aria-describedby"), "confirm-unsavable");
      assert.ok(body.textContent.includes(expandText("confirm.how.local", {}, lang)));
      assert.ok(body.textContent.includes(expandText("confirm.original.unsavable", {}, lang)));
      assert.equal(body.textContent.includes(expandText("confirm.community_patch", {}, lang)), false);
      assert.equal(body.textContent.includes(expandText("original.skip_warning", {}, lang)), false);
      const trustText = lang === "en"
        ? "The driver installer you uploaded will be built on this host and loaded into its kernel. Only use an installer you trust."
        : "你上传的驱动安装包会在这台主机上构建并加载进内核。请只使用你信任的安装包。";
      assert.equal(body.textContent.includes(trustText), uploadWarning);
    }
  } finally {
    doc.createElement = create;
  }
});

test("a rejected read renders the API's own reason", () => {
  const refused = apiErrorNotice(
    new ApiError(403, "auth.origin_mismatch", {}, undefined, null),
    "en",
  );
  assert.match(
    asElement(refused).textContent,
    /The request origin does not match this program's address and was refused\./,
  );

  assert.equal(liveRegions(asElement(refused)).length, 1);

  const invalid = apiErrorNotice(new ApiError(400, "request.invalid", {}, undefined, null), "zh");
  assert.match(asElement(invalid).textContent, /请求内容不合法。/);
});

test("a transport failure renders the generic internal error", () => {
  const notice = apiErrorNotice(new TypeError("Failed to fetch"), "en");
  assert.match(
    asElement(notice).textContent,
    /An internal error occurred in this program: request handling failed\./,
  );
});

test("response-level raw output stays outside the alert region", () => {
  const err = new ApiError(
    500,
    "internal",
    { reason: { key: "internal.data_dir_unreadable", params: {} } },
    "RAW_EXTERNAL_TEXT",
    null,
  );
  const frag = asElement(apiErrorNotice(err, "en"));
  assert.equal(frag.textContent.includes("RAW_EXTERNAL_TEXT"), true);
  assert.equal(
    liveRegions(frag).some((text) => text.includes("RAW_EXTERNAL_TEXT")),
    false,
  );
});

test("stderr reasons are split into a phrase and raw text", () => {
  const { params, raw } = splitRawOutput(
    {
      module: "nvidia_drm",
      unload_reason: { stderr: "rmmod: ERROR: Module nvidia_uvm is in use" },
      users: [{ name: "a", reason: { stderr: "second raw" } }],
    },
    { code: 7 },
  );
  assert.deepEqual(raw, ["rmmod: ERROR: Module nvidia_uvm is in use", "second raw"]);
  assert.deepEqual(params["unload_reason"], { key: "reason.exit_status", params: { code: 7 } });
  assert.deepEqual(params["users"], [
    { name: "a", reason: { key: "reason.exit_status", params: { code: 7 } } },
  ]);

  const keyed = splitRawOutput(
    { read_reason: { key: "reason.io_error", params: {} } },
    { code: 1 },
  );
  assert.deepEqual(keyed.params["read_reason"], { key: "reason.io_error", params: {} });
  assert.deepEqual(keyed.raw, []);
});

test("the unload-refused notice with a split reason keeps raw output outside its alert", () => {
  const { params, raw } = splitRawOutput(
    { module: "nvidia_drm", unload_reason: { stderr: "RAW_MARKER" }, old: "580.173.02" },
    { code: 1 },
  );
  const notice = renderNotice("notice.unload_refused", params, "en");
  assert.match(notice.textContent, /the command ended with exit code 1/);
  assert.equal(notice.textContent.includes("RAW_MARKER"), false);
  assert.equal(notice.getAttribute("role"), "alert");
  // The caller renders the returned raw text in a `pre.raw-output` sibling; appending it next to the
  // notice keeps it outside the alert element.
  const area = domShim().createElement("div");
  area.append(notice as unknown as FakeElement);
  for (const text of raw) {
    const pre = domShim().createElement("pre");
    pre.textContent = text;
    area.append(pre);
  }
  assert.equal(area.textContent.includes("RAW_MARKER"), true);
  assert.equal(
    liveRegions(area).some((text) => text.includes("RAW_MARKER")),
    false,
  );
});

test("one startup.failed problem carries the startup-check notice items", () => {
  const problem: SetupProblem = {
    key: "startup.failed",
    params: {
      items: [
        { key: "startup.pid_host", params: {} },
        { key: "startup.runtime", params: { runtime: "nvidia" } },
        {
          key: "startup.data_mapping",
          params: {
            mapping_reason: {
              key: "startup.mapping.inspect_failed",
              params: {
                read_reason: { key: "reason.exit_status", params: { code: 2 } },
              },
            },
          },
          raw: "docker: external text",
        },
      ],
    },
  };
  const items = startupProblemItems(problem);
  assert.deepEqual(
    items.map((item) => item.key),
    ["startup.pid_host", "startup.runtime", "startup.data_mapping"],
  );
  const dataMapping = items[2];
  assert.ok(dataMapping);
  // The sign-in page's startup-check notice line stops at the bracket  and never shows the external
  // command output.
  const loginLine = startupItemText(dataMapping, "en");
  assert.match(loginLine, /this container's information could not be read \(docker inspect\)/);
  assert.equal(loginLine.includes("exit code 2"), false);
  assert.equal(loginLine.includes("docker: external text"), false);

  const signedInLine = expandText(
    dataMapping.key,
    (dataMapping.params ?? {}) as Record<string, unknown>,
    "en",
  );
  assert.match(signedInLine, /the command ended with exit code 2/);
  assert.equal(signedInLine.includes("docker: external text"), false);
  // Only `startup.failed` is the login status-level container; other outer startup.* keys are not
  // container items either.
  assert.deepEqual(startupProblemItems({ key: "startup.pid_host", params: {} }), []);
  assert.deepEqual(startupProblemItems({ key: "startup.failed", params: {} }), []);
});

test("stderr write reasons stay structured and outside the alerts", () => {
  const detail = {
    error: {
      code: "RESTORE_NOT_WRITTEN",
      params: { cause: "write", write_reason: { stderr: "RAW_STDERR_MARKER" } },
      command: ["mv"],
      exit: { code: 1 },
      raw: "RAW_STDERR_MARKER",
    },
    checks: [],
    replace: [],
  } as unknown as TaskDetail;
  const reason = restoreFailureReason(detail, "en") as { key: string; params: Params };
  // The structured write_reason is still nested at this point: no early flattening.
  assert.deepEqual(reason.params["write_reason"], { stderr: "RAW_STDERR_MARKER" });
  const split = splitRawOutput({ restore_reason: reason }, { code: 1 });
  assert.deepEqual(split.raw, ["RAW_STDERR_MARKER"]);
  for (const [key, variant] of [
    ["notice.restore_failed", "before_write"],
    ["restore.two_failures", undefined],
  ] as const) {
    const notice = renderNotice(key, split.params, "en", { variant });
    assert.equal(notice.textContent.includes("RAW_STDERR_MARKER"), false);
    assert.equal(liveRegions(notice as unknown as FakeElement).some((t) => t.includes("RAW_STDERR_MARKER")), false);
    assert.match(notice.textContent, /the command ended with exit code 1/);
  }
});

test("the same username invalid line is shown only once", () => {
  const problem: SetupProblem = {
    key: "setting.invalid",
    params: { variable: "NVM_INITIAL_ADMIN_USERNAME" },
  };
  assert.equal(setupHasUsernameProblem([problem]), true);
  assert.equal(
    setupHasUsernameProblem([{ key: "setting.invalid", params: { variable: "NVM_PORT" } }]),
    false,
  );
  assert.equal(setupHasUsernameProblem([{ key: "startup.failed", params: {} }]), false);
  assert.equal(setupHasUsernameProblem([]), false);
  // The UTF-8 variant of the same line is matched too, so the username reason is not lost whichever
  // outer key the login status serializer uses.
  assert.equal(
    setupHasUsernameProblem([
      { key: "setting.invalid_utf8", params: { variable: "NVM_INITIAL_ADMIN_USERNAME" } },
    ]),
    true,
  );
});

test("invalid_utf8 selects the invalid-setting notice UTF-8 wording", () => {

  assert.equal(
    settingProblemNoticeKey({
      key: "setting.invalid",
      params: { variable: "NVM_PORT", invalid_utf8: true },
    }),
    "setting.invalid_utf8",
  );
  assert.equal(
    settingProblemNoticeKey({
      key: "setting.invalid",
      params: { variable: "NVM_PORT", requirement: { key: "setting.rule.port", params: {} } },
    }),
    "setting.invalid",
  );
  assert.equal(
    settingProblemNoticeKey({
      key: "setting.invalid",
      params: { variable: "NVM_PORT", invalid_utf8: false },
    }),
    "setting.invalid",
  );
  assert.equal(settingProblemNoticeKey({ key: "setting.unknown", params: {} }), "setting.unknown");

  const notice = renderNotice(
    settingProblemNoticeKey({ key: "setting.invalid", params: { variable: "NVM_INITIAL_ADMIN_PASSWORD", invalid_utf8: true } }),
    { variable: "NVM_INITIAL_ADMIN_PASSWORD" },
    "en",
  );
  assert.match(notice.textContent, /is not valid UTF-8/);
  assert.equal(notice.textContent.includes("is invalid:"), false);
});

test("a loaded-version reason code is expanded, not shown raw", () => {
  assert.deepEqual(logParams("log.loaded_version_unknown", { read_reason: "reason.io_error" })["read_reason"], {
    key: "reason.io_error",
    params: {},
  });
  assert.deepEqual(
    logParams("log.loaded_version_unknown", { read_reason: "reason.file_missing" })["read_reason"],
    { key: "reason.file_missing", params: {} },
  );
  assert.equal(logParams("log.loaded_version_unknown", {})["read_reason"], "UNKNOWN");
  const text = expandText(
    "log.loaded_version_unknown",
    logParams("log.loaded_version_unknown", { read_reason: "reason.io_error" }),
    "en",
  );
  assert.match(text, /a read error occurred/);
  assert.equal(text.includes("reason.io_error"), false);
});
