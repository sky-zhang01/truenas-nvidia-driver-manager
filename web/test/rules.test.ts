import { test } from "node:test";
import assert from "node:assert/strict";

import {
  checkFailureItems,
  checkItems,
  checkReasonText,
  compareRows,
  compareVersion,
  conflict,
  fileTooLarge,
  UPLOAD_LIMIT_BYTES,
  filterVersions,
  gpuLabel,
  majorOf,
  noOriginalReasonParam,
  normalizeReason,
  restoreFailureReason,
  splitRawOutput,
  taskNoticeParams,
  toolkitBelowFloor,
  toolkitTooOld,
  toolkitUpstream,
  versionFormatOk,
  type SortRow,
} from "../src/rules.js";
import type { CheckRecord, GpuReading, Modules, TaskDetail, Unreadable } from "../src/api.js";
import { expandText, type Params } from "../src/dom.js";

const ITEMS = ["615.71.09", "610.57.04", "595.104.02", "590.48.01", "580.178.04", "575.64.05", "570.211.01", "565.77", "550.163.01"];
const PATCHED_ITEMS = [...ITEMS, "470.256.02"];
const OPEN_UNBUILDABLE = ["575.64.05", "570.211.01", "565.77", "550.163.01"];
const OPEN_OPTIONS = ["615.71.09", "610.57.04", "595.104.02", "590.48.01", "580.178.04"];
const CLOSED_OPTIONS = ["580.178.04", "575.64.05", "570.211.01", "565.77", "550.163.01"];
const EITHER_OPTIONS = [...OPEN_OPTIONS];

interface Known {
  device: string;
  name?: string;
  modules: Modules;
  last_branch?: number;
  first?: string;
  usable_from_branch?: number;
}

// Simulated backend responses for ITEMS. No support-bound algorithm is duplicated here.
const TABLE: Record<string, Known> = {
  "1b80": { device: "1b80", name: "NVIDIA GeForce GTX 1080", modules: "closed_only", last_branch: 580, first: "470.42.01", usable_from_branch: 550 },
  "1180": { device: "1180", name: "GeForce GTX 680", modules: "closed_only", last_branch: 470, first: "470.42.01" },
  "06c0": { device: "06c0", modules: "closed_only", last_branch: 390 },
  "0fc0": { device: "0fc0", name: "NVIDIA GeForce GT 640", modules: "closed_only", last_branch: 470, first: "470.42.01" },
  "2b85": { device: "2b85", name: "NVIDIA GeForce RTX 5090", modules: "open_only", first: "570.86.16", usable_from_branch: 580 },
  "2c33": { device: "2c33", name: "NVIDIA RTX PRO 4000 Blackwell SFF Edition", modules: "open_only", first: "570.190", usable_from_branch: 580 },
  "2684": { device: "2684", name: "NVIDIA GeForce RTX 4090", modules: "either", first: "520.56.06", usable_from_branch: 580 },
  "1e02": { device: "1e02", modules: "either", first: "470.42.01", usable_from_branch: 580 },
};

// Devices outside the table, by device-ID band.
const OUTSIDE: Record<string, Known> = {
  "101f": { device: "101f", modules: "closed_only", last_branch: 470 },
  "1b07": { device: "1b07", modules: "closed_only", last_branch: 580, usable_from_branch: 550 },
  "1e37": { device: "1e37", modules: "either", first: "470.42.01", usable_from_branch: 580 },
  "2bb2": { device: "2bb2", modules: "open_only", first: "570.86.16", usable_from_branch: 580 },
  "2c2c": { device: "2c2c", modules: "open_only", first: "570.86.16", usable_from_branch: 580 },
  "2c09": { device: "2c09", modules: "open_only", first: "615.71.09", usable_from_branch: 615 },
  "3001": { device: "3001", modules: "open_only", first: "615.71.09", usable_from_branch: 615 },
};

function gpu(device: string, address = `0000:0a:00.0`, items = ITEMS): GpuReading {
  const known = TABLE[device] ?? OUTSIDE[device];
  if (!known) throw new Error(`unknown test device ${device}`);
  const too_new = known.last_branch === undefined
    ? []
    : items.filter((v) => (majorOf(v) ?? 0) > known.last_branch!);
  const too_old = known.first === undefined
    ? []
    : items.filter((v) => compareVersion(v, known.first as string) < 0);
  const g: GpuReading = {
    address,
    device: known.device,
    modules: known.modules,
    too_new,
    too_old,
  };
  if (known.name !== undefined) g.name = known.name;
  if (known.last_branch !== undefined) g.last_branch = known.last_branch;
  if (known.first !== undefined) g.first_branch = majorOf(known.first) ?? undefined;
  const patchedLowerBounds: Record<string, number> = { "1b80": 470, "1180": 470, "0fc0": 470, "101f": 470, "1b07": 470, "1e02": 470, "1e37": 470 };
  const usable = items === PATCHED_ITEMS ? patchedLowerBounds[device] ?? known.usable_from_branch : known.usable_from_branch;
  if (usable !== undefined) Object.assign(g, { usable_from_branch: usable });
  return g;
}

