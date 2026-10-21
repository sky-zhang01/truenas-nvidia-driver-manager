// Selection changes recompute GPU compatibility locally; the server independently checks it when
// creating a plan.

import {
  ApiError,
  getVersions,
  getUploadToolkitVersion,
  recoverUploadPlan,
  cancelPlan,
  getCurrentUpload,
  openUploadGroup,
  putUploadFile,
  type PlanSource,
  type GroupSummary,
  type GpuReading,
  type UploadFileInfo,
  type VersionsResponse,
} from "../api.js";
import {
  el,
  externalLink,
  clear,
  expandText,
  formatSize,
  queryFailed,
  renderNotice,
  apiErrorNotice,
  type Params,
} from "../dom.js";
import type { App } from "../main.js";
import { message } from "../messages.js";
import { conflict, fileTooLarge, filterVersions, gpuLabel, queryObjectKey, versionFormatOk } from "../rules.js";
import { deriveUploadSlots, rejectionSlot, sortUploadFiles, uploadRejectionKey } from "../upload-slots.js";
import { closeDialog, openDialog, makeDialog } from "../dialog.js";
import { armUpdateCheckStatus, openCheckDialog, openConfirmDialog, renderPlanError } from "./plan.js";
import { runPlanFlow } from "../plan-flow.js";
import { guardSignedIn } from "../session-view.js";
import { uploadContinueReason } from "../upload-continue.js";

// A failed login-status read cannot authorize writes using an earlier result.
export function loginReadFailed(app: App): boolean {
  return app.loginError !== null;
}

export function updateBlockedNode(app: App): HTMLElement {
  const lang = app.lang;
  const box = el("div", { class: "update-blocked", id: "update-blocked" });
  if (loginReadFailed(app)) return box;
  for (const item of app.login.update_blocked ?? []) {
    if (item.key === "setup") {
      box.append(renderNotice("update.disabled_setup", {}, lang));
    } else if (item.key === "extension_missing") {
      box.append(renderNotice("extension.missing", {}, lang));
    } else if (item.key === "task_running") {
      box.append(renderNotice("update.disabled_task_running_text", {}, lang));

    } else if (item.key === "task_state_unreadable") {
      box.append(
        queryFailed(
          "query.object.running_task",
          item.reason ?? { key: "reason.io_error", params: {} },
          null,
          lang,
        ),
      );
    }
  }
  return box;
}

function checkpoint(app: App): HTMLElement {
  return updateBlockedNode(app);
}

function countedGpus(v: VersionsResponse): GpuReading[] {
  return v.gpus ?? [];
}

function gpuSelection(
  app: App,
  gpus: GpuReading[],
  selected: Set<string>,
  onChange: () => void,
  idPrefix = "",
): HTMLElement | null {
  if (gpus.length < 2) return null;
  const lang = app.lang;
  const fs = el("fieldset", { class: "gpu-select", id: `${idPrefix}gpu-select` });
  const legend = el("legend", { text: expandText("update.gpu_select", {}, lang) });
  fs.append(legend);
  for (const g of gpus) {
    const box = el("input", {
      id: `${idPrefix}gpu-${g.address}`,
      type: "checkbox",
      checked: selected.has(g.address),
    });
    box.addEventListener("change", () => {
      if (box.checked) selected.add(g.address);
      else selected.delete(g.address);
      onChange();
    });
    const label = el("label", {
      for: `${idPrefix}gpu-${g.address}`,
      text: expandText("update.gpu_option", { gpu_name: gpuLabel(g), pci_address: g.address }, lang),
    });
    const line = el("div", { class: "gpu-option" });
    line.append(box, label);
    fs.append(line);
  }
  return fs;
}

function notesNode(app: App, notes: { gpu: GpuReading; kind: "last" | "from"; branch: number }[]): HTMLElement {
  const lang = app.lang;
  const box = el("div", { class: "gpu-notes" });
  for (const n of notes) {
    box.append(
      renderNotice(
        "catalog.gpu.not_listed",
        { gpu_name: gpuLabel(n.gpu), branch: n.branch },
        lang,
        n.kind === "from" ? { variant: "from" } : undefined,
      ),
    );
  }
  return box;
}

