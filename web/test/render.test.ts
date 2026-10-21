import { test } from "node:test";
import assert from "node:assert/strict";

import { expand, expandText, messageLine, type Params } from "../src/dom.js";
import { logParams, gpuDiagnosticText } from "../src/views/tasks.js";
import { FakeElement, FakeText, domShim, installDom } from "./dom-shim.js";

test("two-failures notice shows only this occurrence's readings", () => {
  const text = expandText(
    "restore.two_failures",
    {
      check_reason: "驱动扩展刷新 的观测值为 x,期望值为 y。",
      original: "580.178.04",
      restore_reason: "写入原版驱动文件之前已超过执行期限（10 分钟）",
    },
    "zh",
  );
  assert.equal(text.includes("三种写法"), false);
  assert.equal(text.includes("UNKNOWN"), false);
  assert.match(text, /更新后的成功检查未通过：驱动扩展刷新/);
  assert.match(text, /恢复原版驱动 580\.178\.04 失败：写入原版驱动文件之前已超过执行期限/);
});

test("placeholders are filled inside code spans", () => {
  const params = {
    command: "modprobe nvidia",
    exit: { key: "log.exit.code", params: { code: 1 } },
    ms: 12,
  };
  const exp = expand("log.cmd", params, "zh");
  const code = exp.segments.flatMap((s) => s.nodes).find((n) => n.kind === "code");
  assert.equal(code?.text, "modprobe nvidia");
  assert.equal(expandText("log.cmd", params, "zh").includes("{command}"), false);
});

test("account-reset help describes setup code renewal and the data that stays", () => {
  for (const lang of ["en", "zh"] as const) {
    const exp = expandText("help.reset_account", {}, lang);
    assert.match(exp, lang === "en" ? /new setup code appears in the app logs/ : /新的设置码/);
    assert.match(exp, lang === "en" ? /tasks and drivers are unaffected/ : /任务和驱动不受影响/);
    assert.equal(exp.includes("NVM_INITIAL_ADMIN_PASSWORD"), false);
  }
});

test("an in-use unload wait is readable in Chinese and English", () => {
  const params = logParams("log.module_unload_wait", {
    module: "nvidia",
    waited_ms: 2200,
    retries: 1,
    result: "unloaded",
  });
  assert.deepEqual(params["result"], { key: "log.unload_wait.unloaded", params: {} });
  const zh = expandText("log.module_unload_wait", params, "zh");
  const en = expandText("log.module_unload_wait", params, "en");
  assert.match(zh, /nvidia/);
  assert.match(zh, /2200/);
  assert.match(zh, /1/);
  assert.match(zh, /已卸载/);
  assert.equal(zh.includes("unloaded"), false);
  assert.match(en, /nvidia/);
  assert.match(en, /2200/);
  assert.match(en, /unloaded/);
  assert.equal(en.includes("已卸载"), false);
});

test("runner log field names map to the catalog placeholders", () => {
  const step = logParams("log.build_step", { build_step: "capture" });
  assert.deepEqual(step["build_step"], { key: "log.build_step.capture", params: {} });
  const text = expandText(
    "log.build_step",
    step,
    "zh",
  );
  assert.equal(text.includes("undefined"), false);
  assert.match(text, /收取安装程序写下的文件/);
  const mode = logParams("log.build_install_mode", { mode: "single" });
  assert.deepEqual(mode["install_mode"], { key: "log.install_mode.single", params: {} });
  assert.match(expandText("log.build_install_mode", mode, "zh"), /一遍/);
});

import { checkFailureItems } from "../src/rules.js";
import type { CheckRecord } from "../src/api.js";

function seg(exp: ReturnType<typeof expand>, kind: string): string {
  return exp.segments
    .filter((s) => s.kind === kind)
    .map((s) => s.nodes.map((n) => n.text).join(""))
    .join("\n");
}

