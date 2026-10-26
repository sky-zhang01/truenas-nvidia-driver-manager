import { test } from "node:test";
import assert from "node:assert/strict";
import { ApiError } from "../src/api.js";
import { expandText } from "../src/dom.js";
import { renderPlanError } from "../src/views/plan.js";
import { FakeElement, installDom } from "./dom-shim.js";
import { catalogHelpKey, joinReasons, type GpuFilterResult } from "../src/rules.js";
import { MESSAGES } from "../src/messages.js";

test("official-version rejection names the version and the same-branch replacement", () => {
  installDom();
  const box = new FakeElement("div");
  renderPlanError({ lang: "zh" } as never, box as never, new ApiError(422, "plan.source_invalid.branch_unbuildable", {
    version: "580.119.02", from: "580.126.09", reasons: [{ key: "branch.fb_create_format_info", params: {} }],
  }, undefined, null), false);
  assert.equal(box.textContent, "错误版本 580.119.02 无法在本机构建：" + expandText("branch.fb_create_format_info", {}, "zh") + "。请改用 580.126.09 或同分支更新的版本。");
});

test("GPU rejection has a version headline only when it has per-card details", () => {
  installDom();
  const render = (params: Record<string, unknown>): string => {
    const box = new FakeElement("div");
    renderPlanError({ lang: "zh" } as never, box as never, new ApiError(422, "plan.source_invalid.gpu_unsupported", params, undefined, null), false);
    return box.textContent;
  };
  const gpus = [{ device: "2b85", name: "RTX 5090", first_branch: 570, usable_from_branch: 580 }];
  assert.equal(render({ version: "550.163.01", gpus, conflict: false }), "错误下列显卡不支持版本 550.163.01。错误RTX 5090 从 580 驱动起支持。");
  assert.equal(render({ version: "550.163.01", gpus: [], conflict: false }), "");
  assert.equal(render({ version: "550.163.01", gpus: [{ device: "2b85", usable_from_branch: null }], conflict: false }), "");
  assert.equal(render({ version: "590.48.01", gpus: [{ device: "1b80", name: "GTX 1080", last_branch: 580, usable_from_branch: 470 }], conflict: false }), "错误下列显卡不支持版本 590.48.01。错误GTX 1080 最高支持 580 驱动。");
  assert.equal(render({ gpus, conflict: false }), "错误RTX 5090 从 580 驱动起支持。");
});

for (const lang of ["zh", "en"] as const) {
  test(`a GPU conflict keeps per-card explanations before its conclusion without a version headline in ${lang}`, () => {
    installDom();
    const box = new FakeElement("div");
    const gpus = [
      { device: "1b80", name: "GTX 1080", last_branch: 580, usable_from_branch: 470 },
      { device: "2b85", name: "RTX 5090", first_branch: 570, usable_from_branch: 580 },
    ];
    renderPlanError({ lang } as never, box as never, new ApiError(422, "plan.source_invalid.gpu_unsupported", {
      version: "550.163.01", gpus, conflict: true,
    }, undefined, null), false);
    const level = lang === "zh" ? "错误" : "Error";
    assert.deepEqual(box.children.map((node) => node.textContent), [
      level + expandText("catalog.gpu.not_listed", { gpu_name: "GTX 1080", branch: 580 }, lang),
      level + expandText("catalog.gpu.not_listed", { gpu_name: "RTX 5090", branch: 580 }, lang, "from"),
      level + expandText("catalog.no_usable_version", {}, lang),
    ]);
    assert.equal(box.textContent.includes(expandText("plan.source_invalid.gpu_unsupported", { version: "550.163.01" }, lang)), false);
  });
}

test("an unconfirmed official version appends the catalog read failure", () => {
  installDom();
  const box = new FakeElement("div");
  renderPlanError({ lang: "en" } as never, box as never, new ApiError(409, "plan.source_invalid.version_unconfirmed", {
    version: "580.159.03", unreadable: [{ object: "catalog", reason: { key: "reason.io_error", params: {} }, raw: "catalog unavailable" }],
  }, undefined, null), false);
  assert.match(box.textContent, /Cannot confirm whether NVIDIA lists version 580.159.03\. The update was not started\./);
  assert.match(box.textContent, /catalog unavailable/);
});

test("approved source and input messages match both languages", () => {
  const expected = {
    "catalog.other_version": { zh: "其他版本…", en: "Other version…" },
    "update.other_version_example": { zh: "例如 580.159.03", en: "For example 580.159.03" },
    "plan.source_invalid.version_not_official": { zh: "error|NVIDIA 官网没有版本 {version}，请核对版本号。", en: "error|NVIDIA does not list version {version}. Check the version number." },
    "plan.source_invalid.version_below_470": { zh: "error|官网版本须为 470 及以上。", en: "error|Official versions must be 470 or later." },
    "confirm.upload_trust": { zh: "上传的安装包将在本机构建并加载进内核，程序不验证 NVIDIA 签名。", en: "The uploaded installer is built on this host and loaded into its kernel, and the program does not verify NVIDIA signatures." },
    "upload.how.nosig": { zh: "程序不验证 NVIDIA 签名。", en: "The program does not verify NVIDIA signatures." },
  };
  for (const [key, message] of Object.entries(expected)) assert.deepEqual(MESSAGES[key], message, key);
});


test("catalog guidance and other-version availability follow the filtered list and read status", () => {
  const filtered: GpuFilterResult = { options: ["580.178.04"], notes: [], conflict: false, noUsableVersion: false, noneSelected: false };
  assert.equal(catalogHelpKey(filtered, []), "catalog.help");
  assert.equal(catalogHelpKey(filtered, [{ object: "catalog", reason: { key: "reason.io_error", params: {} } }]), "catalog.help.no_other");
  assert.equal(catalogHelpKey({ ...filtered, options: [], noUsableVersion: true }, []), "catalog.help.no_other");
  assert.equal(catalogHelpKey({ ...filtered, options: [], noneSelected: true }, []), "catalog.help.no_other");
  assert.equal(catalogHelpKey({ ...filtered, options: [], conflict: true, noUsableVersion: true }, []), null);
  assert.deepEqual(MESSAGES["catalog.help.no_other"], { zh: "info|离线或自备文件请用「手动上传文件…」。", en: 'info|For offline or your own files, use "Upload files manually…".' });
});

test("multiple branch reasons use Chinese full-width punctuation and English spacing", () => {
  const reasons = [{ key: "branch.extra_cflags_removed", params: {} }, { key: "branch.fb_create_format_info", params: {} }];
  assert.equal(joinReasons(reasons, "zh"), reasons.map((r) => expandText(r.key, {}, "zh")).join("；"));
  assert.equal(joinReasons(reasons, "en"), reasons.map((r) => expandText(r.key, {}, "en")).join("; "));
});