function run(devices: string[], selected: string[], unreadable: Unreadable[] = [], items = ITEMS) {
  const gpus = devices.map((d, i) => gpu(d, `0000:0${i + 1}:00.0`, items));
  const picked = selected.map((d) => gpus[devices.indexOf(d)]?.address ?? "");
  return filterVersions(items, OPEN_UNBUILDABLE, unreadable, gpus, picked);
}

// Explicit API responses: these lower bounds come from the backend, not a frontend calculation.
test("C14 open support uses the host usable bound instead of first support", () => {
  const card = { ...gpu("2b85"), usable_from_branch: 580 };
  const result = filterVersions(ITEMS, OPEN_UNBUILDABLE, [], [card], [card.address]);
  assert.deepEqual(result.options, OPEN_OPTIONS);
  assert.deepEqual(result.notes, [{ gpu: card, kind: "from", branch: 580 }]);
});

test("C14 closed support uses its actual lower bound without open exclusions", () => {
  const versions = ["580.178.04", "565.77", "550.163.01"];
  const card = { ...gpu("1b80"), first_branch: 550, usable_from_branch: 565, too_old: ["550.163.01"], too_new: [] };
  const result = filterVersions(versions, ["565.77"], [], [card], [card.address]);
  assert.deepEqual(result.options, ["580.178.04", "565.77"]);
  assert.deepEqual(result.notes, [{ gpu: card, kind: "from", branch: 565 }]);
});

test("C14 missing usable readings never infer a from note", () => {
  const { usable_from_branch: _omitted, ...card } = { ...gpu("2b85"), usable_from_branch: 580 };
  const result = filterVersions(ITEMS, OPEN_UNBUILDABLE, [], [card], [card.address]);
  assert.deepEqual(result.options, OPEN_OPTIONS);
  assert.deepEqual(result.notes, []);
});

test("C14 lower-bound notes require an actual lower catalog exclusion", () => {
  const card = { ...gpu("2b85"), usable_from_branch: 580, too_old: ["550.163.01"] };
  const result = filterVersions(OPEN_OPTIONS, OPEN_UNBUILDABLE, [], [card], [card.address]);
  assert.deepEqual(result.options, OPEN_OPTIONS);
  assert.deepEqual(result.notes, []);
  const sameBranch = { ...card, too_old: [] };
  assert.deepEqual(filterVersions(["580.126.09", "580.119.02"], ["580.119.02"], [], [sameBranch], [sameBranch.address]).notes,
    [{ gpu: sameBranch, kind: "from", branch: 580 }]);
  const upperExcluded = { ...card, too_old: [], too_new: ["615.71.09"], last_branch: 610 };
  assert.deepEqual(filterVersions(OPEN_OPTIONS, ["615.71.09"], [], [upperExcluded], [upperExcluded.address]).notes,
    [{ gpu: upperExcluded, kind: "last", branch: 610 }]);
});

test("C14 open exclusions include versions below 555", () => {
  const versions = ["555.42.02", "550.163.01"];
  for (const modules of ["open_only", "either"] as Modules[]) {
    const card = { ...gpu("2684"), modules, too_old: [], too_new: [] };
    assert.deepEqual(filterVersions(versions, versions, [], [card], [card.address]).options, []);
  }
});

test("C14 multi-card notes retain individual bounds, last notes, conflict and selection behavior", () => {
  const open = { ...gpu("2b85", "0000:02:00.0"), usable_from_branch: 580 };
  const closed = { ...gpu("1b80", "0000:01:00.0"), usable_from_branch: 550 };
  const either = { ...gpu("2684", "0000:03:00.0"), usable_from_branch: 580 };
  const multi = filterVersions(ITEMS, OPEN_UNBUILDABLE, [], [either, open], [either.address, open.address]);
  assert.deepEqual(multi.options, OPEN_OPTIONS);
  assert.deepEqual(multi.notes, [{ gpu: open, kind: "from", branch: 580 }, { gpu: either, kind: "from", branch: 580 }]);
  const clash = filterVersions(ITEMS, OPEN_UNBUILDABLE, [], [open, closed], [open.address, closed.address]);
  assert.deepEqual(clash.options, []);
  assert.equal(clash.conflict, true);
  assert.equal(clash.noUsableVersion, true);
  assert.deepEqual(clash.notes, [{ gpu: closed, kind: "last", branch: 580 }, { gpu: open, kind: "from", branch: 580 }]);
  const unselected = filterVersions(ITEMS, OPEN_UNBUILDABLE, [], [open, closed], []);
  assert.deepEqual(unselected, { options: [], notes: [], noUsableVersion: false, noneSelected: true, conflict: false });
});