test("GPU-use notice: the page form, its unreadable form and the two task-page forms", () => {
  const page = expand("notice.gpu_in_use", {}, "zh");
  assert.equal(page.level, "error");
  assert.match(seg(page, "title"), /无法更新：显卡正被使用/);
  assert.equal(messageLine("notice.gpu_in_use", "list"), "{name} · {type}");

  const unreadable = expand("notice.gpu_in_use", { read_reason: { key: "reason.io_error", params: {} } }, "zh", "unreadable");
  assert.match(seg(unreadable, "title"), /无法读取显卡使用状态/);
  assert.match(seg(unreadable, "body"), /读取出错/);

  const task = expand("notice.gpu_in_use", {}, "en", "task");
  assert.equal(seg(task, "title"), "Update stopped: GPU in use");
  const taskUnreadable = expand(
    "notice.gpu_in_use",
    { read_reason: { key: "reason.permission_denied", params: {} } },
    "en",
    "task_unreadable",
  );
  assert.equal(seg(taskUnreadable, "title"), "Update stopped: GPU usage state could not be read");
  assert.match(seg(taskUnreadable, "body"), /permission was denied/);
});

test("unload-refused notice: the reload-failed writing", () => {
  const plain = expand("notice.unload_refused", { module: "nvidia_uvm", unload_reason: "Module nvidia_uvm is in use", old: "580.178.04" }, "en");
  assert.match(seg(plain, "body"), /Module nvidia_uvm is in use/);
  assert.match(seg(plain, "body"), /driver is still 580\.178\.04/);

  const failed = expand(
    "notice.unload_refused",
    { module: "nvidia_uvm", unload_reason: "in use", reload_reason: "nvidia: Operation not permitted" },
    "zh",
    "reload_failed",
  );
  assert.match(seg(failed, "body"), /重新加载失败/);
  assert.match(seg(failed, "body"), /显卡应用可能无法启动/);
});

test("restore-stopped notice: busy, unreadable, unload-refused and reload-failed", () => {
  const busy = expand("notice.restore_stopped", { target: "595.104.02", check_reason: "x", original: "580.178.04", current: "595.104.02" }, "en", "busy");
  assert.match(seg(busy, "body"), /the GPU stayed in use/);

  const unreadable = expand(
    "notice.restore_stopped",
    { target: "595.104.02", check_reason: "x", original: "580.178.04", read_reason: "the file does not exist" },
    "en",
    "unreadable",
  );
  assert.match(seg(unreadable, "body"), /GPU usage state could not be read/);

  const refused = expand(
    "notice.restore_stopped",
    { target: "595.104.02", check_reason: "x", original: "580.178.04", module: "nvidia", unload_reason: "in use", current: "595.104.02" },
    "zh",
    "unload_refused",
  );
  assert.match(seg(refused, "body"), /卸载驱动模块 nvidia 被系统拒绝/);

  const failed = expand(
    "notice.restore_stopped",
    { target: "595.104.02", check_reason: "x", original: "580.178.04", module: "nvidia", unload_reason: "in use", reload_reason: "raw", target2: "" },
    "zh",
    "unload_refused.reload_failed",
  );
  assert.match(seg(failed, "body"), /重新加载失败：raw/);
});

test("restore-failed notice: before-write and written, with the reload-failed writing", () => {
  const before = expand(
    "notice.restore_failed",
    { target: "595.104.02", check_reason: "x", restore_reason: "y", original: "580.178.04" },
    "en",
    "before_write",
  );
  assert.match(seg(before, "body"), /the driver file is still 595\.104\.02/i);
  assert.match(seg(before, "action"), /select the original driver 580\.178\.04/i);

  const beforeReload = expand(
    "notice.restore_failed",
    { target: "595.104.02", check_reason: "x", restore_reason: "y", original: "580.178.04", reload_reason: "nvidia: EPERM" },
    "zh",
    "before_write.reload_failed",
  );
  assert.match(seg(beforeReload, "body"), /重新加载失败：nvidia: EPERM/);

  const written = expand(
    "notice.restore_failed",
    { target: "595.104.02", check_reason: "x", restore_reason: "y", original: "580.178.04" },
    "zh",
    "written",
  );
  assert.match(seg(written, "action"), /请另选一个官网版本/);
});

test("no-original notice: the three no-usable-original reasons are nested", () => {
  const saved = expand(
    "notice.no_restore",
    {
      target: "595.104.02",
      check_reason: "c",
      no_original_reason: { key: "restore.no_original.not_saved", params: {} },
    },
    "en",
  );
  assert.match(seg(saved, "body"), /no original driver was saved for this update/);

  const mismatch = expand(
    "notice.no_restore",
    {
      target: "595.104.02",
      check_reason: "c",
      no_original_reason: { key: "restore.no_original.checksum_mismatch", params: {} },
    },
    "zh",
  );
  assert.match(seg(mismatch, "body"), /原版驱动文件校验不符/);

  const unreadable = expand(
    "restore.no_original",
    {
      no_original_reason: {
        key: "restore.no_original.unreadable",
        params: { read_reason: { key: "reason.file_missing", params: {} } },
      },
    },
    "en",
  );
  assert.match(seg(unreadable, "body"), /could not be read: the file does not exist/);
});

