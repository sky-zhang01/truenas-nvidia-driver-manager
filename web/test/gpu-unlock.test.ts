import { test } from "node:test";
import assert from "node:assert/strict";

import type { CheckRecord, GpuReading, HomeResponse, LoadOrderRecord } from "../src/api.js";
import { MESSAGES } from "../src/messages.js";
import { checkFailureItems, checkItems, normalizeReason } from "../src/rules.js";
import { interruptUnlockNote, plannedSteps } from "../src/task-steps.js";
import { installDom, domShim, FakeElement, FakeText } from "./dom-shim.js";

function walk(node: FakeElement): FakeElement[] {
  const out: FakeElement[] = [];
  for (const child of node.children) {
    if (child instanceof FakeElement) out.push(child, ...walk(child));
  }
  return out;
}

function textOf(node: FakeElement | FakeText): string {
  if (node instanceof FakeText) return node.textContent;
  return node.children.map(textOf).join("\n");
}

test("an update with the unlock plans the firmware before the build and the test load before the unload", () => {
  const steps = (kind: string, unlock: boolean): string[] =>
    plannedSteps({ source: { kind }, original: "skip", unlock }, false).slice(0, 6);
  assert.deepEqual(steps("official", true), ["read_loaded_version", "download", "unlock_firmware", "build", "unlock_preload", "unload"]);
  assert.deepEqual(steps("upload", true), ["read_loaded_version", "unlock_firmware", "build", "unlock_preload", "unload", "write"]);
  // A local row is not built: only the test load.
  assert.deepEqual(steps("local", true), ["read_loaded_version", "unlock_preload", "unload", "write", "refresh", "reload"]);
  assert.deepEqual(steps("official", false), ["read_loaded_version", "download", "build", "unload", "write", "refresh"]);
  for (const step of ["unlock_firmware", "unlock_preload"]) {
    assert.ok(MESSAGES[`phase.${step}`], step);
  }
});

test("the interruption note about an idle unlock module follows the phase of an update with the unlock", () => {
  for (const phase of ["read_loaded_version", "save_original", "download", "unlock_firmware", "build"]) {
    assert.equal(interruptUnlockNote(phase, true), false, phase);
  }
  for (const phase of ["unlock_preload", "unload", "write", "reload", "check", "restore_write", null, ""]) {
    assert.equal(interruptUnlockNote(phase, true), true, String(phase));
  }
  assert.equal(interruptUnlockNote("write", false), false);
});

function check(order: LoadOrderRecord | undefined): CheckRecord {
  return {
    pass: "update",
    since_before: { value: 1 },
    since_after: { value: 2 },
    refresh_ok: true,
    nvidia_smi: { value: "610.43.03" },
    proc_version: { value: "610.43.03" },
    expected: "610.43.03",
    version_ok: true,
    passed: order?.vfio_pci_first ?? true,
    ...(order ? { load_order: order } : {}),
  };
}

test("the passthrough item joins the check items only for an update with the unlock rule", () => {
  const order = (first: boolean, readable = true): LoadOrderRecord => ({
    show_depends: readable ? { value: "insmod vfio-pci.ko" } : { unknown: { key: "reason.timed_out", params: {} } },
    nvidia_conf: { value: false },
    vfio_pci_first: first,
    gpu_unlock_before_nvidia: false,
  });
  assert.equal(checkItems(check(undefined)).length, 2);
  const passing = checkItems(check(order(true)));
  assert.equal(passing[2]?.check_item.key, "check.item.vfio_pci_first");
  assert.equal(passing[2]?.observed.key, "check.observed.load_order.first");
  assert.deepEqual(checkFailureItems(check(order(true))), []);
  const failing = checkFailureItems(check(order(false)));
  assert.deepEqual(failing.map((p) => p.observed.key), ["check.observed.load_order.not_first"]);
  assert.equal(checkFailureItems(check(order(false, false)))[0]?.observed.key, "check.observed.status_unreadable");
});

test("the unlock build reasons have their own text", () => {
  for (const code of [
    "gpu_unlock_installer_unusable",
    "gpu_unlock_firmware_failed",
    "gpu_unlock_module_build_failed",
    "gpu_unlock_install_failed",
    "gpu_unlock_module_rejected",
    "artifact.gpu_unlock_module",
    "artifact.gpu_unlock_files",
    "artifact.gpu_unlock_unexpected",
  ]) {
    const key = normalizeReason(code).key;
    assert.equal(key, `build.${code}`);
    assert.ok(MESSAGES[key], key);
  }
  assert.equal(MESSAGES["build.gpu_unlock_installer_unusable"]?.zh, "这个文件不能用来开启解锁");
});

