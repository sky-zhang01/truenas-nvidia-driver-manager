import { test } from "node:test";
import assert from "node:assert/strict";

import type { HomeResponse, HomeTaskRecord } from "../src/api.js";
import { formatTaskTime } from "../src/dom.js";
import { MESSAGES } from "../src/messages.js";
import { interruptCopyKey, plannedSteps } from "../src/task-steps.js";
import { installDom, domShim, FakeElement, FakeText } from "./dom-shim.js";

test("formatTaskTime labels the local zone and rejects invalid input", () => {
  assert.equal(formatTaskTime(null), "UNKNOWN");
  assert.equal(formatTaskTime(undefined), "UNKNOWN");
  assert.equal(formatTaskTime(Number.NaN), "UNKNOWN");
  const ms = Date.UTC(2026, 9, 8, 12, 15, 7);
  const d = new Date(ms);
  const pad = (n: number): string => String(n).padStart(2, "0");
  const offset = -d.getTimezoneOffset();
  const sign = offset >= 0 ? "+" : "-";
  const abs = Math.abs(offset);
  const zone = `UTC${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
  const expected =
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())} ${zone}`;
  assert.equal(formatTaskTime(ms), expected);
  assert.match(formatTaskTime(ms), / UTC[+-]\d{2}:\d{2}$/);
});

test("interrupt copy follows the phase", () => {
  assert.equal(interruptCopyKey("download"), "home.interrupted.file_kept");
  assert.equal(interruptCopyKey("build"), "home.interrupted.file_kept");
  assert.equal(interruptCopyKey("read_loaded_version"), "home.interrupted.file_kept");
  assert.equal(interruptCopyKey("save_original"), "home.interrupted.file_kept");
  assert.equal(interruptCopyKey("unload"), "home.interrupted.unload");
  // Every phase after the modules were unloaded warns that they may not be back, including the restore side.
  for (const phase of [
    "write",
    "refresh",
    "reload",
    "restore_unload",
    "restore_write",
    "restore_refresh",
    "restore_reload",
  ]) {
    assert.equal(interruptCopyKey(phase), "home.interrupted.modules", phase);
  }
  for (const phase of ["align_modules", "restore_align_modules"]) {
    assert.equal(interruptCopyKey(phase), "home.interrupted.file_written");
  }
  // The check phases run with the modules loaded: the generic writing is accurate for them.
  assert.equal(interruptCopyKey("check"), "home.interrupted");
  assert.equal(interruptCopyKey("restore_check"), "home.interrupted");
  assert.equal(interruptCopyKey("read_loaded_version_end"), "home.interrupted");
  assert.equal(interruptCopyKey(null), "home.interrupted.phase_unknown");
  assert.equal(interruptCopyKey(""), "home.interrupted.phase_unknown");
});

test("planned steps follow source and original", () => {
  assert.deepEqual(plannedSteps(null, false), []);
  assert.deepEqual(plannedSteps({ source: { kind: "local" }, original: "skip" }, false), [
    "read_loaded_version",
    "unload",
    "write",
    "refresh",
    "reload",
    "check",
    "read_loaded_version_end",
  ]);
  assert.deepEqual(plannedSteps({ source: { kind: "official" }, original: "save" }, false), [
    "read_loaded_version",
    "save_original",
    "download",
    "build",
    "unload",
    "write",
    "refresh",
    "reload",
    "check",
    "read_loaded_version_end",
  ]);
  const upload = plannedSteps({ source: { kind: "upload" }, original: "already_saved" }, true);
  assert.ok(upload.includes("build"));
  assert.equal(upload.includes("download"), false);
  assert.equal(upload.includes("save_original"), false);
  assert.deepEqual(upload.slice(-6), [
    "restore_unload",
    "restore_write",
    "restore_refresh",
    "restore_reload",
    "restore_check",
    "read_loaded_version_end",
  ]);
  const corrected = plannedSteps({ source: { kind: "local" }, original: "skip" }, true, { update: true, restore: true });
  assert.ok(corrected.indexOf("align_modules") > corrected.indexOf("reload"));
  assert.ok(corrected.indexOf("align_modules") < corrected.indexOf("check"));
  assert.ok(corrected.indexOf("restore_align_modules") > corrected.indexOf("restore_reload"));
  assert.ok(corrected.indexOf("restore_align_modules") < corrected.indexOf("restore_check"));
  const quiet = plannedSteps({ source: { kind: "official" }, original: "save" }, false);
  assert.equal(quiet.includes("restore_write"), false);
});

function walk(node: FakeElement): FakeElement[] {
  const out: FakeElement[] = [];
  for (const child of node.children) {
    if (child instanceof FakeElement) out.push(child, ...walk(child));
  }
  return out;
}