test("version format and ordering", () => {
  assert.equal(versionFormatOk("580.173.02"), true);
  assert.equal(versionFormatOk("580"), false);
  assert.equal(versionFormatOk("580."), false);
  assert.equal(versionFormatOk("580.173.02.01.5"), false);
  assert.equal(versionFormatOk("580.173.02-beta"), false);
  assert.equal(compareVersion("580.95.05", "580.173.02"), -1);
  assert.equal(compareVersion("580.173.02", "580.95.05"), 1);
  assert.equal(compareVersion("580.173.02", "580.173.02"), 0);
  assert.equal(compareVersion("470.42.02", "470.42.01"), 1);
  assert.equal(compareVersion("580.178", "580.178.0"), -1);
  assert.equal(compareVersion("580.178.0", "580.178"), 1);
  assert.equal(compareVersion("570.86.16", "570.86.16.0"), -1);
  assert.equal(compareVersion("580.9", "580.10"), -1);
  assert.equal(majorOf("615.71.09"), 615);
  assert.equal(majorOf("nope"), null);
});

test("toolkit floor and the 615 coupling", () => {
  assert.equal(toolkitUpstream("1.20.1-1"), "1.20.1");
  assert.equal(toolkitUpstream("1.19.0"), "1.19.0");
  assert.equal(toolkitBelowFloor("1.19.0-1"), true);
  assert.equal(toolkitBelowFloor("1.19.1-1"), false);
  assert.equal(toolkitBelowFloor("1.19.2-1"), false);
  assert.equal(toolkitTooOld("614.99.99", "1.19.0"), false);
  assert.equal(toolkitTooOld("615.71.09", "1.19.1"), true);
  assert.equal(toolkitTooOld("615.71.09", "1.20.0"), true);
  assert.equal(toolkitTooOld("615.71.09", "1.20"), true);
  assert.equal(toolkitTooOld("615.71.09", "1.20.1-1"), false);
  assert.equal(toolkitTooOld("615.71.09", "1.20.1.0-1"), false);
  assert.equal(toolkitTooOld("616.1.1", "1.21.0"), false);
  assert.equal(toolkitTooOld("610.57.04", "1.19.1"), false);
  assert.equal(toolkitTooOld("615.71.09", null), false);
});

test("the 1 GiB upload limit", () => {
  // The limit is 1 GiB (src/codes.rs `UPLOAD_FILE_LIMIT`), not 1,000,000,000 bytes.
  assert.equal(UPLOAD_LIMIT_BYTES, 1024 * 1024 * 1024);
  assert.equal(fileTooLarge(1_000_000_001), false);
  assert.equal(fileTooLarge(1_073_741_824), false);
  assert.equal(fileTooLarge(1_073_741_825), true);
  assert.equal(fileTooLarge(2_000_000_000), true);
});

test("row sorting: UNKNOWN sorts last in both directions", () => {
  const a: SortRow = { version: "580.95.05", source: "official", date: 1, size: 10, kernel: "6.12.105", status: "in_use", keep: true };
  const b: SortRow = { version: "580.173.02", source: "official", date: 2, size: 20, kernel: "6.12.105", status: "available", keep: false };
  const u: SortRow = { version: null, source: null, date: null, size: null, kernel: null, status: "UNKNOWN", keep: null };
  assert.ok(compareRows(a, b, "version", "asc") < 0);
  assert.ok(compareRows(a, b, "version", "desc") > 0);
  const short: SortRow = { ...a, version: "580.178" };
  const long: SortRow = { ...b, version: "580.178.0" };
  assert.ok(compareRows(short, long, "version", "asc") < 0);
  assert.ok(compareRows(short, long, "version", "desc") > 0);
  assert.ok(compareRows(a, u, "version", "asc") < 0);
  assert.ok(compareRows(a, u, "version", "desc") < 0);
  assert.ok(compareRows(u, b, "size", "desc") > 0);
  assert.ok(compareRows(a, b, "keep", "asc") > 0);
});

test("no counted GPUs lists everything", () => {
  const res = filterVersions(ITEMS, OPEN_UNBUILDABLE, [], [], []);
  assert.deepEqual(res.options, ITEMS);
  assert.deepEqual(res.notes, []);
  assert.equal(res.noUsableVersion, false);
});

test("one closed-only GPU is auto-selected and excludes newer branches", () => {
  const res = run(["1b80"], ["1b80"]);
  assert.deepEqual(res.options, CLOSED_OPTIONS);
  assert.equal(res.notes.length, 1);
  assert.equal(res.notes[0]?.kind, "last");
  assert.equal(res.notes[0]?.branch, 580);
  assert.equal(res.noUsableVersion, false);
});

