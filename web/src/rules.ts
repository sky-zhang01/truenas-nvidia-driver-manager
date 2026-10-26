import type { CheckRecord, GpuReading, ReasonField, TaskDetail, Unreadable, Reading } from "./api.js";
import { isUnknown } from "./api.js";
import { expandText, formatDate, formatDateMicros, type Params, type ReasonLike } from "./dom.js";
import type { Lang } from "./messages.js";

export const VERSION_PATTERN = /^[0-9]{1,9}(\.[0-9]{1,9}){1,3}$/;

export function versionFormatOk(s: string): boolean {
  return VERSION_PATTERN.test(s);
}

export function parseVersion(s: string): number[] | null {
  if (!VERSION_PATTERN.test(s)) return null;
  return s.split(".").map((p) => Number(p));
}

// Non-version text sorts before valid versions.
export function compareVersion(a: string, b: string): number {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  if (pa === null && pb === null) return a < b ? -1 : a > b ? 1 : 0;
  if (pa === null) return -1;
  if (pb === null) return 1;
  const len = Math.min(pa.length, pb.length);
  for (let i = 0; i < len; i += 1) {
    const x = pa[i]!;
    const y = pb[i]!;
    if (x !== y) return x < y ? -1 : 1;
  }
  return pa.length < pb.length ? -1 : pa.length > pb.length ? 1 : 0;
}

export function majorOf(s: string): number | null {
  const p = parseVersion(s);
  return p === null ? null : (p[0] ?? null);
}

const TOOLKIT_SECURITY_FLOOR = "1.19.1";
const TOOLKIT_DRIVER_FLOOR_BRANCH = 615;
const TOOLKIT_DRIVER_FLOOR = "1.20.1";

export const TOOLKIT_PACKAGES: readonly string[] = [
  "libnvidia-container1",
  "libnvidia-container-tools",
  "nvidia-container-toolkit-base",
  "nvidia-container-toolkit",
];

export function toolkitUpstream(version: string): string {
  const bar = version.indexOf("-");
  return bar < 0 ? version : version.slice(0, bar);
}

export function toolkitBelowFloor(toolkitVersion: string): boolean {
  return compareVersion(toolkitUpstream(toolkitVersion), TOOLKIT_SECURITY_FLOOR) < 0;
}

// Drivers 615 and later require toolkit 1.20.1 or later.
export function toolkitTooOld(driverVersion: string, toolkitVersion: string | null): boolean {
  const major = majorOf(driverVersion);
  if (major === null || major < TOOLKIT_DRIVER_FLOOR_BRANCH) return false;
  if (toolkitVersion === null) return false;
  return compareVersion(toolkitUpstream(toolkitVersion), TOOLKIT_DRIVER_FLOOR) < 0;
}

export const UPLOAD_LIMIT_BYTES = 1024 * 1024 * 1024;

export function fileTooLarge(size: number): boolean {
  return size > UPLOAD_LIMIT_BYTES;
}

export type SortColumn = "version" | "source" | "date" | "size" | "kernel" | "status" | "keep";
export type SortDir = "asc" | "desc";

export interface SortRow {
  version: string | null;
  source: string | null;
  date: number | null;
  size: number | null;
  kernel: string | null;
  status: string;
  keep: boolean | null;
}

// UNKNOWN and empty values sort last in either direction.
export function compareRows(a: SortRow, b: SortRow, col: SortColumn, dir: SortDir): number {
  const sign = dir === "asc" ? 1 : -1;
  const av = a[col];
  const bv = b[col];
  const aUnknown = av === null || av === undefined || av === "UNKNOWN";
  const bUnknown = bv === null || bv === undefined || bv === "UNKNOWN";
  if (aUnknown && bUnknown) return 0;
  if (aUnknown) return 1;
  if (bUnknown) return -1;
  let cmp = 0;
  switch (col) {
    case "version":
      cmp = compareVersion(String(av), String(bv));
      break;
    case "date":
    case "size":
      cmp = Number(av) < Number(bv) ? -1 : Number(av) > Number(bv) ? 1 : 0;
      break;
    case "keep":
      cmp = Number(av) < Number(bv) ? -1 : Number(av) > Number(bv) ? 1 : 0;
      break;
    default:
      cmp = String(av) < String(bv) ? -1 : String(av) > String(bv) ? 1 : 0;
      break;
  }
  return cmp * sign;
}

export interface GpuNote {
  gpu: GpuReading;
  kind: "last" | "from";
  branch: number;
}

