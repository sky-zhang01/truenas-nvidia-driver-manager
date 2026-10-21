import { test } from "node:test";
import assert from "node:assert/strict";
import { deriveUploadSlots, type UploadSlots } from "../src/upload-slots.js";
import { uploadContinueReason } from "../src/upload-continue.js";

const flags = { checking: false, loading: false, readFailed: false, busy: false, blocked: false, gpuOk: true, version: "580.178.04" };
const ready: UploadSlots = { ...deriveUploadSlots(null, null), ready: 5, missing: 0, problem: 0, versionMode: "detected", problems: [], canContinue: true };

test("upload footer keeps its check, load, read, transfer, host and GPU priority", () => {
  let state = { ...flags, checking: true, loading: true, readFailed: true, busy: true, blocked: true, gpuOk: false };
  for (const [field, key] of [
    ["checking", "upload.checking"], ["loading", "upload.loading"], ["readFailed", "upload.read_failed"],
    ["busy", "upload.why.busy"], ["blocked", "upload.why.blocked"], ["gpuOk", "upload.why.gpu"],
  ] as const) {
    assert.equal(uploadContinueReason(ready, state), key);
    state = { ...state, [field]: field === "gpuOk" };
  }
  assert.equal(uploadContinueReason(ready, state), "upload.why.ready");
});

test("version confirmation precedes checklist counts", () => {
  for (const [versionMode, key] of [["choose", "upload.why.version"], ["manual", "upload.why.version"]] as const) {
    assert.equal(uploadContinueReason({ ...ready, versionMode, problem: 1, missing: 1 }, { ...flags, version: null }), key);
  }
});

test("an invalid manual version keeps the short version reason ahead of checklist counts", () => {
  const manual = { ...ready, versionMode: "manual" as const, canContinue: false, problem: 1, missing: 1 };
  assert.equal(uploadContinueReason(manual, { ...flags, version: "58" }), "upload.why.version");
});

test("zero and nonzero missing counts retain distinct summaries", () => {
  assert.equal(uploadContinueReason({ ...ready, problem: 2, missing: 0 }, flags), "upload.why.problem_only");
  assert.equal(uploadContinueReason({ ...ready, problem: 2, missing: 1 }, flags), "upload.why.problem");
  assert.equal(uploadContinueReason({ ...ready, missing: 1 }, flags), "upload.why.missing");
  assert.equal(uploadContinueReason({ ...ready, problems: [{ key: "upload.group.version_required" }] }, flags), "upload.why.review");
  assert.equal(uploadContinueReason(ready, flags), "upload.why.ready");
});
