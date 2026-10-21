import type { UploadSlots } from "./upload-slots.js";
import { versionFormatOk } from "./rules.js";

interface ContinueFlags {
  checking: boolean;
  loading: boolean;
  readFailed: boolean;
  busy: boolean;
  blocked: boolean;
  gpuOk: boolean;
  version: string | null;
}

export function uploadContinueReason(model: UploadSlots, flags: ContinueFlags): string {
  if (flags.checking) return "upload.checking";
  if (flags.loading) return "upload.loading";
  if (flags.readFailed) return "upload.read_failed";
  if (flags.busy) return "upload.why.busy";
  if (flags.blocked) return "upload.why.blocked";
  if (!flags.gpuOk) return "upload.why.gpu";
  if (model.versionMode === "choose" && !flags.version) return "upload.why.version";
  if (model.versionMode === "manual" && !versionFormatOk(flags.version ?? "")) return "upload.why.version";
  if (model.problem) return model.missing ? "upload.why.problem" : "upload.why.problem_only";
  if (model.missing) return "upload.why.missing";
  if (model.problems.length) return "upload.why.review";
  return "upload.why.ready";
}