export interface GpuFilterResult {

  options: string[];

  notes: GpuNote[];

  noUsableVersion: boolean;
  // Multiple counted GPUs with no selection show only the selection prompt.
  noneSelected: boolean;

  conflict: boolean;
}

function byAddress(a: GpuReading, b: GpuReading): number {
  return a.address < b.address ? -1 : a.address > b.address ? 1 : 0;
}

export function conflict(selected: GpuReading[]): boolean {
  let closed = false;
  let open = false;
  for (const g of selected) {
    if (g.modules === "closed_only") closed = true;
    if (g.modules === "open_only") open = true;
  }
  return closed && open;
}

export function gpuLabel(gpu: GpuReading): string {
  return gpu.name && gpu.name !== "" ? gpu.name : `10de:${gpu.device}`;
}

export function filterVersions(
  items: string[],
  openUnbuildable: readonly string[],
  unreadable: Unreadable[],
  gpus: GpuReading[],
  selected: readonly string[],
): GpuFilterResult {
  const counted = [...gpus].sort(byAddress);
  const picked = counted.filter((g) => selected.includes(g.address));
  if (counted.length === 0) {
    return { options: items, notes: [], noUsableVersion: false, noneSelected: false, conflict: false };
  }
  if (counted.length >= 2 && picked.length === 0) {
    return { options: [], notes: [], noUsableVersion: false, noneSelected: true, conflict: false };
  }
  const clash = conflict(picked);
  const usesOpen = picked.length > 0 && !picked.some((g) => g.modules === "closed_only");
  const excluded = new Set<string>();
  const notes: GpuNote[] = [];
  for (const g of picked) {
    for (const v of g.too_new) excluded.add(v);
    for (const v of g.too_old) excluded.add(v);
    if (g.too_new.length > 0 && g.last_branch !== undefined) {
      notes.push({ gpu: g, kind: "last", branch: g.last_branch });
    }
    const usableFrom = g.usable_from_branch;
    if (usableFrom !== undefined && items.some((v) => {
      const major = majorOf(v);
      return major !== null && major <= usableFrom
        && (g.too_old.includes(v)
          || (!clash && usesOpen && !g.too_new.includes(v) && openUnbuildable.includes(v)));
    })) {
      notes.push({ gpu: g, kind: "from", branch: usableFrom });
    }
  }
  if (clash) {
    return { options: [], notes, noUsableVersion: true, noneSelected: false, conflict: true };
  }
  const options = items.filter((v) => !excluded.has(v)
    && !(usesOpen && openUnbuildable.includes(v)));
  const noUsableVersion = picked.length > 0 && unreadable.length === 0 && options.length === 0;
  return { options, notes, noUsableVersion, noneSelected: false, conflict: false };
}

export function catalogHelpKey(filtered: GpuFilterResult, unreadable: Unreadable[]): string | null {
  if (filtered.conflict) return null;
  return unreadable.length === 0 && filtered.options.length > 0 ? "catalog.help" : "catalog.help.no_other";
}

export function joinReasons(reasons: ReasonLike[], lang: Lang): string {
  const sep = lang === "zh" ? "；" : "; ";
  return reasons.map((r) => expandText(r.key, (r.params ?? {}) as Params, lang)).join(sep);
}

const BUILD_PLAIN: readonly string[] = [
  "run_check_failed",
  "modules_copy_failed",
  "kernel_gcc_unknown",
  "extract_failed",
  "options_unreadable",
  "kernel_module_type_unsupported",
  "installer_failed",
  "installer_userspace_failed",
  "installer_kernel_module_failed",
  "kernel_module_build_failed",
  "depmod_failed",
  "docker_run_failed",
  "container_exit",
];

const BUILD_SAME_TEXT: Record<string, string> = {
  capture_empty: "build.capture_failed",
  stage_copy_failed: "build.capture_failed",
  release_write_failed: "build.release_write_failed",
  mksquashfs_failed: "build.mksquashfs_failed",
  result_write_failed: "build.mksquashfs_failed",
};

