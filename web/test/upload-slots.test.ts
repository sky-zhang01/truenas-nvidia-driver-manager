import { test } from "node:test";
import assert from "node:assert/strict";
import type { GroupSummary, UploadFileInfo } from "../src/api.js";
import { TOOLKIT_PACKAGES, deriveUploadSlots, rejectionSlot, sortUploadFiles, uploadRejectionKey } from "../src/upload-slots.js";
import { expandText } from "../src/dom.js";

test("a rejected Debian version uses actionable checklist wording without withheld fields", () => {
  const key = uploadRejectionKey("deb_version_format");
  assert.equal(key, "upload.err.deb_version_format");
  for (const lang of ["en", "zh"] as const) {
    const rendered = expandText(key, { file_name: "toolkit.deb", version: "", package: "", architecture: "" }, lang);
    assert.equal(rendered, lang === "en"
      ? "The version of toolkit.deb could not be read; download the package again using the link in the checklist."
      : "无法读取 toolkit.deb 的版本号，请从清单中的链接重新下载对应包。");
    assert.equal(/UNKNOWN|undefined|\{[^}]+\}|  /.test(rendered), false);
  }
});

function group(driver: string[] | null = null, versions: string[] = []): GroupSummary {
  return {
    group_id: "test", run: driver === null ? null : { sha256: "hash", fingerprint: "hash", candidates: driver },
    debs: Object.fromEntries(versions.map((version, i) => [TOOLKIT_PACKAGES[i], { version }])),
    missing: TOOLKIT_PACKAGES.slice(versions.length), toolkit_version: versions.length === 4 && new Set(versions).size === 1 ? versions[0].split("-")[0] : null,
    below_security_floor: false, problems: [],
  };
}
const complete = (driver = "595.58.03", toolkit = "1.20.1-1"): GroupSummary => group([driver], Array(4).fill(toolkit));

test("empty group has exactly five waiting slots", () => {
  const result = deriveUploadSlots(null, null);
  assert.deepEqual(result.slots.map((slot) => slot.id), ["run", ...TOOLKIT_PACKAGES]);
  assert.deepEqual(result.slots.map((slot) => slot.state), Array(5).fill("waiting"));
  assert.equal(result.ready, 0); assert.equal(result.missing, 5); assert.equal(result.problem, 0);
  assert.equal(result.canContinue, false);
});

test("partial group derives missing from actual content rather than stale missing flags", () => {
  const g = group(["595.58.03"], ["1.20.1-1"]); g.missing = [];
  const result = deriveUploadSlots(g, null);
  assert.equal(result.ready, 2); assert.equal(result.missing, 3); assert.equal(result.problem, 0);
  assert.equal(result.driverVersion, "595.58.03");
});

test("five complete files enable continuing and keep Debian revision", () => {
  const result = deriveUploadSlots(complete(), null);
  assert.equal(result.ready, 5); assert.equal(result.canContinue, true);
  assert.equal(result.toolkitVersion, "1.20.1-1");
});

test("selected download version cannot override the only detected driver candidate", () => {
  const result = deriveUploadSlots(complete(), "615.1.2");
  assert.equal(result.driverVersion, "595.58.03"); assert.equal(result.canContinue, true);
});

test("multiple candidates require explicit confirmation and never select first", () => {
  const g = complete(); g.run!.candidates = ["595.58.03", "615.1.2"];
  const undecided = deriveUploadSlots(g, null);
  assert.equal(undecided.driverVersion, null); assert.equal(undecided.versionMode, "choose");
  assert.equal(undecided.slots[0].state, "problem"); assert.equal(undecided.canContinue, false);
  assert.equal(deriveUploadSlots(g, "595.58.03").canContinue, true);
  assert.equal(deriveUploadSlots(g, "580.1.2").canContinue, false);
});

test("unreadable driver version requires a valid two to four component manual version", () => {
  const g = complete(); g.run!.candidates = [];
  assert.equal(deriveUploadSlots(g, null).versionMode, "manual");
  for (const invalid of ["615", "615.1000.2.3.4", "abc", "615.1<script>"]) {
    assert.equal(deriveUploadSlots(g, invalid).canContinue, false);
  }
  for (const valid of ["615.1", "615.1.2", "615.1000.2.3"]) assert.equal(deriveUploadSlots(g, valid).canContinue, true);
});

test("toolkit majority identifies only the mismatched slot", () => {
  const g = group(["595.58.03"], ["1.19.0-1", "1.20.1-1", "1.20.1-1", "1.20.1-1"]);
  const result = deriveUploadSlots(g, null);
  assert.equal(result.ready, 4); assert.equal(result.problem, 1);
  assert.equal(result.slots[1].state, "problem");
  assert.equal(result.slots[1].problems[0].key, "upload.err.version_mismatch");
  assert.equal(result.slots[1].problems[0].params?.others, "1.20.1-1");
});

