import type { GroupProblem, GroupSummary, UploadFileInfo } from "./api.js";
import { toolkitTooOld, versionFormatOk } from "./rules.js";

export const TOOLKIT_PACKAGES = [
  "libnvidia-container1",
  "libnvidia-container-tools",
  "nvidia-container-toolkit-base",
  "nvidia-container-toolkit",
] as const;
export type ToolkitPackage = typeof TOOLKIT_PACKAGES[number];
export type UploadSlotId = "run" | ToolkitPackage;
export interface UploadSlot {
  id: UploadSlotId;
  state: "waiting" | "ready" | "problem";
  version: string | null;
  file?: UploadFileInfo;
  problems: GroupProblem[];
}
export interface UploadSlots {
  slots: UploadSlot[];
  ready: number;
  missing: number;
  problem: number;
  canContinue: boolean;
  driverVersion: string | null;
  versionMode: "missing" | "detected" | "choose" | "manual";
  toolkitVersion: string | null;
  /** Unique blocking reasons, including reasons shared by several slots. */
  problems: GroupProblem[];
  warnings: GroupProblem[];
}

const validDriverVersion = (version: string | null): version is string =>
  version !== null && versionFormatOk(version);
const isToolkitPackage = (value: unknown): value is ToolkitPackage =>
  TOOLKIT_PACKAGES.some((name) => name === value);

/** Only server-reported content identity may assign a rejection to a slot. */
export function rejectionSlot(params: Record<string, unknown>): UploadSlotId | null {
  if (isToolkitPackage(params.package)) return params.package;
  return params.kind === "run" ? "run" : null;
}

/** Checklist wording must not depend on package fields withheld by the server. */
export function uploadRejectionKey(reason: string): string {
  const mapped: Record<string, string> = { run_not_shebang: "run_not_installer", run_not_linux_x86_64: "run_not_linux64", header_unreadable: "unreadable", deb_fields_unreadable: "unreadable", deb_package_unknown: "deb_not_needed", deb_arch: "deb_not_amd64" };
  if (mapped[reason]) return `upload.err.${mapped[reason]}`;
  return ["deb_version_format", "write_failed"].includes(reason) ? `upload.err.${reason}` : `upload.rejected.${reason}`;
}

/** Keep browser selection order within each type; do not mutate the selection. */
export function sortUploadFiles<T extends { name: string }>(files: readonly T[]): T[] {
  const order = (file: T): number => /\.deb$/i.test(file.name) ? 0 : /\.run$/i.test(file.name) ? 1 : 2;
  return [...files].sort((a, b) => order(a) - order(b));
}

function belowDriverFloor(version: string): boolean {
  return toolkitTooOld("615.0", version);
}

/**
 * The selected version is the explicit radio/manual confirmation, never the
 * download-link control. Group content remains authoritative over receipts.
 */
export function deriveUploadSlots(
  group: GroupSummary | null,
  selectedVersion: string | null,
  receipts: readonly UploadFileInfo[] = [],
): UploadSlots {
  const candidates = [...new Set(group?.run?.candidates ?? [])];
  const versionMode: UploadSlots["versionMode"] = !group?.run ? "missing"
    : candidates.length === 1 ? "detected" : candidates.length > 1 ? "choose" : "manual";
  const driverVersion = versionMode === "detected" ? candidates[0]
    : versionMode === "choose" ? (selectedVersion !== null && candidates.includes(selectedVersion) ? selectedVersion : null)
    : versionMode === "manual" && validDriverVersion(selectedVersion) ? selectedVersion : null;
  const runProblems: GroupProblem[] = [];
  if (group?.run && driverVersion === null) {
    runProblems.push({key: versionMode === "choose" ? "upload.version.choose" : "upload.version.enter"});
  }
  const slots: UploadSlot[] = [{
    id: "run", state: !group?.run ? "waiting" : runProblems.length ? "problem" : "ready",
    version: driverVersion, problems: runProblems,
  }];
  const versions = TOOLKIT_PACKAGES.flatMap((name) => group?.debs[name] ? [group.debs[name].version] : []);
  const frequencies = new Map<string, number>();
  for (const version of versions) frequencies.set(version, (frequencies.get(version) ?? 0) + 1);
  const majority = [...frequencies].find(([, count]) => count > versions.length / 2)?.[0] ?? null;
  const differing = frequencies.size > 1;
  const toolkitVersion = versions.length === TOOLKIT_PACKAGES.length && !differing ? versions[0] : null;
  const driverMajor = driverVersion === null ? null : Number(driverVersion.split(".")[0]);
  const oldVersions = [...new Set(versions.filter((version) => driverMajor !== null && driverMajor >= 615 && belowDriverFloor(version)))];
  // One shared reason prevents the security advisory and backend floor error
  // from repeating the same information four times in the dialog.
  const floorProblem: GroupProblem | null = oldVersions.length ? {
    key: "upload.err.toolkit_too_old",
    params: {version: oldVersions.join(", "), driver_version: driverVersion},
  } : null;
  for (const name of TOOLKIT_PACKAGES) {
    const version = group?.debs[name]?.version ?? null;
    const problems: GroupProblem[] = [];
    if (version !== null && differing && version !== majority) {
      problems.push(majority === null
        ? {key: "upload.group.versions_differ", params: {versions: [...frequencies.keys()]}}
        : {key: "upload.err.version_mismatch", params: {package: name, version, others: majority}});
    }
    if (version !== null && floorProblem && belowDriverFloor(version)) problems.push(floorProblem);
    slots.push({id: name, state: version === null ? "waiting" : problems.length ? "problem" : "ready", version, problems});
  }
  for (const slot of slots) {
    if (slot.state === "waiting") continue;
    // Old receipts cannot resurrect a cleared slot, overwrite current content,
    // or attach a renamed package to the wrong row.
    slot.file = [...receipts].reverse().find((file) => slot.id === "run"
      ? file.kind === "run" && file.sha256 === group?.run?.sha256
      : file.kind === "deb" && file.package === slot.id && file.version === slot.version);
  }
  const problems: GroupProblem[] = [];
  const seen = new Set<string>();
  const addProblem = (problem: GroupProblem): void => {
    const identity = JSON.stringify(problem);
    if (!seen.has(identity)) { seen.add(identity); problems.push(problem); }
  };
  for (const slot of slots) for (const problem of slot.problems) addProblem(problem);
  // These backend summaries are already represented by the derived slot state.
  const represented = new Set(["upload.group.run_missing", "upload.group.missing", "upload.group.versions_differ", "upload.group.toolkit_too_old"]);
  for (const problem of group?.problems ?? []) if (!represented.has(problem.key)) addProblem(problem);
  const warnings: GroupProblem[] = [];
  if (group?.below_security_floor && toolkitVersion !== null && driverMajor !== null && driverMajor < 615) {
    warnings.push({key: "toolkit.below_floor", params: {toolkit_version: group.toolkit_version ?? toolkitVersion}});
  }
  const ready = slots.filter((slot) => slot.state === "ready").length;
  const missing = slots.filter((slot) => slot.state === "waiting").length;
  const problem = slots.filter((slot) => slot.state === "problem").length;
  return {slots, ready, missing, problem, canContinue: ready === 5 && problems.length === 0,
    driverVersion, versionMode, toolkitVersion, problems, warnings};
}