// Colon-suffixed reason codes carry their parameter after the colon.
export function normalizeReason(code: string, params: Params = {}): ReasonField {
  if (code === "community_patch_failed:archive") {
    return { key: "build.community_patch_failed.archive", params: {} };
  }
  const patchFailure = /^community_patch_failed:(0[1-9]|[1-4][0-9]|5[0-4])$/.exec(code);
  if (patchFailure) {
    return { key: "build.community_patch_failed", params: { patch: patchFailure[1] } };
  }
  const moduleFailure = /^(kernel_module_missing|kernel_module_install_failed):(nvidia|nvidia_uvm|nvidia_modeset|nvidia_drm|nvidia_peermem)$/.exec(code);
  if (moduleFailure) {
    return {
      key: `build.${moduleFailure[1]}`,
      params: { module: moduleFailure[2]!.replaceAll("_", "-") },
    };
  }
  if (/^compiler_missing:gcc-[0-9]+$/.test(code)) {
    return { key: "build.compiler_missing", params: { version: code.slice("compiler_missing:gcc-".length) } };
  }
  if (code.startsWith("deb_extract_failed:")) {
    return { key: "build.deb_extract_failed", params: { package: code.slice("deb_extract_failed:".length) } };
  }
  if (/^unexpected:[0-9]+$/.test(code)) {
    return { key: "build.unexpected", params: { line: code.slice("unexpected:".length) } };
  }
  if (code.startsWith("artifact.")) {
    return { key: `build.${code}`, params };
  }
  if (BUILD_PLAIN.includes(code)) {
    return { key: `build.${code}`, params };
  }
  if (BUILD_SAME_TEXT[code]) {
    return { key: BUILD_SAME_TEXT[code] as string, params };
  }
  return { key: code, params };
}

export function stateReadErrorReasonKey(code: string): string {
  return `state_read_error.reason.${code}`;
}

// Stored restore reason keys are complete catalog keys. Preserve nested read_reason only for an
// unreadable original.
export function noOriginalReasonParam(restore: TaskDetail["restore"]): ReasonField {
  return {
    key: restore?.reason ?? "restore.no_original.not_saved",
    params: restore?.read_reason ? { read_reason: normalizeReason(restore.read_reason, {}) } : {},
  };
}

export function queryObjectKey(code: string): string {
  return `query.object.${code}`;
}

export const QUERY_OBJECT_HOST_TASK_UNIT = "query.object.host_task_unit";

export const QUERY_OBJECT_RUNNING_TASK = "query.object.running_task";

export function sourceKey(kind: string): string {
  return `drivers.source.${kind}`;
}

export interface CheckParams {
  check_item: ReasonField;
  observed: ReasonField;
  expected: ReasonField;
}

function sideText(reading: Reading<string>): unknown {
  if ("value" in reading) return reading.value;
  return { key: "check.observed.version_unreadable", params: {} };
}

export function refreshObserved(
  before: Reading<number>,
  after: Reading<number>,
  refreshOk: boolean | undefined,
): ReasonField {
  // A readable status with no initial merge time cannot establish a refresh comparison, even if the
  // task merges the extension at the end.
  if (
    isUnknown(before) &&
    before.unknown.key === "reason.format_unrecognized" &&
    !isUnknown(after)
  ) {
    return { key: "check.observed.refresh_before_unmerged", params: {} };
  }
  // Only not_loaded means absent; unreadable systemd-sysext status must retain its own wording.
  if (isUnknown(before) || isUnknown(after)) {
    return { key: "check.observed.status_unreadable", params: {} };
  }
  if ("value" in before && "value" in after) {
    if (!Number.isSafeInteger(before.value) || before.value < 0
      || !Number.isSafeInteger(after.value) || after.value < 0) {
      return { key: "check.observed.status_unreadable", params: {} };
    }
    return {
      key: refreshOk === false && before.value !== after.value
        ? "check.observed.refresh_unconfirmed"
        : "check.observed.refresh_both",
      params: {
        before: formatDateMicros(before.value),
        after: formatDateMicros(after.value),
      },
    };
  }
  if (!("value" in before)) {
    return { key: "check.observed.refresh_before_absent", params: {} };
  }
  if (!("value" in after)) {
    return { key: "check.observed.refresh_after_absent", params: {} };
  }
  return { key: "check.observed.status_unreadable", params: {} };
}

export function versionObserved(smi: Reading<string>, proc: Reading<string>): ReasonField {
  return {
    key: "check.observed.version_both",
    params: {
      smi_version: sideText(smi),
      proc_version: sideText(proc),
    },
  };
}

export function refreshParams(
  rec: Pick<CheckRecord, "since_before" | "since_after"> & { refresh_ok: boolean | undefined },
): CheckParams {
  return {
    check_item: { key: "check.item.refresh", params: {} },
    observed: refreshObserved(rec.since_before, rec.since_after, rec.refresh_ok),
    expected: { key: "check.expected.refresh", params: {} },
  };
}

