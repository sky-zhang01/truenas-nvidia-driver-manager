import type { GpuReading, HomeGpuDevice, HomeResponse } from "./api.js";
import { el, expandText, queryFailed } from "./dom.js";
import type { Lang } from "./messages.js";
import { queryObjectKey } from "./rules.js";

// The unlock choice of one update; off unless the user turns it on.
export interface UnlockChoice {
  on: boolean;
}

// Catalog row label. The support table comes from the server; a release on that table is marked.
export function versionOptionText(version: string, unlockVersions: readonly string[] | undefined, lang: Lang): string {
  if (!unlockVersions?.includes(version)) return version;
  return expandText("update.version.unlock", { version }, lang);
}

// No table means an older response: the GPU list alone decides. An empty selection is not a match.
function versionAllowsUnlock(version: string | null | undefined, unlockVersions: readonly string[] | undefined): boolean {
  if (unlockVersions === undefined) return true;
  return version !== null && version !== undefined && version !== "" && unlockVersions.includes(version);
}

// One install's checkbox. An unsupported GPU selection or release turns this choice off.
// The page and an open upload dialog install different releases, so each keeps its own choice.
export function unlockChoiceNode(lang: Lang, gpus: GpuReading[], choice: UnlockChoice, idPrefix = "", onChange?: () => void, unlockVersions?: readonly string[], selectedVersion?: string | null): HTMLElement {
  const gpuOk = gpus.some((g) => g.gpu_unlock !== undefined);
  const versionOk = versionAllowsUnlock(selectedVersion, unlockVersions);
  const supported = gpuOk && versionOk;
  if (!supported) choice.on = false;
  const box = el("input", { id: `${idPrefix}gpu-unlock`, type: "checkbox", checked: choice.on, disabled: !supported });
  box.addEventListener("change", () => { choice.on = box.checked; onChange?.(); });
  const line = el("div", { class: "gpu-option" });
  line.append(box, el("label", { for: box.id, text: expandText("update.gpu_unlock", {}, lang) }));
  const area = el("div", { class: "gpu-unlock", id: `${idPrefix}gpu-unlock-area` });
  area.append(line);
  area.append(el("p", { class: "hint", text: expandText("update.gpu_unlock.range", {}, lang) }));
  if (!gpuOk && gpus.length > 0) {
    area.append(el("p", { class: "hint", text: expandText("update.gpu_unlock.none", {}, lang) }));
  } else if (unlockVersions !== undefined && gpuOk && !versionOk && selectedVersion) {
    area.append(el("p", { class: "hint", text: expandText("update.gpu_unlock.version_unsupported", {}, lang) }));
  }
  return area;
}

// The upload toggle is copied onto the page, then the page redraws against its own release.
// That redraw may turn the page choice off. The upload choice stays, and the upload plan reads it.
export function publishUploadUnlock(upload: UnlockChoice, page: UnlockChoice, redrawPage: () => void): void {
  page.on = upload.on;
  redrawPage();
}

function cardText(address: string, result: string, line: string | undefined, devices: HomeGpuDevice[], lang: Lang): string {
  const model = devices.find((d) => d.address === address)?.name ?? address;
  if (result === "not_unlocked") {
    return line === undefined
      ? expandText("home.gpu_unlock.not_run", { model }, lang)
      : expandText("home.gpu_unlock.not_unlocked", { model, line }, lang);
  }
  return expandText(`home.gpu_unlock.${result}`, { model }, lang);
}

// The home reading of a merged image with the unlock rule: one line per target GPU, named by its
// model when the GPU list has it; UNKNOWN with the failed query when it cannot be read. Nothing when
// the merged image has no rule.
export function unlockReadingNode(home: HomeResponse, lang: Lang): HTMLElement | null {
  const reading = home.gpu_unlock;
  if (reading === null || reading === undefined) return null;
  const value = el("div", { class: "gpu-readout" });
  if (reading.unknown !== undefined) {
    value.append(el("p", { class: "reading-value", text: "UNKNOWN" }), queryFailed(queryObjectKey("gpu_unlock"), reading.unknown, null, lang));
  } else {
    const devices = home.gpus.value?.devices ?? [];
    for (const card of reading.value.cards) {
      value.append(el("p", { class: "reading-value", text: cardText(card.address, card.result, card.line, devices, lang) }));
    }
    if (reading.value.reloaded_after_boot) {
      value.append(el("p", { class: "hint", text: expandText("home.gpu_unlock.reloaded", {}, lang) }));
    }
  }
  const row = el("div", { class: "reading", id: "home-gpu-unlock" });
  row.append(el("span", { class: "reading-label", text: expandText("home.gpu_unlock", {}, lang) }), value);
  return row;
}