function textOf(node: FakeElement | FakeText): string {
  if (node instanceof FakeText) return node.textContent;
  return node.children.map(textOf).join("");
}

function interruptedHome(phase: string | null, kind: "official" | "upload" | "local"): HomeResponse {
  const task: HomeTaskRecord = {
    task_id: "20261004T172811Z-69618e40",
    status: "INTERRUPTED",
    phase,
    created_at: 1791134891390,
    finished_at: null,
    interrupt_discovered_at: 1791135491390,
    source:
      kind === "upload"
        ? { kind, fingerprint: "0123456789ab" }
        : kind === "local"
          ? { kind, row_kind: "official" }
          : { kind },
    version: "595.104.02",
  };
  return {
    loaded_version: { value: "580.0" },
    canonical: { version: { value: "580.0" }, sha256: { value: "aa" } },
    truenas_version: { value: "25.04" },
    kernel: { value: "6.12" },
    link: { exists: true, points_to_canonical: true },
    original_saved: { saved: false },
    gpus: { value: { devices: [] } },
    latest_task: task,
    interrupted: task,
    active_task: null,
    update_blocked: [],
  };
}

test("interrupt details link to local drivers without claiming the file is there", async () => {
  installDom();
  const { renderHome } = await import("../src/views/home.js");
  const cases: Array<{ phase: string | null; kind: "official" | "upload" | "local"; notice: RegExp }> = [
    { phase: "download", kind: "official", notice: /未更换|was not replaced/ },
    { phase: "write", kind: "official", notice: /无法判断驱动文件是否已更换|Whether the driver file was replaced cannot be determined/ },
    { phase: "write", kind: "local", notice: /无法判断驱动文件是否已更换|Whether the driver file was replaced cannot be determined/ },
    { phase: null, kind: "upload", notice: /停在哪一步未记录|was not recorded/ },
    {
      phase: "write",
      kind: "official",
      notice: /驱动模块可能已卸载且未重新加载|Driver modules may be unloaded without having been reloaded/,
    },
    {
      phase: "restore_reload",
      kind: "local",
      notice: /驱动模块可能已卸载且未重新加载|Driver modules may be unloaded without having been reloaded/,
    },
    { phase: "align_modules", kind: "official", notice: /驱动文件已写入|The driver file was written/ },
    { phase: "restore_align_modules", kind: "local", notice: /驱动文件已写入|The driver file was written/ },
    { phase: "unload", kind: "official", notice: /请重新选择版本更新|Choose a version and update again/ },
  ];
  for (const lang of ["zh", "en"] as const) {
    for (const item of cases) {
      const container = domShim().createElement("main") as unknown as HTMLElement;
      renderHome(
        {
          lang,
          homeEpoch: 1,
          signedIn: true,
          passwordChanged: false,
          loadHome: async () => interruptedHome(item.phase, item.kind),
          reportTaskState() {},
        } as never,
        container,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
      const root = container as unknown as FakeElement;
      const details = walk(root).find((node) => node.className === "interrupt-detail");
      assert.ok(details, `${item.kind}/${item.phase}/${lang}`);
      const link = walk(details).find((node) => node.tagName === "A");
      assert.equal(link?.getAttribute("href"), "#/drivers");
      assert.equal(link?.textContent, MESSAGES["nav.drivers"][lang]);
      const detailText = textOf(details);
      assert.equal(detailText.includes("构建好的驱动可能已经"), false);
      assert.equal(detailText.includes("may already"), false);
      assert.equal(/built driver/i.test(detailText), false);
      assert.match(detailText, lang === "zh" ? /查看已保存的驱动/ : /See saved drivers/);
      assert.match(textOf(root), item.notice);
    }
  }

  const done = domShim().createElement("main") as unknown as HTMLElement;
  const succeeded = interruptedHome("read_loaded_version_end", "official");
  succeeded.interrupted = null;
  if (succeeded.latest_task && "status" in succeeded.latest_task) {
    succeeded.latest_task.status = "SUCCEEDED";
  }
  renderHome(
    {
      lang: "zh",
      homeEpoch: 1,
      signedIn: true,
      passwordChanged: false,
      loadHome: async () => succeeded,
      reportTaskState() {},
    } as never,
    done,
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  const doneRoot = done as unknown as FakeElement;
  assert.equal(walk(doneRoot).some((node) => node.className === "interrupt-detail"), false);
  assert.equal(walk(doneRoot).some((node) => node.getAttribute("href") === "#/drivers"), false);
  assert.match(textOf(doneRoot), /SUCCEEDED|成功/);
});