function versionParams(rec: CheckRecord): CheckParams {
  return {
    check_item: { key: "check.item.version", params: {} },
    observed: versionObserved(rec.nvidia_smi, rec.proc_version),
    expected: { key: "check.expected.version", params: { target: rec.expected } },
  };
}

// Include passing items too so successful task details show their actual readings.
export function checkItems(rec: CheckRecord): CheckParams[] {
  return [refreshParams(rec), versionParams(rec)];
}

export function checkFailureItems(rec: CheckRecord): CheckParams[] {
  const out: CheckParams[] = [];
  if (!rec.refresh_ok) out.push(refreshParams(rec));
  if (!rec.version_ok) out.push(versionParams(rec));
  return out;
}

export function buildStepKey(step: string): string {
  return `log.build_step.${step}`;
}

export function readingText(r: Reading<unknown> | null | undefined): string {
  if (!r) return "UNKNOWN";
  if ("value" in r) return String(r.value);
  return "UNKNOWN";
}

function reasonField(value: unknown): ReasonField | { stderr: string } | null {
  if (value && typeof value === "object") {
    const key = (value as { key?: unknown }).key;
    if (typeof key === "string") {
      return normalizeReason(key, ((value as { params?: Params }).params ?? {}) as Params);
    }
    if (typeof (value as { stderr?: unknown }).stderr === "string") {
      return { stderr: (value as { stderr: string }).stderr };
    }
  }
  return null;
}

// Stderr-only reasons use the exit-status phrase while keeping the verbatim output separate.
function exitReason(exit: unknown): ReasonField {
  const e = (exit ?? {}) as {
    code?: unknown;
    signal?: unknown;
    timed_out?: unknown;
    spawn_failed?: unknown;
  };
  if (typeof e.code === "number") return { key: "reason.exit_status", params: { code: e.code } };
  if (typeof e.signal === "number") return { key: "reason.signal", params: { signal: e.signal } };
  if (e.spawn_failed === true) return { key: "reason.spawn_failed", params: {} };
  if (e.timed_out === true) return { key: "reason.timed_out", params: {} };
  return { key: "reason.exit_status", params: {} };
}

// Replace stderr-bearing parameters with phrase-only reasons; return raw text for rendering outside
// alert/status regions.
export function splitRawOutput(
  params: Params,
  exit: unknown,
): { params: Params; raw: string[] } {
  const raw: string[] = [];
  const walk = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(walk);
    if (value && typeof value === "object") {
      const obj = value as Record<string, unknown>;
      if (typeof obj["stderr"] === "string") {
        raw.push(obj["stderr"] as string);
        return exitReason(exit);
      }
      const out: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(obj)) out[k] = walk(v);
      return out;
    }
    return value;
  };
  return { params: walk(params) as Params, raw };
}

function checkRecord(detail: TaskDetail, pass: "update" | "restore"): CheckRecord | undefined {
  return detail.checks.find((c) => c.pass === pass && !c.passed) ?? detail.checks.find((c) => c.pass === pass);
}

export function checkReasonText(detail: TaskDetail, pass: "update" | "restore", lang: Lang): string {
  const rec = checkRecord(detail, pass);
  if (!rec) return "UNKNOWN";
  const items = checkFailureItems(rec);
  if (items.length === 0) return "UNKNOWN";
  const sep = lang === "zh" ? ";" : "; ";
  return items.map((p) => expandText("check.reason", p as unknown as Params, lang)).join(sep);
}

function reloadFailed(detail: TaskDetail, pass: "update" | "restore"): boolean {
  const rec = detail.replace.find((r) => r.pass === pass);
  return rec ? (rec.module_correction?.reloaded ?? rec.reloaded).some((x) => !x.ok) : false;
}

function unloadedEmpty(detail: TaskDetail, pass: "update" | "restore"): boolean {
  const rec = detail.replace.find((r) => r.pass === pass);
  return rec !== undefined && rec.unloaded.length === 0;
}

function reloadReason(detail: TaskDetail, pass: "update" | "restore"): unknown {
  const rec = detail.replace.find((r) => r.pass === pass);
  return (rec?.module_correction?.reloaded ?? rec?.reloaded)?.find((x) => !x.ok)?.reason ?? "UNKNOWN";
}