test("the load-order log line reads both positions", async () => {
  installDom();
  const { logParams } = await import("../src/views/tasks.js");
  const params = logParams("log.unlock_load_order", { vfio_pci_first: true, gpu_unlock_before_nvidia: false });
  assert.deepEqual(params["vfio"], { key: "log.load_order.first", params: {} });
  assert.deepEqual(params["unlock"], { key: "log.load_order.not_first", params: {} });
  assert.equal(logParams("log.unlock_load_order", {})["vfio"], "UNKNOWN");
});

const gpu = (address: string, name: string, unlock?: "stable" | "experimental"): GpuReading => ({
  address, device: "2504", name, modules: "either", too_new: [], too_old: [],
  ...(unlock ? { gpu_unlock: unlock } : {}),
});

test("the unlock choice shows the support range by series once, with no card model, and is off without a supported GPU", async () => {
  installDom();
  const { unlockChoiceNode } = await import("../src/gpu-unlock.js");
  const choice = { on: true };
  const node = unlockChoiceNode("zh", [gpu("0000:01:00.0", "RTX 3060", "stable"), gpu("0000:02:00.0", "RTX 4090", "experimental"), gpu("0000:03:00.0", "RTX 5090")], choice) as unknown as FakeElement;
  const box = walk(node).find((n) => n.tagName === "INPUT") as unknown as { disabled: boolean; checked: boolean };
  assert.equal(box.disabled, false);
  assert.equal(box.checked, true);
  const text = textOf(node);
  const range = MESSAGES["update.gpu_unlock.range"]?.zh as string;
  assert.equal(text.split(range).length - 1, 1);
  const order = ["RTX 30", "RTX 40", "RTX 20", "RTX 50"].map((s) => range.indexOf(s));
  assert.deepEqual([...order].sort((x, y) => x - y), order);
  assert.ok(order.every((i) => i >= 0));
  for (const model of ["3060", "4090", "5090"]) assert.equal(text.includes(model), false, model);
  assert.equal(/0000:0/.test(text), false);

  const none = { on: true };
  const off = unlockChoiceNode("en", [gpu("0000:03:00.0", "RTX 5090")], none) as unknown as FakeElement;
  const offBox = walk(off).find((n) => n.tagName === "INPUT") as unknown as { disabled: boolean; checked: boolean };
  assert.equal(offBox.disabled, true);
  assert.equal(none.on, false);
  assert.match(textOf(off), /None of the selected GPUs supports the unlock\./);
  assert.match(textOf(off), /Unlock support: the RTX 30 series is stable, the RTX 40 and RTX 20 series are experimental, and the RTX 50 series is not supported\./);
});

test("a release on the support table is marked and the unlock box can be checked", async () => {
  installDom();
  const { unlockChoiceNode, versionOptionText } = await import("../src/gpu-unlock.js");
  const table = ["610.43.03"];
  assert.match(versionOptionText("610.43.03", table, "zh"), /610\.43\.03 · 支持解锁/);
  assert.match(versionOptionText("610.43.03", table, "en"), /610\.43\.03 · supports unlock/);
  assert.equal(versionOptionText("610.57.04", table, "zh"), "610.57.04");
  const choice = { on: false };
  const node = unlockChoiceNode("zh", [gpu("0000:01:00.0", "RTX 3060", "stable")], choice, "", undefined, table, "610.43.03") as unknown as FakeElement;
  const box = walk(node).find((n) => n.tagName === "INPUT") as unknown as { disabled: boolean; checked: boolean };
  assert.equal(box.disabled, false);
  assert.equal(choice.on, false);
});

function recordClicks(): Map<FakeElement, Map<string, Array<() => void>>> {
  installDom();
  const recorded = new Map<FakeElement, Map<string, Array<() => void>>>();
  const doc = domShim();
  const create = doc.createElement.bind(doc);
  doc.createElement = (tagName: string): FakeElement => {
    const node = create(tagName);
    node.addEventListener = ((type: string, listener: () => void): void => {
      const byType = recorded.get(node) ?? new Map();
      const list = byType.get(type) ?? [];
      list.push(listener);
      byType.set(type, list);
      recorded.set(node, byType);
    }) as FakeElement["addEventListener"];
    return node;
  };
  return recorded;
}