test("Kepler (1180) and 0fc0 have no usable version", () => {
  for (const device of ["1180", "0fc0"]) {
    const res = run([device], [device]);
    assert.deepEqual(res.options, []);
    assert.equal(res.notes[0]?.kind, "last");
    assert.equal(res.notes[0]?.branch, 470);
    assert.equal(res.noUsableVersion, true);
  }
  const res390 = run(["06c0"], ["06c0"]);
  assert.equal(res390.notes[0]?.branch, 390);
  assert.equal(res390.noUsableVersion, true);
});

test("a closed-only and an open-only GPU together list nothing", () => {
  const res = run(["1b80", "2b85"], ["1b80", "2b85"]);
  assert.deepEqual(res.options, []);
  assert.equal(res.conflict, true);
  assert.equal(res.noUsableVersion, true);
  assert.equal(res.notes.length, 2);
  assert.equal(res.notes[0]?.branch, 580);
  assert.equal(res.notes[1]?.branch, 580);
});

test("devices outside the table use their band", () => {
  const k20 = run(["101f"], ["101f"]);
  assert.deepEqual(k20.options, []);
  assert.equal(k20.notes[0]?.branch, 470);
  assert.equal(k20.noUsableVersion, true);

  const p102 = run(["1b07"], ["1b07"]);
  assert.deepEqual(p102.options, CLOSED_OPTIONS);
  assert.equal(p102.notes[0]?.branch, 580);

  const t10 = run(["1e37"], ["1e37"]);
  assert.deepEqual(t10.options, EITHER_OPTIONS);
  assert.equal(t10.notes[0]?.branch, 580);
  assert.deepEqual(t10.notes.map(({ kind, branch }) => ({ kind, branch })), [{ kind: "from", branch: 580 }]);

  const engineering = run(["2c2c"], ["2c2c"]);
  assert.deepEqual(engineering.options, OPEN_OPTIONS);
  assert.equal(engineering.notes[0]?.kind, "from");
  assert.equal(engineering.notes[0]?.branch, 580);

  const unknownPrefix = run(["3001"], ["3001"]);
  assert.deepEqual(unknownPrefix.options, ["615.71.09"]);
  assert.equal(unknownPrefix.notes[0]?.kind, "from");
  assert.equal(unknownPrefix.notes[0]?.branch, 615);

  const bb2 = run(["2bb2"], ["2bb2"]);
  assert.equal(bb2.notes[0]?.branch, 580);
});

test("two GPUs selected: one line per excluding GPU, by address", () => {
  const res = run(["1b07", "1b80"], ["1b07", "1b80"]);
  assert.deepEqual(res.options, CLOSED_OPTIONS);
  assert.equal(res.notes.length, 2);
  assert.equal(res.notes[0]?.kind, "last");
  assert.equal(res.notes[1]?.kind, "last");
  assert.equal(
    gpuLabel(res.notes[0]!.gpu),
    "10de:1b07",
  );
});

test("two or more counted GPUs and none selected lists nothing", () => {
  const res = run(["1b80", "2b85"], []);
  assert.deepEqual(res.options, []);
  assert.equal(res.noneSelected, true);
  assert.equal(res.noUsableVersion, false);
  assert.deepEqual(res.notes, []);
});

test("module compatibility uses the whole selected group and preserves support notes", () => {
  for (const [devices, selected, options, notes] of [
    [["2684"], ["2684"], EITHER_OPTIONS, ["from:580"]],
    [["2b85"], ["2b85"], OPEN_OPTIONS, ["from:580"]],
    [["2c33"], ["2c33"], OPEN_OPTIONS, ["from:580"]],
    [["1b80", "2684"], ["1b80", "2684"], CLOSED_OPTIONS, ["last:580"]],
    [["1b07", "2684"], ["1b07", "2684"], CLOSED_OPTIONS, ["last:580"]],
    [["2684", "2b85"], ["2684", "2b85"], OPEN_OPTIONS, ["from:580", "from:580"]],
    [["2b85", "1e37"], ["2b85", "1e37"], OPEN_OPTIONS, ["from:580", "from:580"]],
    [["2b85", "2c2c"], ["2b85", "2c2c"], OPEN_OPTIONS, ["from:580", "from:580"]],
    [["2b85", "2c33"], ["2b85", "2c33"], OPEN_OPTIONS, ["from:580", "from:580"]],
    [["1b80", "2b85"], ["1b80"], CLOSED_OPTIONS, ["last:580"]],
    [["1b80", "2b85"], ["2b85"], OPEN_OPTIONS, ["from:580"]],
    [["1b80", "2684"], ["2684"], EITHER_OPTIONS, ["from:580"]],
    [["1b07", "2c2c"], ["1b07", "2c2c"], [], ["last:580", "from:580"]],
    [["1b80", "2c2c"], ["1b80", "2c2c"], [], ["last:580", "from:580"]],
    [["1b07", "2b85"], ["1b07", "2b85"], [], ["last:580", "from:580"]],
  ] as [string[], string[], string[], string[]][]) {
    const res = run(devices, selected);
    assert.deepEqual(res.options, options, selected.join("+"));
    assert.deepEqual(res.notes.map((n) => `${n.kind}:${n.branch}`), notes);
  }
});