// Keep restore stderr nested until splitRawOutput so it never enters an alert region.
export function restoreFailureReason(detail: TaskDetail, lang: Lang): unknown {
  const error = detail.error;
  if (!error) return "UNKNOWN";
  const p = error.params as Params;
  if (error.code === "RESTORE_CHECK_FAILED") {
    return {
      key: "restore.fail.check",
      params: { restore_check_reason: checkReasonText(detail, "restore", lang) },
    };
  }
  if (p["cause"] === "deadline") {
    return { key: "restore.fail.deadline", params: { deadline: p["deadline"] ?? 10 } };
  }
  return {
    key: "restore.fail.write",
    params: { write_reason: reasonField(p["write_reason"]) ?? "UNKNOWN" },
  };
}

// Result parameters combine error.params, checks, restore and loaded versions; the notice alone does
// not carry all readings.
export function taskNoticeParams(detail: TaskDetail, lang: Lang): Params {
  const notice = detail.notice;
  if (!notice) return {};
  const params: Params = { ...(notice.params ?? {}) };
  const error = detail.error;
  const p = (error?.params ?? {}) as Params;
  const target =
    detail.plan && typeof detail.plan["version"] === "string" ? String(detail.plan["version"]) : "UNKNOWN";
  const original = detail.restore?.original?.version ?? "UNKNOWN";
  const check = checkReasonText(detail, "update", lang);
  const reloadFields = (pass: "update" | "restore"): void => {
    const correction = detail.replace.find((r) => r.pass === pass)?.module_correction;
    const failed = correction ? reloadFailed(detail, pass) : params["reload_failed"] === true || reloadFailed(detail, pass);
    if (correction) params["reload_failed"] = failed;
    if (failed) {
      params["reload_failed"] = true;
      params["reload_reason"] = reloadReason(detail, pass);
    } else if (unloadedEmpty(detail, pass)) {
      // With no unloaded modules, an "already reloaded" sentence would claim an action that did not occur.
      params["not_unloaded"] = true;
    }
  };
  switch (notice.key) {
    case "notice.restored":
      params["target"] = target;
      params["original"] = original;
      params["check_reason"] = check;
      params["date"] = detail.restore?.original ? formatDate(detail.restore.original.saved_at) : "UNKNOWN";
      break;
    case "notice.gpu_in_use":
      params["read_reason"] = reasonField(p["read_reason"]) ?? "UNKNOWN";
      break;
    case "notice.unload_refused":
      params["module"] = p["module"] ?? "UNKNOWN";
      params["unload_reason"] = reasonField(p["unload_reason"]) ?? "UNKNOWN";
      params["old"] = readingText(detail.loaded_version_start);
      reloadFields("update");
      break;
    case "notice.restore_stopped":
      params["target"] = target;
      params["original"] = original;
      params["check_reason"] = check;
      params["current"] = readingText(detail.loaded_version_end);
      params["read_reason"] = reasonField(p["read_reason"]) ?? "UNKNOWN";
      params["module"] = p["module"] ?? "UNKNOWN";
      params["unload_reason"] = reasonField(p["unload_reason"]) ?? "UNKNOWN";
      reloadFields("restore");
      if ((detail.gpu_users?.users ?? []).length === 0) {
        // An empty application list still keeps the action for selecting an original driver.
        params["empty_list"] = true;
      }
      break;
    case "notice.restore_failed":
      params["target"] = target;
      params["original"] = original;
      params["check_reason"] = check;
      params["restore_reason"] = restoreFailureReason(detail, lang);
      reloadFields("restore");
      break;
    case "notice.no_restore":
      params["target"] = target;
      params["check_reason"] = check;
      params["no_original_reason"] = noOriginalReasonParam(detail.restore);
      break;
    case "code.explanation": {
      params["code"] = error?.code ?? params["code"];
      params["reload_failed"] = false;
      if (error) {
        switch (error.code) {
          case "SAVE_ORIGINAL_FAILED":
            params["save_reason"] = reasonField(p["save_reason"]) ?? "UNKNOWN";
            break;
          case "DOWNLOAD_FAILED":
          case "BUILD_FAILED":
            params["reason"] = reasonField(p["reason"]) ?? "UNKNOWN";
            break;
          case "WRITE_FAILED":
            params["write_reason"] = reasonField(p["write_reason"]) ?? "UNKNOWN";
            params["old"] = readingText(detail.loaded_version_start);
            reloadFields("update");
            break;
          case "REPLACE_DEADLINE_EXCEEDED":
            params["deadline"] = p["deadline"] ?? 10;
            params["old"] = readingText(detail.loaded_version_start);
            reloadFields("update");
            break;
          default:
            break;
        }
      }
      break;
    }
    default:
      break;
  }
  return params;
}