test("checking unlock for an uploaded supported release stays on after the page redraws an unsupported release", async () => {
  const recorded = recordClicks();
  const { publishUploadUnlock, unlockChoiceNode } = await import("../src/gpu-unlock.js");
  const table = ["610.43.03"];
  const gpus = [gpu("0000:01:00.0", "RTX 3060", "stable")];
  const page = { on: false };
  const upload = { on: false };
  let redraws = 0;
  const node = unlockChoiceNode("zh", gpus, upload, "upload-", () => {
    publishUploadUnlock(upload, page, () => {
      redraws += 1;
      unlockChoiceNode("zh", gpus, page, "", undefined, table, "610.57.04");
    });
  }, table, "610.43.03") as unknown as FakeElement;
  const box = walk(node).find((n) => n.tagName === "INPUT") as FakeElement & { checked: boolean; disabled: boolean };
  assert.equal(box.disabled, false);
  box.checked = true;
  for (const listener of recorded.get(box)?.get("change") ?? []) listener();
  assert.equal(redraws, 1);
  assert.equal(box.checked, true);
  assert.equal(upload.on, true);
  assert.equal(page.on, false);
});

test("a release outside the support table turns the unlock box off and says why", async () => {
  installDom();
  const { unlockChoiceNode } = await import("../src/gpu-unlock.js");
  const table = ["610.43.03"];
  const choice = { on: true };
  const node = unlockChoiceNode("zh", [gpu("0000:01:00.0", "RTX 3060", "stable")], choice, "", undefined, table, "610.57.04") as unknown as FakeElement;
  const box = walk(node).find((n) => n.tagName === "INPUT") as unknown as { disabled: boolean; checked: boolean };
  assert.equal(box.disabled, true);
  assert.equal(choice.on, false);
  assert.match(textOf(node), /这个版本暂不支持解锁。想解锁，请选标有「支持解锁」的版本。/);
  const english = unlockChoiceNode("en", [gpu("0000:01:00.0", "RTX 3060", "stable")], { on: true }, "", undefined, table, "610.57.04") as unknown as FakeElement;
  assert.match(textOf(english), /This version does not support the unlock yet\. To unlock, choose a version marked "supports unlock"\./);
});

function home(unlock: HomeResponse["gpu_unlock"]): HomeResponse {
  return {
    loaded_version: { value: "610.43.03" },
    canonical: { version: { value: "610.43.03" }, sha256: { value: "aa" } },
    truenas_version: { value: "25.10.7" },
    kernel: { value: "6.12" },
    link: { exists: true, points_to_canonical: true },
    original_saved: { saved: false },
    gpus: { value: { devices: [{ address: "0000:01:00.0", device: "2504", name: "RTX 3060", passthrough: false }] } },
    gpu_unlock: unlock,
    latest_task: null,
    interrupted: null,
    active_task: null,
    update_blocked: [],
  } as HomeResponse;
}

test("the home reading names each target by model and keeps the kernel line of a card that was not unlocked", async () => {
  installDom();
  const { unlockReadingNode } = await import("../src/gpu-unlock.js");
  assert.equal(unlockReadingNode(home(null), "zh"), null);
  const reading = unlockReadingNode(home({
    value: {
      cards: [
        { address: "0000:01:00.0", result: "unlocked" },
        { address: "0000:09:00.0", result: "not_unlocked", line: "gpu_unlock: no supported card at \"0000:09:00.0\"" },
        { address: "0000:0a:00.0", result: "not_unlocked" },
      ],
      reloaded_after_boot: true,
    },
  }), "zh") as unknown as FakeElement;
  const text = textOf(reading);
  assert.match(text, /显卡全速解锁/);
  assert.match(text, /RTX 3060 已解锁/);
  // A target that is no GPU of this host keeps its address.
  assert.match(text, /0000:09:00\.0 没有解锁。系统日志原文：gpu_unlock: no supported card/);
  assert.match(text, /0000:0a:00\.0 没有解锁。这次开机时，解锁没有运行。/);
  assert.match(text, /这次开机之后，驱动又加载过一次。解锁状态要以下次开机为准。/);
  // An unreadable reading keeps UNKNOWN and says which query failed and why.
  const unknown = unlockReadingNode(home({ unknown: { key: "reason.timed_out", params: { seconds: 5 } } }), "en") as unknown as FakeElement;
  assert.match(textOf(unknown), /UNKNOWN/);
  assert.match(unknown.textContent, /Full-speed GPU unlock status query failed: it did not finish within 5 seconds\./);
});
