const FILE_KEPT = new Set([
  "read_loaded_version",
  "save_original",
  "download",
  "unlock_firmware",
  "unlock_preload",
  "build",
]);

// After module unload, an interrupted task may have replaced the file without reloading modules.
const MODULES_UNLOADED = new Set([
  "write",
  "refresh",
  "reload",
  "restore_unload",
  "restore_write",
  "restore_refresh",
  "restore_reload",
]);

export interface StepPlan {
  source?: { kind?: string } | null;
  original?: string | null;
  unlock?: boolean;
}

// Before these phases the task has not test-loaded the unlock module.
const BEFORE_UNLOCK_PRELOAD = new Set(["read_loaded_version", "save_original", "download", "unlock_firmware", "build"]);

// An interrupted update with the unlock may leave the idle module it test-loaded until the next boot;
// an unknown phase cannot rule that out.
export function interruptUnlockNote(phase: string | null, unlock: boolean): boolean {
  return unlock && !BEFORE_UNLOCK_PRELOAD.has(phase ?? "");
}

// A missing phase cannot establish whether writing started; later phases cannot promise an unchanged
// driver file.
export function interruptCopyKey(phase: string | null): string {
  if (phase === null || phase === "") return "home.interrupted.phase_unknown";
  if (phase === "align_modules" || phase === "restore_align_modules") return "home.interrupted.file_written";
  if (FILE_KEPT.has(phase)) return "home.interrupted.file_kept";
  if (phase === "unload") return "home.interrupted.unload";
  if (MODULES_UNLOADED.has(phase)) return "home.interrupted.modules";
  return "home.interrupted";
}

// Restore steps appear only after restoration starts; the final version read remains last.
export function plannedSteps(plan: StepPlan | null, restoring: boolean, correction: { update?: boolean; restore?: boolean } = {}): string[] {
  if (!plan) return [];
  const kind = plan.source?.kind ?? null;
  const steps = ["read_loaded_version"];
  if (plan.original === "save") steps.push("save_original");
  if (kind === "official") steps.push("download");
  const builds = kind === "official" || kind === "upload";
  if (builds && plan.unlock) steps.push("unlock_firmware");
  if (builds) steps.push("build");
  if (plan.unlock) steps.push("unlock_preload");
  steps.push("unload", "write", "refresh", "reload");
  if (correction.update) steps.push("align_modules");
  steps.push("check");
  if (restoring) {
    steps.push(
      "restore_unload",
      "restore_write",
      "restore_refresh",
      "restore_reload",
    );
    if (correction.restore) steps.push("restore_align_modules");
    steps.push("restore_check");
  }
  steps.push("read_loaded_version_end");
  return steps;
}