test("no toolkit majority marks all supplied conflicting versions for attention", () => {
  const result = deriveUploadSlots(group(["595.58.03"], ["1.19.0-1", "1.19.0-1", "1.20.1-1", "1.20.1-1"]), null);
  assert.equal(result.ready, 1); assert.equal(result.problem, 4); assert.equal(result.missing, 0);
  assert.equal(result.canContinue, false);
});

test("partial differing versions cannot silently count as ready", () => {
  const result = deriveUploadSlots(group(null, ["1.19.0-1", "1.20.1-1"]), null);
  assert.equal(result.ready, 0); assert.equal(result.problem, 2); assert.equal(result.missing, 3);
});

test("driver 615 toolkit floor blocks all old toolkit slots and gives one merged reason", () => {
  const g = complete("615.1.2", "1.19.0-1"); g.below_security_floor = true;
  g.problems = [{key: "upload.group.toolkit_too_old"}];
  const result = deriveUploadSlots(g, null);
  assert.equal(result.ready, 1); assert.equal(result.problem, 4); assert.equal(result.canContinue, false);
  assert.equal(result.problems.filter((p) => p.key === "upload.err.toolkit_too_old").length, 1);
  assert.equal(result.warnings.some((p) => p.key === "toolkit.below_floor"), false);
  assert.equal(deriveUploadSlots(complete("615.1.2", "1.20.1-1"), null).canContinue, true);
});

test("security floor is advisory only for confirmed older drivers", () => {
  const g = complete("595.58.03", "1.18.0-1"); g.below_security_floor = true;
  assert.equal(deriveUploadSlots(g, null).warnings[0].key, "toolkit.below_floor");
  assert.equal(deriveUploadSlots(g, null).canContinue, true);
  g.run!.candidates = [];
  assert.equal(deriveUploadSlots(g, null).warnings.length, 0);
});

test("accepted receipts attach by backend kind/package, never by client filename", () => {
  const receipt: UploadFileInfo = {kind: "deb", package: TOOLKIT_PACKAGES[0], version: "1.20.1-1", client_name: "renamed.run", sha256: "hash", size: 10};
  const result = deriveUploadSlots(complete(), null, [receipt]);
  assert.equal(result.slots[1].file, receipt); assert.equal(result.slots[0].file, undefined);
  const cleared = deriveUploadSlots(null, null, [receipt]);
  assert.equal(cleared.slots[1].file, undefined);
});

test("replacing a toolkit slot uses the latest accepted matching receipt", () => {
  const base: UploadFileInfo = {kind: "deb", package: TOOLKIT_PACKAGES[0], version: "1.19.0-1", client_name: "old.deb", sha256: "old", size: 10};
  const replacement = {...base, version: "1.20.1-1", client_name: "new.deb", sha256: "new"};
  assert.equal(deriveUploadSlots(complete(), null, [base, replacement]).slots[1].file, replacement);
});

test("rejected unknown, dev and non installer files stay unassigned even with plausible filenames", () => {
  for (const params of [
    { package: "nvidia-container-toolkit-dev", file_name: "nvidia-container-toolkit.deb" },
    { reason: "deb_arch", file_name: "libnvidia-container1.deb" },
    { reason: "not_run_or_deb", file_name: "driver.run.sha256sum" },
  ]) assert.equal(rejectionSlot(params), null);
  assert.equal(rejectionSlot({kind: "run"}), "run");
  assert.equal(rejectionSlot({package: TOOLKIT_PACKAGES[0]}), TOOLKIT_PACKAGES[0]);
});

test("upload ordering is stable: deb first, run next, unsupported files last", () => {
  const files = [{name: "z.run"}, {name: "b.deb"}, {name: "a.deb"}, {name: "checksum.sha256sum"}, {name: "a.RUN"}];
  const ordered = sortUploadFiles(files);
  assert.deepEqual(ordered.map((file) => file.name), ["b.deb", "a.deb", "z.run", "a.RUN", "checksum.sha256sum"]);
  assert.equal(files[0].name, "z.run");
});

test("toolkit floor uses the existing numeric comparison for two and four component versions", () => {
  for (const version of ["1.21-1", "1.20.1.1000-1"]) {
    const group = complete();
    group.run!.candidates = ["615.71.09"];
    for (const pkg of TOOLKIT_PACKAGES) group.debs[pkg].version = version;
    assert.equal(deriveUploadSlots(group, null).canContinue, true);
  }
});