test("two-failures notice: the two records, in both languages", () => {
  const params: Params = {
    check_reason: "driver version was `nvidia-smi` 580.178.04, `/proc/driver/nvidia/version` 580.178.04; expected 595.104.02",
    original: "580.178.04",
    restore_reason: { key: "restore.fail.write", params: { write_reason: { key: "write.rename_failed", params: {} } } },
  };
  const zh = expand("restore.two_failures", params, "zh");
  assert.match(seg(zh, "body"), /更新后的成功检查未通过：driver version was/);
  assert.match(seg(zh, "body"), /mv -T 失败/);
  const en = expand("restore.two_failures", params, "en");
  assert.match(seg(en, "body"), /Restoring the original driver 580\.178\.04 failed/);
  assert.match(seg(en, "body"), /mv -T failed/);
  assert.ok(
    zh.segments.some((s) => s.nodes.some((n) => n.kind === "code" && n.text === "mv -T")),
    "nested reason markup is preserved",
  );

  const deadline = expand(
    "restore.two_failures",
    { original: "580.178.04", restore_reason: { key: "restore.fail.deadline", params: { deadline: 10 } } },
    "en",
  );
  assert.match(seg(deadline, "body"), /execution deadline \(10 minutes\)/);
});

test("both refresh readings agree, refresh item", () => {
  const rec: CheckRecord = {
    pass: "update",
    since_before: { value: 1_759_050_930_123_456 },
    since_after: { value: 1_759_050_930_123_456 },
    refresh_ok: false,
    nvidia_smi: { value: "595.104.02" },
    proc_version: { value: "595.104.02" },
    expected: "595.104.02",
    version_ok: true,
    passed: false,
  };
  const items = checkFailureItems(rec);
  assert.equal(items.length, 1);
  const en = expandText("check.reason", items[0] as unknown as Params, "en");
  assert.match(en, /driver extension refresh was .* before and .* after the refresh/);
  assert.match(en, /expected a confirmed unmerge and subsequent merge, with a merge time different from before/);
  const zh = expandText("check.reason", items[0] as unknown as Params, "zh");
  assert.match(zh, /驱动扩展刷新 的观测值为 刷新前 .*、刷新后 .*/);
});

test("version item with an unreadable side", () => {
  const rec: CheckRecord = {
    pass: "restore",
    since_before: { value: 1 },
    since_after: { value: 2 },
    refresh_ok: true,
    nvidia_smi: { unknown: { key: "reason.io_error", params: {} } },
    proc_version: { value: "580.178.04" },
    expected: "595.104.02",
    version_ok: false,
    passed: false,
  };
  const items = checkFailureItems(rec);
  assert.equal(items.length, 1);
  const exp = expand("check.reason", items[0] as unknown as Params, "en");
  const en = seg(exp, "body");
  assert.match(en, /driver version was nvidia-smi unreadable, \/proc\/driver\/nvidia\/version 580\.178\.04; expected 595\.104\.02/);
  assert.ok(
    exp.segments.some((s) => s.nodes.some((n) => n.kind === "code" && n.text === "nvidia-smi")),
    "the nested reading keeps its code markup",
  );
});

test("the refresh item when a reading is missing", () => {
  const rec: CheckRecord = {
    pass: "update",
    since_before: { not_loaded: true },
    since_after: { value: 5 },
    refresh_ok: false,
    nvidia_smi: { value: "x" },
    proc_version: { value: "x" },
    expected: "x",
    version_ok: true,
    passed: false,
  };
  const items = checkFailureItems(rec);
  const exp = expand("check.reason", items[0] as unknown as Params, "en");
  assert.match(seg(exp, "body"), /no nvidia extension on \/usr before the refresh/);
  assert.ok(
    exp.segments.some((s) => s.nodes.some((n) => n.kind === "code" && n.text === "/usr")),
    "the nested reading keeps its code markup",
  );
});