export function renderUpdate(app: App, container: HTMLElement): void {
  const lang = app.lang;
  void (async () => {
    let versions: VersionsResponse;
    try {
      versions = await getVersions();
    } catch (err) {
      // Late responses cannot update a departed page or end a subsequent session.
      if (!container.isConnected) return;
      if (err instanceof ApiError && err.isSessionEnded) {
        app.sessionEnded();
        return;
      }

      container.append(apiErrorNotice(err, lang));
      return;
    }
    if (!await guardSignedIn(app, container)) return;

    let gpus = countedGpus(versions);
    const selected = new Set<string>();
    if (gpus.length === 1 && gpus[0]) selected.add(gpus[0].address);
    let chosen: string | null = null;
    let planInFlight = false;

    container.append(el("h2", { text: expandText("nav.update", {}, lang) }));
    let blocked = checkpoint(app);
    container.append(blocked);

    const errorBox = el("div", { class: "update-error" });
    const describe = el("div", { class: "catalog-describe", id: "catalog-describe" });
    const selectArea = el("div", { class: "catalog-area" });
    const gpuArea = el("div", { class: "gpu-area" });

    const updateBtn = el("button", {
      type: "button",
      class: "btn-primary",
      text: expandText("update.button", {}, lang),
    });
    const uploadBtn = el("button", {
      type: "button",
      text: expandText("upload.entry.button", {}, lang),
    });
    const link = externalLink("https://www.nvidia.com/en-us/drivers/unix/#linux64", expandText("upload.official", {}, lang), lang);
    const buttonArea = el("div", { class: "actions" });
    buttonArea.append(updateBtn, uploadBtn, link);

    container.append(gpuArea, selectArea, describe, buttonArea, errorBox);
    container.append(el("p", { class: "hint upload-entry-hint", text: expandText("upload.entry.hint", {}, lang) }));

    const selectedGpus = (): GpuReading[] => gpus.filter((g) => selected.has(g.address));

    const rebuild = (): void => {
      // Rebuilding GPU checkboxes must restore focus to the control just operated.
      const activeId =
        document.activeElement instanceof HTMLElement ? document.activeElement.id : "";
      clear(gpuArea);
      const box = gpuSelection(app, gpus, selected, rebuild);
      if (box) gpuArea.append(box);
      if (activeId !== "") document.getElementById(activeId)?.focus();

      clear(selectArea);
      const label = el("label", { for: "version-select", text: expandText("update.catalog_label", {}, lang) });
      const select = el("select", { id: "version-select" });
      select.setAttribute("aria-describedby", "catalog-describe");
      selectArea.append(label, select);

      const filtered = filterVersions(
        versions.items.map((i) => i.version),
        versions.open_unbuildable,
        versions.unreadable ?? [],
        gpus,
        [...selected],
      );
      for (const v of filtered.options) {
        select.append(el("option", { value: v, text: v }));
      }
      if (chosen !== null && filtered.options.includes(chosen)) {
        select.value = chosen;
      } else {
        select.selectedIndex = -1;
        chosen = null;
      }
      select.addEventListener("change", () => {
        chosen = select.value === "" ? null : select.value;
        refreshButton();
      });

      clear(describe);

      const gpuCount = selectedGpus().length;
      const conclusion = filtered.noUsableVersion
        ? renderNotice("catalog.no_usable_version", {}, lang, {
            class: "conclusion",
            variant: gpuCount === 1 ? "one" : undefined,
          })
        : null;
      describe.append(el("p", { class: "hint", text: expandText("catalog.help", {}, lang) }));
      describe.append(notesNode(app, filtered.notes));
      if (conclusion) {
        conclusion.id = "catalog-conclusion";
        describe.append(conclusion);
      }
      for (const u of versions.unreadable ?? []) {
        describe.append(queryFailed(queryObjectKey(u.object), u.reason, u.raw ?? null, lang));
      }
      refreshButton();
    };

    const refreshButton = (): void => {
      const filtered = filterVersions(
        versions.items.map((i) => i.version),
        versions.open_unbuildable,
        versions.unreadable ?? [],
        gpus,
        [...selected],
      );
      const blockedCount = (app.login.update_blocked ?? []).length;
      const readFailed = loginReadFailed(app);
      const noneSelected = filtered.noneSelected;
      const conflictNow = conflict(selectedGpus());
      const disabled =
        planInFlight ||
        readFailed ||
        blockedCount > 0 ||
        conflictNow ||
        filtered.noUsableVersion ||
        noneSelected ||
        chosen === null;
      updateBtn.disabled = disabled;
      uploadBtn.disabled = readFailed;
      updateBtn.removeAttribute("title");
      updateBtn.removeAttribute("aria-describedby");
      uploadBtn.removeAttribute("aria-describedby");
      const reasons = el("span", { class: "hint" });
      if (readFailed) {
        updateBtn.setAttribute("aria-describedby", "login-read-error");
        uploadBtn.setAttribute("aria-describedby", "login-read-error");
      } else if (blockedCount > 0 || conflictNow || filtered.noUsableVersion || noneSelected) {

        updateBtn.setAttribute(
          "aria-describedby",
          conflictNow || filtered.noUsableVersion
            ? "catalog-conclusion"
            : noneSelected
              ? "gpu-select"
              : "update-blocked",
        );
      } else if (chosen === null) {
        reasons.textContent = expandText("update.disabled_no_version", {}, lang);
      }
      const old = buttonArea.querySelector(".button-reason");
      if (old) old.remove();
      reasons.classList.add("button-reason");
      buttonArea.append(reasons);
    };

    const redrawGate = (): void => {
      const fresh = checkpoint(app);
      blocked.replaceWith(fresh);
      blocked = fresh;
      refreshButton();
    };

    rebuild();

    let activePlan: AbortController | null = null;
    const runPlan = (source: PlanSource): void => {
      if (loginReadFailed(app) || planInFlight) return;
      // Ignore double clicks while the first plan request is in flight.
      planInFlight = true;
      updateBtn.disabled = true;
      clear(errorBox);
      const controller = new AbortController();
      activePlan = controller;
      const ownsPlan = (): boolean => !controller.signal.aborted && activePlan === controller && container.isConnected;
      const cancel = (): void => {
        if (activePlan !== controller) return;
        controller.abort(); stopCheckStatus();
        activePlan = null; planInFlight = false;
        if (container.isConnected) { refreshButton(); updateBtn.focus(); }
      };
      const stopCheckStatus = armUpdateCheckStatus(updateBtn, lang, app, cancel);
      app.addCleanup(() => controller.abort());
      void runPlanFlow({
        source, signal: controller.signal, owns: ownsPlan,
        ready: (res) => openConfirmDialog(app, res.plan_id, res.dialog, updateBtn),
        blocked: (res) => openCheckDialog(app, res.failures ?? [], [], res.gpu, updateBtn, () => runPlan(source)),
        uncomputable: (res) => openCheckDialog(app, [], res.items ?? [], null, updateBtn),
        failed: async (err) => {
          if (err instanceof ApiError) {
            renderPlanError(app, errorBox, err, true);
            if (err.key === "plan.changed") {
              // A rejected plan may mean the GPU set changed; refresh versions and rebuild the
              // selection while reporting any read failure.
              try {
                const freshVersions = await getVersions();
                if (!ownsPlan()) return;
                versions = freshVersions;
                await app.refreshLogin(ownsPlan);
                if (!ownsPlan()) return;
                if (!app.signedIn) {
                  app.render();
                  return;
                }
                gpus = countedGpus(versions);
                selected.clear();
                if (gpus.length === 1 && gpus[0]) selected.add(gpus[0].address);
                rebuild();
                redrawGate();
              } catch (refreshErr) {
                if (!ownsPlan()) return;
                if (refreshErr instanceof ApiError && refreshErr.isSessionEnded) {
                  app.sessionEnded();
                  return;
                }
                if (loginReadFailed(app)) {
                  const readError = el("div", { id: "login-read-error" });
                  readError.append(apiErrorNotice(refreshErr, lang));
                  errorBox.append(readError);
                  redrawGate();
                } else {
                  errorBox.append(apiErrorNotice(refreshErr, lang));
                }
              }
            }
          } else {
            errorBox.append(apiErrorNotice(err, lang));
          }
        },
        finish: () => {
          stopCheckStatus();
          if (activePlan === controller) {
            activePlan = null; planInFlight = false;
            if (container.isConnected) refreshButton();
          }
        },
      });
    };
    updateBtn.addEventListener("click", () => {
      if (chosen !== null) runPlan({ kind: "official", version: chosen, gpus: [...selected] });
    });

    uploadBtn.addEventListener("click", () => {
      if (loginReadFailed(app)) return;
      openUploadDialog(app, versions, selected, uploadBtn, () => {
        rebuild();
      }, chosen);
    });
  })();
}