test("qualified 470 GPU inputs retain the exact choices and support notes", () => {
  const r = "470.256.02";
  const h = OPEN_OPTIONS;
  const c = [...CLOSED_OPTIONS, r];
  const e = [...EITHER_OPTIONS, r];
  const rows: [string, string[], string[], string[], string[]][] = [
    ["no counted GPU", [], [], PATCHED_ITEMS, []],
    ["Blackwell", ["2b85"], ["2b85"], h, ["from:580"]],
    ["Ada", ["2684"], ["2684"], EITHER_OPTIONS, ["from:580"]],
    ["early Either", ["1e02"], ["1e02"], e, []],
    ["Pascal", ["1b80"], ["1b80"], c, ["last:580"]],
    ["Kepler", ["1180"], ["1180"], [r], ["last:470"]],
    ["unknown Kepler band", ["101f"], ["101f"], [r], ["last:470"]],
    ["pre-Kepler", ["06c0"], ["06c0"], [], ["last:390"]],
    ["new Open band", ["2c09"], ["2c09"], ["615.71.09"], ["from:615"]],
    ["unknown Pascal band", ["1b07"], ["1b07"], c, ["last:580"]],
    ["unknown Either band", ["1e37"], ["1e37"], e, []],
    ["unknown Open band", ["2c2c"], ["2c2c"], h, ["from:580"]],
    ["newest Open band", ["3001"], ["3001"], ["615.71.09"], ["from:615"]],
    ["Pascal and Ada", ["1b80", "2684"], ["1b80", "2684"], CLOSED_OPTIONS, ["last:580", "from:580"]],
    ["Pascal and Blackwell", ["1b80", "2b85"], ["1b80", "2b85"], [], ["last:580", "from:580"]],
    ["Pascal and unknown Open", ["1b80", "2c2c"], ["1b80", "2c2c"], [], ["last:580", "from:580"]],
    ["Ada and Blackwell", ["2684", "2b85"], ["2684", "2b85"], h, ["from:580", "from:580"]],
    ["two Pascal cards", ["1b07", "1b80"], ["1b07", "1b80"], c, ["last:580", "last:580"]],
    ["unknown Pascal and Ada", ["1b07", "2684"], ["1b07", "2684"], CLOSED_OPTIONS, ["last:580", "from:580"]],
    ["unknown Pascal and Blackwell", ["1b07", "2b85"], ["1b07", "2b85"], [], ["last:580", "from:580"]],
    ["Blackwell and early Either", ["2b85", "1e37"], ["2b85", "1e37"], h, ["from:580"]],
  ];
  for (const [label, devices, selected, expected, notes] of rows) {
    const result = run(devices, selected, [], PATCHED_ITEMS);
    assert.deepEqual(result.options, expected, label);
    assert.deepEqual(result.notes.map((n) => `${n.kind}:${n.branch}`), notes, label);
    assert.equal(result.noUsableVersion, devices.length > 0 && expected.length === 0, label);
  }
  for (const [devices, expected] of [
    [["1180", "1b80"], [r]],
    [["1180", "1e02"], [r]],
    [["1180", "2684"], []],
  ] as [string[], string[]][]) {
    assert.deepEqual(run(devices, devices, [], PATCHED_ITEMS).options, expected, devices.join("+"));
  }
});

test("open exclusions have no frontend branch threshold and do not constrain unspecified module types", () => {
  const versions = ["555.42.02", "550.163.01"];
  const either = { ...gpu("2684"), too_old: [], too_new: [] };
  assert.deepEqual(filterVersions(versions, versions, [], [either], [either.address]).options, []);
  assert.deepEqual(filterVersions(versions, versions, [], [either], []).options, versions);
  assert.deepEqual(filterVersions(versions, versions, [], [], []).options, versions);
  assert.deepEqual(filterVersions(versions, [], [], [either], [either.address]).options, versions);
  const closed = { ...gpu("1b80"), too_old: [], too_new: [] };
  assert.deepEqual(filterVersions(versions, versions, [], [closed], [closed.address]).options, versions);
});

test("the conclusion line needs an empty list and no unreadable object", () => {
  const withUnreadable = run(["1180"], ["1180"], [
    { object: "catalog", reason: { key: "reason.connect_failed", params: {} } },
  ]);
  assert.deepEqual(withUnreadable.options, []);
  assert.equal(withUnreadable.noUsableVersion, false);
  assert.equal(withUnreadable.notes.length, 1);
});