test("code explanation dispatches on the error code and honours the reload flag", () => {
  const dl = expand("code.explanation", { code: "DOWNLOAD_FAILED" }, "en");
  assert.match(seg(dl, "body"), /The download failed;/);
  const write = expand(
    "code.explanation",
    { code: "WRITE_FAILED", reload_failed: true, write_reason: "x", reload_reason: "y" },
    "zh",
  );
  assert.match(seg(write, "body"), /重新加载失败：y/);
  const before = expand(
    "code.explanation",
    { code: "WRITE_FAILED", reload_failed: false, write_reason: "x", old: "580.178.04" },
    "en",
  );
  assert.match(seg(before, "body"), /the driver is still 580\.178\.04/);
});

test("the official toolkit floor reason expands in both task languages", () => {
  const reason = {
    key: "download.toolkit_too_old",
    params: { driver_version: "615.71.09", toolkit_version: "1.20.0" },
  };
  const zh = expandText("task.error_reason", { reason }, "zh");
  const en = expandText("task.error_reason", { reason }, "en");
  assert.equal(zh, "失败原因：toolkit 源里 4 个包都有的最新版本 1.20.0 低于驱动 615.71.09 所需的 1.20.1。");
  assert.equal(en, "Reason: the newest toolkit version 1.20.0 shared by all four packages in the toolkit repository is below the 1.20.1 required by driver 615.71.09.");
});

test("the runtime flags select the writings", () => {
  const refused = {
    module: "nvidia_uvm",
    unload_reason: "in use",
    old: "580.178.04",
    reload_reason: "nvidia: EPERM",
  };
  // `reload_failed` alone selects the not-all-reloaded writing.
  const failed = expand("notice.unload_refused", { ...refused, reload_failed: true }, "en");
  assert.match(seg(failed, "body"), /Reloading failed/);
  // Nothing unloaded: the "already reloaded" sentence is left out.
  const none = expand("notice.unload_refused", { ...refused, not_unloaded: true }, "en");
  assert.equal(seg(none, "body").includes("already unloaded have been reloaded"), false);
  assert.match(seg(none, "body"), /the driver is still 580\.178\.04/);
  const write = expand(
    "code.explanation",
    { code: "WRITE_FAILED", reload_failed: false, not_unloaded: true, write_reason: "x", old: "580.178.04" },
    "en",
  );
  assert.equal(seg(write, "body").includes("already unloaded have been reloaded"), false);
  assert.match(seg(write, "body"), /the driver is still 580\.178\.04/);
});

test("restore-stopped notice with an empty application list keeps its Local-drivers action", () => {
  const params = {
    target: "595.104.02",
    check_reason: "x",
    original: "580.178.04",
    current: "595.104.02",
    empty_list: true,
  };
  const busy = expand("notice.restore_stopped", { ...params }, "en", "busy");
  assert.equal(seg(busy, "list"), "");
  assert.match(
    seg(busy, "action"),
    /Select the original driver in Local drivers and update again\. If an app keeps restarting, stop it first\./,
  );
  assert.equal(seg(busy, "action").includes("Stop the applications below"), false);
  const unreadable = expand(
    "notice.restore_stopped",
    { ...params, read_reason: "the file does not exist" },
    "zh",
    "unreadable",
  );
  assert.equal(seg(unreadable, "list"), "");
  assert.match(seg(unreadable, "action"), /请在「本地驱动」中选择原版驱动 580\.178\.04 重新更新。/);
});