interface UploadState {
  group: GroupSummary | null;
  version: string | null;
  receiptGroup: string | null;
  received: Map<string, UploadFileInfo>;
  readGen: number;
  busy: boolean;
}

const uploadManualVersion: { value: string | null } = { value: null };

function openUploadDialog(
  app: App,
  versions: VersionsResponse,
  selected: Set<string>,
  trigger: HTMLElement,
  onSelectionChange: () => void,
  initialDownloadVersion: string | null,
): void {
  let lang = app.lang;
  let gpus = countedGpus(versions);
  const state: UploadState = { group: null, version: null, receiptGroup: null, received: new Map(), readGen: 0, busy: false };
  let loading = true;
  let groupReadFailed = false;
  let checking = false;
  let toolkitVersion: string | null = null;
  let toolkitFallback = false;
  let downloadVersion = initialDownloadVersion ?? "";
  let uploadAbort: AbortController | null = null;
  let batchIndex = 0;
  let batchTotal = 0;
  let currentFile: File | null = null;
  let sent = 0;
  let reading = false;
  let announcement = "";
  type Rejection = { file: string; key: string; params: Params; slot: string | null };
  const rejections: Rejection[] = [];
  const { dialog: dlg, title, show } = makeDialog({ title: "", className: "nvm-dialog upload-dialog", trigger });
  dlg.setAttribute("aria-modal", "true");
  const x = el("button", { type: "button", class: "upload-dismiss", text: "×" });
  const body = el("div", { class: "upload-body" });
  const footer = el("footer", { class: "upload-footer" });
  const lead = el("p", { class: "hint" });
  const gpuArea = el("div", { class: "gpu-area" });
  const conclusionBox = el("div");
  const versionControl = el("div", { class: "upload-download-version" });
  const drop = el("div", { class: "upload-drop" });
  const files = el("input", { id: "upload-files", type: "file", accept: ".run,.deb", multiple: true, class: "visually-hidden" });
  files.tabIndex = -1; files.setAttribute("aria-label", expandText("upload.files_label", {}, lang));
  const choose = el("button", { type: "button" });
  const dropText = el("span");
  const extension = el("p", { class: "hint" });
  drop.append(dropText, choose, files);
  const progressBox = el("div", { class: "upload-progress" });
  const summaryBox = el("div", { class: "upload-summary" });
  const unusedBox = el("div", { class: "upload-unused" });
  const how = el("details", { class: "upload-how" });
  const howTitle = el("summary");
  const howBody = el("p");
  const howTrust = el("p");
  how.append(howTitle, howBody, howTrust);
  const errorBox = el("div", { class: "dialog-error" });
  const counter = el("p", { class: "upload-counter" });
  const live = el("span", { class: "visually-hidden" });
  live.setAttribute("aria-live", "polite"); live.setAttribute("aria-atomic", "true");
  const reason = el("p", { class: "hint", id: "upload-continue-reason" });
  const cancelConfirm = el("div", { class: "upload-cancel-confirm", role: "group", hidden: true });
  const cancelMessage = el("p", { id: "upload-cancel-message" });
  cancelConfirm.setAttribute("aria-labelledby", cancelMessage.id);
  const keepUploading = el("button", { type: "button" });
  const stopUploading = el("button", { type: "button", class: "btn-danger" });
  cancelConfirm.append(cancelMessage, el("div", { class: "dialog-buttons" }, [keepUploading, stopUploading]));
  const buttons = el("div", { class: "dialog-buttons" });
  const restartBtn = el("button", { type: "button", class: "upload-restart" });
  const closeBtn = el("button", { type: "button" });
  const updateBtn = el("button", { type: "button", class: "btn-primary" });
  updateBtn.setAttribute("aria-describedby", reason.id); reason.tabIndex = -1;
  const actions = el("div", { class: "upload-footer-actions" });
  actions.append(closeBtn, updateBtn);
  buttons.append(restartBtn, actions);
  footer.append(errorBox, counter, live, reason, cancelConfirm, buttons);
  body.append(lead, gpuArea, conclusionBox, versionControl, drop, extension, progressBox, summaryBox, unusedBox, how);
  dlg.append(x, body, footer);
  const text = (key: string, params: Params = {}): string => expandText(key, params, lang);
  const selectedGpus = (): GpuReading[] => gpus.filter((g) => selected.has(g.address));
  const model = () => deriveUploadSlots(state.group, state.version, [...state.received.values()]);
  const adopt = (group: GroupSummary | null): void => {
    if (state.group?.run?.sha256 !== group?.run?.sha256) state.version = null;
    if ((group?.group_id ?? null) !== state.receiptGroup) {
      state.received.clear();
      state.version = null;
    }
    state.receiptGroup = group?.group_id ?? null;
    state.group = group;
    const candidates = group?.run?.candidates ?? [];
    if (candidates.length === 1) state.version = candidates[0] ?? null;
    else if (candidates.length > 1 && !candidates.includes(state.version ?? "")) state.version = null;
  };
  // Keep announcements mounted when the completed checklist no longer needs a counter.
  const silenceNotices = (): void => {
    for (const node of errorBox.querySelectorAll('[role="alert"], [role="status"]')) node.removeAttribute("role");
  };
  const showError = (err: unknown): void => {
    if (!dlg.isConnected) return;
    if (err instanceof ApiError && err.isSessionEnded) { app.sessionEnded(); return; }
    errorBox.append(apiErrorNotice(err, lang)); silenceNotices();
    announcement = text("upload.action_failed"); refreshFooter();
  };
  const refreshFooter = (): void => {
    const m = model();
    const blocked = loginReadFailed(app) || (app.login.update_blocked ?? []).length > 0;
    const gpuOk = (gpus.length < 2 || selected.size > 0) && !conflict(selectedGpus());
    const allowed = m.canContinue && gpuOk && !blocked && !loading && !groupReadFailed && !checking && !state.busy;
    updateBtn.setAttribute("aria-disabled", String(!allowed));
    updateBtn.textContent = text("upload.update");
    counter.textContent = text("upload.counter", { n: m.ready });
    counter.hidden = m.canContinue;
    live.textContent = announcement;
    reason.textContent = text(uploadContinueReason(m, {
      checking, loading, readFailed: groupReadFailed, busy: state.busy, blocked, gpuOk, version: state.version,
    }), { n: m.problem || m.missing, m: m.missing });
    restartBtn.hidden = !state.group && rejections.length === 0;
    restartBtn.disabled = state.busy || checking || loading;
    closeBtn.textContent = text(state.busy ? "upload.cancel" : "dialog.close");
    x.setAttribute("aria-label", closeBtn.textContent);
    closeBtn.disabled = checking;
    x.disabled = checking;
    files.disabled = state.busy || checking || loading || groupReadFailed;
    choose.disabled = files.disabled;
  };
  const rebuildGpu = (): void => {
    const active = document.activeElement instanceof HTMLElement ? document.activeElement.id : "";
    clear(gpuArea); clear(conclusionBox);
    const group = gpuSelection(app, gpus, selected, () => { rebuildGpu(); refreshFooter(); onSelectionChange(); }, "upload-");
    if (group) gpuArea.append(group);
    if (conflict(selectedGpus())) conclusionBox.append(renderNotice("catalog.no_usable_version", {}, lang, { role: "none", class: "conclusion" }));
    if (active) document.getElementById(active)?.focus();
  };
  const rebuildLinks = (): void => {
    const value = versionFormatOk(downloadVersion) ? downloadVersion : null;
    const link = summaryBox.querySelector<HTMLAnchorElement>(".upload-run-download");
    const filename = summaryBox.querySelector<HTMLElement>(".upload-run-filename");
    if (link) {
      const replacement = externalLink(value ? `https://download.nvidia.com/XFree86/Linux-x86_64/${value}/NVIDIA-Linux-x86_64-${value}-no-compat32.run` : "https://download.nvidia.com/XFree86/Linux-x86_64/", text(value ? "upload.download.run" : "upload.download.directory"), lang);
      replacement.classList.add("upload-run-download"); link.replaceWith(replacement);
    }
    if (filename) filename.textContent = `NVIDIA-Linux-x86_64-${value ?? `<${text("upload.version.placeholder")}>`}-no-compat32.run`;
  };
  const rebuildDownloadVersion = (): void => {
    clear(versionControl);
    const label = el("label", { for: "upload-link-version", text: text("upload.version_label") });
    let input: HTMLInputElement | HTMLSelectElement;
    if (versions.items.length > 0) {
      input = el("select", { id: "upload-link-version" });
      input.append(el("option", { value: "", text: text("upload.version.placeholder") }));
      for (const v of versions.items) input.append(el("option", { value: v.version, text: v.version }));
      input.value = downloadVersion;
    } else input = el("input", { id: "upload-link-version", type: "text", value: downloadVersion });
    input.addEventListener("input", () => {
      downloadVersion = input.value;
      const invalid = input.value !== "" && !versionFormatOk(input.value);
      input.setAttribute("aria-invalid", String(invalid));
      hint.textContent = text("upload.version.link_hint") + (invalid ? ` ${text("upload.version.invalid")}` : "");
      rebuildLinks(); rebuildMismatch();
    });
    input.setAttribute("aria-describedby", "upload-link-hint");
    const hint = el("p", { class: "hint", id: "upload-link-hint", text: text("upload.version.link_hint") });
    versionControl.append(label, input, hint);
  };
  const rebuildMismatch = (): void => {
    summaryBox.querySelector(".upload-version-mismatch")?.remove();
    const current = model();
    const v = current.driverVersion;
    if (current.versionMode !== "manual" && v && versionFormatOk(downloadVersion) && v !== downloadVersion) summaryBox.prepend(el("p", { class: "hint upload-version-mismatch", text: text("upload.version.mismatch", { version: v, selected: downloadVersion }) }));
  };
  const chooseAgain = (): HTMLButtonElement => {
    const b = el("button", { type: "button", text: text("upload.replace"), disabled: state.busy || checking || groupReadFailed });
    b.addEventListener("click", () => files.click()); return b;
  };
  const rebuildSummary = (): void => {
    const focusId = document.activeElement instanceof HTMLElement && summaryBox.contains(document.activeElement) ? document.activeElement.id : "";
    clear(summaryBox); clear(unusedBox);
    if (state.group?.run?.candidates.length === 0 && state.version === null && uploadManualVersion.value) state.version = uploadManualVersion.value;
    const m = model();
    for (const node of [lead, versionControl, drop, extension]) node.hidden = m.canContinue;
    if (m.canContinue) summaryBox.append(el("p", { class: "upload-complete", text: text(m.versionMode === "manual" ? "upload.complete.manual" : "upload.complete", { version: m.driverVersion, toolkit: m.toolkitVersion }) }));
    const list = el("ol", { class: "upload-slots" });
    list.setAttribute("aria-label", text("upload.checklist"));
    const shownProblems = new Set<string>();
    for (const [index, slot] of m.slots.entries()) {
      if (index === 0) summaryBox.append(el("h4", { text: text("upload.step.run") }));
      const uploading = state.busy && slot.id === "run" && currentFile !== null && /\.run$/i.test(currentFile.name);
      const li = el("li", { id: `upload-slot-${slot.id}`, class: `upload-slot upload-slot-${uploading ? "uploading" : slot.state}` });
      if (index === 1) {
        li.append(el("h4", { text: text("upload.step.toolkit") }));
        if (!m.canContinue) li.append(el("p", { class: "hint", text: text(toolkitFallback ? "upload.toolkit.fallback" : "upload.toolkit.lead", { pinned: toolkitVersion ?? text("upload.loading") }) }));
      }
      li.tabIndex = -1;
      const row = el("div", { class: "upload-slot-row" });
      const symbol = el("span", { class: "upload-slot-symbol", text: uploading ? "⟳" : slot.state === "ready" ? "✓" : slot.state === "problem" ? "!" : "○" });
      symbol.setAttribute("aria-hidden", "true"); row.append(symbol);
      row.append(el("strong", { text: slot.id === "run" ? text("upload.run.label") : slot.id }));
      row.append(el("span", { class: "upload-slot-status", text: text(uploading ? "upload.slot.uploading" : `upload.slot.${slot.state}`, { version: slot.version ?? "" }) }));
      if (slot.state !== "waiting") row.append(chooseAgain());
      li.append(row);
      if (slot.file) li.append(el("p", { class: "hint upload-receipt", text: text("upload.receipt", { file_name: slot.file.client_name, size: formatSize(slot.file.size) }) }));
      if (slot.id === "run") {
        const downloadHelp = el("div", { hidden: slot.state === "ready" });
        const filename = el("code", { class: "upload-run-filename" });
        const want = el("p", { class: "hint upload-file-want" }); want.append(document.createTextNode(text("upload.run.want") + " "), filename);
        const why = el("div", { class: "upload-run-why" }); why.append(el("p", { class: "hint", text: text("upload.run.why") }));
        downloadHelp.append(want, why);
        const link = externalLink("https://download.nvidia.com/XFree86/Linux-x86_64/", text("upload.download.directory"), lang);
        link.classList.add("upload-run-download"); why.append(link);
        const skip = el("details"); skip.append(el("summary", { text: text("upload.run.skip") }), el("p", { text: text("upload.run.skip.body") })); downloadHelp.append(skip); li.append(downloadHelp);
        const candidates = state.group?.run?.candidates ?? [];
        if (m.versionMode === "choose") {
          const fs = el("fieldset"); fs.append(el("legend", { text: text("upload.version.choose") }));
          for (const c of candidates) {
            const radio = el("input", { id: `version-${c}`, type: "radio", name: "upload-version", value: c, checked: state.version === c });
            radio.addEventListener("change", () => { state.version = c; rebuildSummary(); document.getElementById(`version-${c}`)?.focus(); });
            fs.append(el("div", { class: "version-option" }, [radio, el("label", { for: radio.id, text: c })]));
          }
          li.append(fs);
        } else if (m.versionMode === "manual") {
          const input = el("input", { id: "upload-version-input", type: "text", value: state.version ?? "" });
          const invalid = el("p", { id: "upload-version-invalid", class: "hint" });
          input.setAttribute("aria-describedby", invalid.id);
          invalid.textContent = input.value && !versionFormatOk(input.value) ? text("upload.version.invalid") : "";
          input.setAttribute("aria-invalid", String(!!invalid.textContent));
          input.addEventListener("input", () => {
            const start = input.selectionStart; const end = input.selectionEnd;
            state.version = input.value || null; uploadManualVersion.value = state.version;
            rebuildSummary();
            const replacement = document.getElementById(input.id);
            if (replacement instanceof HTMLInputElement && start !== null && end !== null) replacement.setSelectionRange(start, end);
          });
          li.append(el("label", { for: input.id, text: text("upload.version.enter") }), input, invalid);
        }
      } else if (toolkitVersion && !m.canContinue) {
        row.append(externalLink(`https://nvidia.github.io/libnvidia-container/stable/deb/amd64/${slot.id}_${toolkitVersion}_amd64.deb`, text("upload.download.toolkit", { version: toolkitVersion }), lang));
      }
      for (const p of slot.problems) {
        const identity = JSON.stringify(p);
        if (shownProblems.has(identity)) continue;
        shownProblems.add(identity);
        if (slot.id === "run" && ["upload.version.choose", "upload.version.enter"].includes(p.key)) continue;
        li.append(el("p", { class: "upload-slot-error", text: text(p.key, { ...p.params, pinned: toolkitVersion ?? "1.20.1-1" }) }));
      }
      for (const r of rejections.filter((r) => r.slot === slot.id)) li.append(el("p", { class: "upload-slot-error", text: text(r.key, r.params) }), chooseAgain());
      list.append(li);
    }
    summaryBox.append(list);
    for (const p of m.problems) if (!shownProblems.has(JSON.stringify(p))) summaryBox.append(el("p", { class: "upload-slot-error", text: text(p.key, p.params ?? {}) }));
    for (const p of m.warnings) summaryBox.append(el("p", { class: "hint", text: text(p.key, p.params ?? {}) }));
    const unused = rejections.filter((r) => r.slot === null);
    if (unused.length) {
      unusedBox.append(el("h4", { text: text("upload.unused", { n: unused.length }) }));
      for (const r of unused) {
        const row = el("div", { class: "upload-unused-file" });
        const dismiss = el("button", { type: "button", text: text("upload.unused.remove") });
        dismiss.addEventListener("click", () => { rejections.splice(rejections.indexOf(r), 1); rebuildSummary(); });
        const namesFile = message(r.key)?.[lang].includes("{file_name}") && r.params["file_name"] === r.file;
        if (!namesFile) row.append(el("strong", { text: r.file }));
        row.append(el("p", { text: text(r.key, r.params) }), dismiss); unusedBox.append(row);
      }
    }
    rebuildLinks(); rebuildMismatch(); refreshFooter();
    if (focusId) document.getElementById(focusId)?.focus();
  };
  const rebuildProgress = (): void => {
    clear(progressBox);
    if (!state.busy || !currentFile) return;
    progressBox.append(el("p", { text: text("upload.progress.overall", { n: batchIndex, total: batchTotal, file_name: currentFile.name }) }));
    const progress = el("progress", { max: String(currentFile.size || 1) });
    progress.setAttribute("aria-label", text("upload.progress.label"));
    if (!reading) progress.value = sent;
    progressBox.append(progress, el("p", { class: "hint", text: reading ? text("upload.reading") : text("upload.progress.bytes", { done: formatSize(sent), total: formatSize(currentFile.size) }) }));
    if (currentFile.size > 10 * 1024 * 1024) progressBox.append(el("p", { class: "hint", text: text("upload.big_file") }));
  };
  const loadGroup = async (): Promise<void> => {
    loading = true; refreshFooter();
    const gen = ++state.readGen;
    try {
      const group = await getCurrentUpload();
      if (gen !== state.readGen || !dlg.isConnected) return;
      adopt(group); groupReadFailed = false;
    } catch (err) {
      if (gen !== state.readGen || !dlg.isConnected) return;
      groupReadFailed = true; showError(err);
      const retry = el("button", { type: "button", class: "upload-read-retry", text: text("upload.read_retry") });
      retry.addEventListener("click", () => { loading = true; clear(errorBox); refreshFooter(); void loadGroup(); });
      errorBox.append(retry);
    } finally { if (gen === state.readGen && dlg.isConnected) { loading = false; rebuildSummary(); } }
  };
  const reject = (file: File, key: string, params: Params, slot: string | null): void => {
    const old = rejections.findIndex((r) => r.file === file.name); if (old >= 0) rejections.splice(old, 1);
    announcement = text(key, { ...params, file_name: file.name });
    rejections.push({ file: file.name, key, params: { ...params, file_name: typeof params["file_name"] === "string" ? params["file_name"] : file.name }, slot });
  };
  const receiveFiles = async (picked: File[]): Promise<void> => {
    if (state.busy || checking || loading || groupReadFailed || !picked.length) return;
    clear(errorBox); ++state.readGen; state.busy = true; uploadAbort = new AbortController();
    const controller = uploadAbort;
    refreshFooter(); batchTotal = picked.length;
    try {
      let groupId = state.group?.group_id;
      if (!groupId) { const opened = await openUploadGroup(); groupId = opened.group_id; adopt(null); state.receiptGroup = groupId; }
      for (const [i, file] of sortUploadFiles(picked).entries()) {
        if (controller.signal.aborted || !dlg.isConnected) break;
        batchIndex = i + 1; currentFile = file; sent = 0; reading = false; rebuildSummary(); rebuildProgress();
        if (fileTooLarge(file.size)) { reject(file, "upload.err.too_large", {}, null); rebuildSummary(); continue; }
        try {
          const res = await putUploadFile(groupId, file.name, file, (done) => { sent = done; reading = done >= file.size; rebuildProgress(); }, controller.signal);
          if (!dlg.isConnected) break;
          if (res.group.group_id !== groupId || res.file.client_name !== file.name) throw new Error("Unexpected upload receipt");
          const oldRun = state.group?.run?.sha256;
          adopt(res.group);
          if (res.file.kind === "run" && oldRun !== res.group.run?.sha256) state.version = res.file.candidates?.length === 1 ? res.file.candidates[0] ?? null : null;
          const id = res.file.kind === "run" ? "run" : res.file.package ?? file.name;
          state.received.set(id, res.file);
          announcement = text("upload.file_ready", { file_name: res.file.client_name });
          for (let r = rejections.length - 1; r >= 0; r--) if (rejections[r]?.slot === id || rejections[r]?.file === file.name) rejections.splice(r, 1);
        } catch (err) {
          if (!dlg.isConnected) break;
          if (err instanceof ApiError) {
            if (err.isSessionEnded) { app.sessionEnded(); break; }
            const reasonKey = String(err.params["reason"] ?? "");
            const key = err.status === 0 ? "upload.err.interrupted" : err.key === "upload.rejected" ? uploadRejectionKey(reasonKey) : err.key;
            reject(file, key, err.params, rejectionSlot(err.params) ?? (reasonKey.startsWith("run_") ? "run" : null));
            if (err.key === "upload.group_replaced") break;
          } else showError(err);
        }
        rebuildSummary();
      }
    } catch (err) { if (dlg.isConnected) showError(err); }
    finally {
      if (dlg.isConnected) await loadGroup();
      state.busy = false; uploadAbort = null; currentFile = null; files.value = ""; rebuildProgress();
      if (dlg.isConnected) { rebuildSummary(); refreshFooter(); if (!cancelConfirm.hidden) resumeUpload(); }
    }
  };
  choose.addEventListener("click", () => files.click());
  files.addEventListener("change", () => void receiveFiles(Array.from(files.files ?? [])));
  drop.addEventListener("dragover", (event) => { event.preventDefault(); if (!files.disabled) drop.classList.add("dragover"); });
  drop.addEventListener("dragleave", () => drop.classList.remove("dragover"));
  drop.addEventListener("drop", (event) => { event.preventDefault(); drop.classList.remove("dragover"); if (event.dataTransfer) void receiveFiles(Array.from(event.dataTransfer.files)); });
  let cancelTrigger: HTMLElement | null = null;
  const resumeUpload = (): void => {
    cancelConfirm.hidden = true;
    cancelTrigger?.focus();
  };
  const dismiss = (trigger: HTMLElement = closeBtn): void => {
    if (checking) return;
    if (state.busy) {
      cancelTrigger = trigger;
      cancelConfirm.hidden = false;
      keepUploading.focus();
      return;
    }
    closeDialog(dlg); dlg.remove();
  };
  keepUploading.addEventListener("click", resumeUpload);
  stopUploading.addEventListener("click", () => {
    uploadAbort?.abort();
    closeDialog(dlg); dlg.remove();
  });
  closeBtn.addEventListener("click", () => dismiss(closeBtn)); x.addEventListener("click", () => dismiss(x));
  dlg.addEventListener("cancel", (event) => {
    event.preventDefault(); event.stopImmediatePropagation();
    if (!cancelConfirm.hidden) resumeUpload();
    else dismiss();
  });
  dlg.addEventListener("close", () => { if (!checking) uploadAbort?.abort(); });
  restartBtn.addEventListener("click", () => {
    if (restartBtn.disabled) return;
    void (async () => { loading = true; ++state.readGen; refreshFooter(); clear(errorBox);
      try { const opened = await openUploadGroup(); adopt(null); state.receiptGroup = opened.group_id; state.version = null; rejections.length = 0; announcement = ""; await loadGroup(); }
      catch (err) { showError(err); loading = false; refreshFooter(); }
    })();
  });
  const runUploadPlan = (): void => {
    if (updateBtn.getAttribute("aria-disabled") === "true") {
      const missing = summaryBox.querySelector<HTMLElement>(".upload-slot-problem, .upload-slot-waiting");
      (missing ?? reason).focus(); return;
    }
    if (!state.group || !state.version || checking) return;
    checking = true; refreshFooter(); clear(errorBox);
    // Keep the footer in place while the waiting list scrolls inside the body.
    body.style.maxHeight = `${body.getBoundingClientRect().height}px`;
    const stop = armUpdateCheckStatus(updateBtn, lang, app, undefined, false, body);
    void runPlanFlow({
      source: { kind: "upload", group_id: state.group.group_id, version: state.version, gpus: [...selected] },
      owns: () => dlg.isConnected,
      orphanedReady: (planId) => { void cancelPlan(planId).catch(() => {}); },
      ready: (res) => {
        closeDialog(dlg);
        openConfirmDialog(app, res.plan_id, res.dialog, trigger, {
          onBack: () => { checking = false; openDialog(dlg, title, { trigger }); refreshFooter(); void loadGroup(); },
          onStarted: () => dlg.remove(),
        });
      },
      blocked: (res) => openCheckDialog(app, res.failures ?? [], [], res.gpu, updateBtn, runUploadPlan),
      uncomputable: (res) => openCheckDialog(app, [], res.items ?? [], null, updateBtn),
      failed: async (err) => {
        if (err instanceof ApiError) {
          renderPlanError(app, errorBox, err, true); silenceNotices(); announcement = text("upload.check_failed");
          if (err.key === "plan.changed") {
            try { const fresh = await getVersions(); if (!dlg.isConnected) return; Object.assign(versions, fresh); gpus = countedGpus(fresh); selected.clear(); if (gpus.length === 1 && gpus[0]) selected.add(gpus[0].address); rebuildGpu(); onSelectionChange(); await loadGroup(); }
            catch (refreshErr) { showError(refreshErr); }
          }
        } else showError(err);
      },
      finish: () => { stop(); body.style.removeProperty("max-height"); checking = false; if (dlg.isConnected) refreshFooter(); },
    });
  };
  updateBtn.addEventListener("click", runUploadPlan);
  const relabel = (): void => {
    lang = app.lang; title.textContent = text("upload.title"); x.setAttribute("aria-label", text("dialog.close"));
    cancelMessage.textContent = text("upload.cancel.confirm");
    keepUploading.textContent = text("upload.cancel.keep"); stopUploading.textContent = text("upload.cancel.confirm_action");
    lead.textContent = text("upload.lead"); dropText.textContent = text("upload.drop"); choose.textContent = text("upload.choose"); extension.textContent = text("upload.extension");
    howTitle.textContent = text("upload.how.title"); howBody.textContent = text("upload.how.body"); howTrust.textContent = text("upload.how.nosig");
    restartBtn.textContent = text("upload.restart"); rebuildGpu(); rebuildDownloadVersion(); rebuildSummary(); rebuildProgress();
  };
  const onRedraw = (): void => { if (!dlg.isConnected) return; relabel(); app.addCleanup(onRedraw); };
  app.addCleanup(onRedraw);
  relabel(); show();
  const recoverAndLoad = (): void => {
    loading = true; clear(errorBox); refreshFooter();
    void recoverUploadPlan().then(loadGroup).catch((err: unknown) => {
      if (!dlg.isConnected) return;
      groupReadFailed = true; showError(err); loading = false; refreshFooter();
      const retry = el("button", { type: "button", text: text("upload.read_retry") });
      retry.addEventListener("click", recoverAndLoad); errorBox.append(retry);
    });
  };
  recoverAndLoad();
  void getUploadToolkitVersion().then((res) => { if (!dlg.isConnected) return; toolkitVersion = res.version; toolkitFallback = res.fallback; rebuildSummary(); }).catch(showError);
}