test("conflict looks only at the selected GPUs", () => {
  assert.equal(conflict([gpu("1b80"), gpu("2b85")]), true);
  assert.equal(conflict([gpu("1b80"), gpu("2684")]), false);
  assert.equal(conflict([gpu("2b85"), gpu("2c2c")]), false);
  assert.equal(conflict([gpu("1b07"), gpu("2c2c")]), true);
});

test("labels fall back to the device id outside the table", () => {
  assert.equal(gpuLabel(gpu("1b80")), "NVIDIA GeForce GTX 1080");
  assert.equal(gpuLabel(gpu("1b07")), "10de:1b07");
});

test("reason codes with a colon carry their value", () => {
  assert.deepEqual(normalizeReason("compiler_missing:gcc-12"), {
    key: "build.compiler_missing",
    params: { version: "12" },
  });
  assert.deepEqual(normalizeReason("deb_extract_failed:nvidia-container-toolkit"), {
    key: "build.deb_extract_failed",
    params: { package: "nvidia-container-toolkit" },
  });
  assert.deepEqual(normalizeReason("unexpected:42"), {
    key: "build.unexpected",
    params: { line: "42" },
  });
  assert.deepEqual(normalizeReason("installer_failed"), { key: "build.installer_failed", params: {} });
  assert.deepEqual(normalizeReason("artifact.sha_mismatch"), {
    key: "build.artifact.sha_mismatch",
    params: {},
  });
  assert.deepEqual(normalizeReason("download.http_status", { status: 404 }), {
    key: "download.http_status",
    params: { status: 404 },
  });
  assert.deepEqual(normalizeReason("stage_copy_failed"), { key: "build.capture_failed", params: {} });
  assert.deepEqual(normalizeReason("release_write_failed"), {
    key: "build.release_write_failed",
    params: {},
  });
});

test("community patch and module failures normalize only the fixed suffixes", () => {
  assert.deepEqual(normalizeReason("community_patch_failed:archive"), {
    key: "build.community_patch_failed.archive", params: {},
  });
  for (const patch of Array.from({ length: 54 }, (_, i) => String(i + 1).padStart(2, "0"))) {
    const reason = normalizeReason(`community_patch_failed:${patch}`);
    assert.deepEqual(reason, { key: "build.community_patch_failed", params: { patch } });
    assert.match(expandText(reason.key, reason.params ?? {}, "en"), new RegExp(`patch ${patch} failed`));
  }
  assert.deepEqual(normalizeReason("kernel_module_build_failed"), {
    key: "build.kernel_module_build_failed", params: {},
  });
  for (const [suffix, name] of [
    ["nvidia", "nvidia"],
    ["nvidia_uvm", "nvidia-uvm"],
    ["nvidia_modeset", "nvidia-modeset"],
    ["nvidia_drm", "nvidia-drm"],
    ["nvidia_peermem", "nvidia-peermem"],
  ]) {
    for (const failure of ["kernel_module_missing", "kernel_module_install_failed"]) {
      assert.deepEqual(normalizeReason(`${failure}:${suffix}`), {
        key: `build.${failure}`, params: { module: name },
      });
    }
  }
  for (const code of [
    "community_patch_failed", "community_patch_failed:", "community_patch_failed:00",
    "community_patch_failed:55", "community_patch_failed:1",
    "community_patch_failed:01\n", "community_patch_failed:01\r", "community_patch_failed:01\t",
    "community_patch_failed:01 ", "community_patch_failed: 01",
    "community_patch_failed:../01",
    "kernel_module_missing:", "kernel_module_missing:nvidia-drm",
    "kernel_module_missing:nvidia_other", "kernel_module_install_failed:../nvidia",
    "kernel_module_install_failed:nvidia_uvm\n",
  ]) {
    assert.deepEqual(normalizeReason(code), { key: code, params: {} }, code);
  }
});

test("stderr outside the alert keeps the real signal as its reason", () => {
  const result = splitRawOutput(
    { unload_reason: { stderr: "rmmod: interrupted" } },
    { signal: 15 },
  );
  assert.deepEqual(result.raw, ["rmmod: interrupted"]);
  assert.deepEqual(result.params["unload_reason"], {
    key: "reason.signal",
    params: { signal: 15 },
  });
});

test("a signal reason expands in both interface languages", () => {
  assert.equal(
    expandText("reason.signal", { signal: 15 }, "zh"),
    "命令被信号 15 终止",
  );
  assert.equal(
    expandText("reason.signal", { signal: 15 }, "en"),
    "the command was terminated by signal 15",
  );
});

const FAILING_CHECK: CheckRecord = {
  pass: "update",
  since_before: { value: 1000000 },
  since_after: { value: 1000000 },
  refresh_ok: false,
  nvidia_smi: { value: "580.178.04" },
  proc_version: { value: "580.178.04" },
  expected: "595.104.02",
  version_ok: false,
  passed: false,
};