test("runner log records map to the placeholder values", () => {
  const deadline = logParams("log.build_deadline_set", { minutes: 30, deadline_ms: 1_790_581_000_000 });
  assert.match(String(deadline["time"]), /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  assert.match(
    expandText("log.build_deadline_set", deadline, "en"),
    /Download and build must finish by \d{4}-\d{2}-\d{2} \d{2}:\d{2}/,
  );
  assert.equal(expandText("log.build_deadline_set", deadline, "en").includes("UNKNOWN"), false);

  const refresh = logParams("log.check_item", {
    check_item: "refresh",
    passed: true,
    since_before: { value: 1_759_050_930_123_456 },
    since_after: { value: 1_759_050_930_223_456 },
  });
  assert.deepEqual(refresh["check_item"], { key: "check.item.refresh", params: {} });
  assert.deepEqual(refresh["check_result"], { key: "check.pass", params: {} });
  const refreshText = expandText("log.check_item", refresh, "en");
  assert.match(refreshText, /driver extension refresh: passed \(observed .* before and .* after the refresh; expected a confirmed unmerge and subsequent merge, with a merge time different from before\)/);

  const version = logParams("log.check_item", {
    check_item: "version",
    passed: false,
    nvidia_smi: { value: "580.178.04" },
    proc_version: { value: "580.178.04" },
    expected: "595.104.02",
  });
  assert.deepEqual(version["check_result"], { key: "check.fail", params: {} });
  const versionText = expandText("log.check_item", version, "en");
  assert.match(versionText, /driver version: failed \(observed nvidia-smi 580\.178\.04, \/proc\/driver\/nvidia\/version 580\.178\.04; expected 595\.104\.02\)/);

  const users = logParams("log.gpu_users", {
    applications: [{ name: "jellyfin", kind: "docker" }],
    errors: [],
  });
  assert.match(
    expandText("log.gpu_users", users, "en"),
    /Applications that requested the GPU: jellyfin · Docker app\./,
  );
  const none = logParams("log.gpu_users", { applications: [], errors: [] });
  assert.match(expandText("log.gpu_users", none, "en"), /Applications that requested the GPU: none\./);
  const failedCategory = logParams("log.gpu_users", {
    applications: [],
    errors: [{ kind: "incus" }],
  });
  assert.match(
    expandText("log.gpu_users", failedCategory, "en"),
    /Applications that requested the GPU: Incus instance query failed\./,
  );
});

test("every parameter is a text node, never markup", () => {
  const exp = expand(
    "query.failed",
    { query_object: { key: "query.object.catalog", params: {} }, reason: "<b>x</b>" },
    "en",
  );
  const text = seg(exp, "body");
  assert.match(text, /<b>x<\/b>/);
  assert.equal(exp.segments[0]?.nodes.some((n) => n.kind === "link"), false);
});

test("an unknown reason key is shown as the key with its params", () => {
  // A reason code written by a newer build has no catalog entry: the raw key and its parameters are
  // shown instead of a hidden placeholder.
  const exp = expand("reason.brand_new_cause", { detail: 7 }, "zh");
  const body = exp.segments.flatMap((s) => s.nodes).map((n) => n.text).join("");
  assert.match(body, /reason\.brand_new_cause/);
  assert.match(body, /"detail":7/);
  // Keys that name inherited object properties are unknown keys too.
  for (const key of ["constructor", "toString", "__proto__", "hasOwnProperty"]) {
    const inherited = expand(key, {}, "en");
    const text = inherited.segments.flatMap((s) => s.nodes).map((n) => n.text).join("");
    assert.equal(text, key);
    assert.equal(messageLine(key, "body"), null);
  }
});

test("log.restore_none expands the no-original reason, not the state-read-error notice domain", () => {
  // The state file / log payload carries the complete `restore.no_original.*` key.
  const asString = logParams("log.restore_none", { reason: "restore.no_original.not_saved" });
  const stringText = expandText("log.restore_none", asString, "en");
  assert.match(
    stringText,
    /The original driver is not restored: no original driver was saved for this update\./,
  );
  assert.equal(stringText.includes("state_read_error.reason"), false);
  assert.equal(stringText.includes("restore.no_original.restore"), false);

  const asField = logParams("log.restore_none", {
    reason: { key: "restore.no_original.checksum_mismatch", params: {} },
  });
  assert.match(
    expandText("log.restore_none", asField, "en"),
    /The original driver is not restored: the original driver file failed verification\./,
  );

  // An unreadable original keeps its nested {read_reason}, whether the log carries it beside the key
  // or inside the reason field.
  const sibling = logParams("log.restore_none", {
    reason: "restore.no_original.unreadable",
    read_reason: "reason.io_error",
  });
  assert.match(
    expandText("log.restore_none", sibling, "en"),
    /the original driver file could not be read: a read error occurred/,
  );
  const nested = logParams("log.restore_none", {
    reason: {
      key: "restore.no_original.unreadable",
      params: { read_reason: { key: "reason.permission_denied", params: {} } },
    },
  });
  const nestedText = expandText("log.restore_none", nested, "en");
  assert.match(nestedText, /the original driver file could not be read: permission was denied/);
  assert.equal(nestedText.includes("[object Object]"), false);
});

/** The element children of one shim node, in order. */
function elements(node: FakeElement): FakeElement[] {
  return node.children.filter((n): n is FakeElement => n instanceof FakeElement);
}

test("the sign-in page carries the usage line and the collapsed account guidance", async () => {
  installDom();
  const { renderLogin } = await import("../src/views/login.js");
  const login = {
    logged_in: false,
    setup: { problems: [] },
    credentials: "ok",
    show_reset_help: false,
  };
  type RenderArgs = Parameters<typeof renderLogin>;
  const app = { lang: "zh", logoutDone: false, login } as unknown as RenderArgs[0];

  const container = domShim().createElement("main") as unknown as HTMLElement;
  renderLogin(app, container);
  const shim = container as unknown as FakeElement;

  const stack = elements(shim).find((n) => n.className === "login-stack");
  assert.ok(stack, "the sign-in group is missing");
  const els = elements(stack);

  assert.equal(els[0]?.tagName, "H2");
  const intro = els[1];
  assert.ok(intro, "the usage line is missing");
  assert.equal(intro.tagName, "P");
  assert.match(intro.textContent, /用你的管理员账号登录/);

  // One collapsed guidance group, placed after the form (never a tutorial wall before it), with the
  // short next step visible even when collapsed.
  const formAt = els.findIndex((n) => n.tagName === "FORM");
  const hint = els.find((n) => n.className === "login-help-hint");
  const details = els.find((n) => n.tagName === "DETAILS");
  assert.ok(formAt !== -1 && hint && details, "form, hint and guidance must all exist");
  assert.ok(els.indexOf(hint) > formAt, "the hint must follow the form");
  assert.ok(els.indexOf(details) > els.indexOf(hint), "the guidance must follow the hint");
  assert.match(hint.textContent, /联系部署者/);
  assert.equal(details.getAttribute("open"), null, "the guidance starts collapsed");
  const summary = elements(details).find((n) => n.tagName === "SUMMARY");
  assert.ok(summary, "the guidance has a native summary entry");
  assert.match(summary.textContent, /没有账号/);

  const list = elements(details).find((n) => n.tagName === "UL");
  assert.ok(list, "the guidance list is missing");
  const items = elements(list);
  assert.equal(items.length, 3);
  const itemText = items.map((n) => n.textContent).join("\n");
  assert.match(itemText, /设置页/);
  assert.match(itemText, /账号会保留/);
  assert.match(itemText, /TrueNAS.*命令行/);

  const codes: string[] = [];
  const walk = (node: FakeElement | FakeText): void => {
    if (node instanceof FakeText) return;
    if (node.tagName === "CODE") codes.push(node.textContent);
    for (const child of node.children) walk(child);
  };
  for (const child of details.children) walk(child);
  assert.equal(codes.includes("NVM_INITIAL_ADMIN_PASSWORD"), false);

  // The default page carries no conditioned account-reset help file path and no secret value; only
  // variable names and generic wording.
  const pageText = stack.textContent;
  assert.equal(pageText.includes("NVM_INITIAL_ADMIN_PASSWORD"), false);
  assert.equal(pageText.includes("initial-password"), false);

  const containerEn = domShim().createElement("main") as unknown as HTMLElement;
  renderLogin({ lang: "en", logoutDone: false, login } as unknown as RenderArgs[0], containerEn);
  const shimEn = (containerEn as unknown as FakeElement).children.find(
    (n): n is FakeElement => n instanceof FakeElement && n.className === "login-stack",
  );
  assert.ok(shimEn, "the English sign-in group is missing");
  const detailsEn = elements(shimEn).find((n) => n.tagName === "DETAILS");
  assert.ok(detailsEn, "the English guidance is missing");
  assert.match(detailsEn.textContent, /No account|administrator account/);
  assert.match(elements(shimEn)[1]?.textContent ?? "", /administrator account/);
});


test("GPU diagnostic line shows names and PCI addresses or the actual read reason in both languages", () => {
  for (const lang of ["en", "zh"] as const) {
    assert.equal(gpuDiagnosticText({ value: [{ name: "NVIDIA Example", pci_address: "00000000:01:00.0" }] }, lang), "NVIDIA Example (00000000:01:00.0)");
    assert.match(gpuDiagnosticText({ unknown: { key: "reason.exit_status", params: { code: 9 } } }, lang), /9/);
    assert.equal(gpuDiagnosticText(null, lang), expandText("task.gpus.not_read", {}, lang));
    assert.equal(gpuDiagnosticText({ value: [] }, lang), expandText("task.gpus.none", {}, lang));
    assert.match(expandText("task.gpus", {}, lang), /nvidia-smi/);
  }
});