function detailWith(
  status: string,
  notice: { key: string; variant?: string; params?: Record<string, unknown> },
  error: Record<string, unknown> | null,
): TaskDetail {
  return {
    task_id: "20261003T000000Z-00000000",
    status,
    state_note: null,
    phase: null,
    plan: { version: "595.104.02" },
    loaded_version_start: { value: "580.178.04" },
    loaded_version_end: { value: "580.178.04" },
    download: null,
    replace: [],
    checks: [FAILING_CHECK],
    restore: {
      decision: "restore",
      reason: null,
      read_reason: null,
      original: { row_id: "row", version: "580.178.04", saved_at: 1790985600000 },
    },
    error,
    gpu_users: null,
    version_change: false,
    notice,
    log: { lines: [], next: 0, reset: false },
    unit_query: null,
  } as unknown as TaskDetail;
}

test("success notice values come from the checks and the restore record", () => {
  const detail = detailWith("STOCK_RESTORED", {
    key: "notice.restored",
    params: { target: "595.104.02", original: "580.178.04", date: 1790985600000 },
  }, null);
  const params = taskNoticeParams(detail, "zh");
  assert.equal(String(params["date"]).includes("1790985600000"), false);
  assert.match(String(params["date"]), /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  const reason = String(params["check_reason"]);
  assert.match(reason, /驱动扩展刷新 的观测值为/);
  assert.match(reason, /期望值为 595\.104\.02/);
  assert.equal(reason.includes("UNKNOWN"), false);
  assert.equal(reason.includes("update:"), false);
});

test("two-failures notice and restore-failed notice read the missed deadline without unfilled branches", () => {
  const detail = detailWith(
    "FAILED",
    { key: "notice.restore_failed", variant: "before_write", params: { reload_failed: false } },
    {
      code: "RESTORE_NOT_WRITTEN",
      params: { cause: "deadline", deadline: 10 },
      command: null,
      exit: null,
      raw: null,
    },
  );
  const reason = restoreFailureReason(detail, "zh") as { key: string; params: Params };
  assert.equal(reason.key, "restore.fail.deadline");
  assert.equal(
    expandText(reason.key, reason.params, "zh"),
    "写入原版驱动文件之前已超过执行期限（10 分钟）",
  );
  const params = taskNoticeParams(detail, "zh");
  assert.deepEqual(params["restore_reason"], reason);
  assert.match(String(params["check_reason"]), /驱动扩展刷新 的观测值为/);
  assert.equal(checkReasonText(detail, "restore", "zh"), "UNKNOWN");
});

test("a passing check record still yields both reading sentences", () => {
  const rec: CheckRecord = {
    pass: "update",
    since_before: { value: 1_759_050_930_123_456 },
    since_after: { value: 1_759_050_930_223_456 },
    refresh_ok: true,
    nvidia_smi: { value: "595.104.02" },
    proc_version: { value: "595.104.02" },
    expected: "595.104.02",
    version_ok: true,
    passed: true,
  };
  assert.deepEqual(checkFailureItems(rec), []);
  const items = checkItems(rec);
  assert.equal(items.length, 2);
  const refresh = expandText("check.reason", items[0] as unknown as Params, "en");
  assert.match(refresh, /driver extension refresh was .* before and .* after the refresh/);
  assert.match(refresh, /expected a confirmed unmerge and subsequent merge, with a merge time different from before/);
  const version = expandText("check.reason", items[1] as unknown as Params, "en");
  assert.match(version, /driver version was nvidia-smi 595\.104\.02/);
  assert.match(version, /expected 595\.104\.02/);
});

test("restore-failed notice with nothing unloaded drops the reload sentence", () => {
  const detail = detailWith(
    "FAILED",
    {
      key: "notice.restore_failed",
      variant: "before_write",
      params: { reload_failed: false },
    },
    {
      code: "RESTORE_NOT_WRITTEN",
      params: { cause: "write", write_reason: "write.temp_failed" },
      command: null,
      exit: null,
      raw: null,
    },
  );
  (detail as { replace: unknown }).replace = [
    { pass: "restore", unloaded: [], reloaded: [], refresh_exit: null, ldconfig_exit: null, readback_ok: null, readonly_after: null },
  ];
  const params = taskNoticeParams(detail, "en");
  assert.equal(params["not_unloaded"], true);
  assert.notEqual(params["reload_failed"], true);
  const text = expandText("notice.restore_failed", params, "en", "before_write");
  assert.equal(text.includes("already unloaded have been reloaded"), false);
  assert.match(text, /the driver file is still 595\.104\.02/i);
});

test("a failed reload keeps the reload sentence", () => {
  const detail = detailWith(
    "FAILED",
    { key: "notice.restore_failed", variant: "before_write", params: { reload_failed: true } },
    {
      code: "RESTORE_NOT_WRITTEN",
      params: { cause: "write", write_reason: "write.temp_failed" },
      command: null,
      exit: null,
      raw: null,
    },
  );
  (detail as { replace: unknown }).replace = [
    {
      pass: "restore",
      unloaded: ["nvidia"],
      reloaded: [{ module: "nvidia", ok: false, reason: { key: "reason.exit_status", params: { code: 1 } } }],
      refresh_exit: null,
      ldconfig_exit: null,
      readback_ok: null,
      readonly_after: null,
    },
  ];
  const params = taskNoticeParams(detail, "en");
  assert.equal(params["reload_failed"], true);
  assert.equal(params["not_unloaded"], undefined);
  const text = expandText("notice.restore_failed", params, "en", "before_write");
  assert.match(text, /Reloading failed/);
  assert.match(text, /GPU apps may fail to start/);
  assert.equal(text.includes("already unloaded have been reloaded"), false);
});

test("code explanation uses the save reason from error.params", () => {
  const detail = detailWith(
    "FAILED",
    { key: "code.explanation", params: { code: "SAVE_ORIGINAL_FAILED", reload_failed: false } },
    {
      code: "SAVE_ORIGINAL_FAILED",
      params: { save_reason: { key: "save.facts_unreadable", params: {} } },
      command: null,
      exit: null,
      raw: null,
    },
  );
  const params = taskNoticeParams(detail, "zh");
  assert.deepEqual(params["save_reason"], { key: "save.facts_unreadable", params: {} });
  assert.equal(params["code"], "SAVE_ORIGINAL_FAILED");
});

test("the no-original reason parameter keeps the complete restore reason key", () => {
  // The state file stores the complete key (`restore.no_original.*`), so the parameter must not
  // prepend the prefix again.
  for (const reason of [
    "restore.no_original.not_saved",
    "restore.no_original.checksum_mismatch",
    "restore.no_original.unreadable",
  ]) {
    const readReason = reason === "restore.no_original.unreadable" ? "reason.io_error" : null;
    const param = noOriginalReasonParam({
      decision: "none",
      reason,
      read_reason: readReason,
      original: null,
    });
    assert.equal(param.key, reason);
    assert.deepEqual(
      param.params,
      readReason === null ? {} : { read_reason: { key: "reason.io_error", params: {} } },
    );
  }
  const detail = detailWith("FAILED", { key: "notice.no_restore", params: {} }, null);
  detail.restore = {
    decision: "none",
    reason: "restore.no_original.unreadable",
    read_reason: "reason.io_error",
    original: null,
  };
  const params = taskNoticeParams(detail, "en");
  assert.deepEqual(params["no_original_reason"], {
    key: "restore.no_original.unreadable",
    params: { read_reason: { key: "reason.io_error", params: {} } },
  });
  const text = expandText("notice.no_restore", params, "en");
  assert.match(text, /the original driver file could not be read: a read error occurred/);
  assert.equal(text.includes("restore.no_original.restore.no_original"), false);
});

test("reload failure copy follows the last corrective round", () => {
  for (const pass of ["update", "restore"] as const) {
    for (const finalOk of [false, true]) {
      const detail = detailWith("FAILED", {
        key: pass === "update" ? "notice.unload_refused" : "notice.restore_stopped", params: { reload_failed: finalOk },
      }, {
        code: pass === "update" ? "MODULE_UNLOAD_REFUSED" : "RESTORE_ABORTED",
        params: { cause: "unload_refused", module: "nvidia", unload_reason: { stderr: "busy" } },
        command: null, exit: null, raw: null,
      });
      (detail as { replace: unknown }).replace = [{
        pass, unloaded: ["nvidia"],
        reloaded: [{ module: "nvidia", ok: !finalOk, reason: finalOk ? { stderr: "old failure" } : null }],
        module_correction: { reloaded: [{ module: "nvidia", ok: finalOk, reason: finalOk ? null : { stderr: "final failure" } }] },
      }];
      const params = taskNoticeParams(detail, "en");
      assert.equal(params["reload_failed"], !finalOk);
    }
  }
});


test("support notes render the backend lower bound as one sentence", () => {
  const result = run(["2b85"], ["2b85"]);
  assert.deepEqual(result.notes.map(({ kind, branch }) => ({ kind, branch })), [{ kind: "from", branch: 580 }]);
  assert.equal("missing" in result.notes[0]!, false);
  for (const lang of ["zh", "en"] as const) {
    const plain = expandText("catalog.gpu.not_listed", { gpu_name: "GPU", branch: result.notes[0]!.branch }, lang, "from");
    assert.equal(plain, lang === "zh" ? "GPU 从 580 驱动起支持。" : "GPU is supported from driver 580.");
  }
});
