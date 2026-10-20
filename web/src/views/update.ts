// Selection changes recompute GPU compatibility locally; the server independently checks it when
// creating a plan.

import {
  ApiError,
  getVersions,
  getCurrentUpload,
  openUploadGroup,
  putUploadFile,
  postPlan,
  type GroupSummary,
  type GpuReading,
  type UploadFileInfo,
  type VersionsResponse,
} from "../api.js";
import {
  el,
  clear,
  expandText,
  formatMB,
  queryFailed,
  renderNotice,
  apiErrorNotice,
  type Params,
} from "../dom.js";
import type { App } from "../main.js";
import { conflict, fileTooLarge, filterVersions, gpuLabel, queryObjectKey, toolkitTooOld, versionFormatOk } from "../rules.js";
import { closeDialog, openDialog } from "../dialog.js";
import { armUpdateCheckStatus, openCheckDialog, openConfirmDialog, renderPlanError } from "./plan.js";

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
    // A successful version read confirms the session; a failed login follow-up must not blank the page.
    if (!container.isConnected) return;
    try {
      await app.refreshLogin(() => container.isConnected);
      if (!container.isConnected) return;
      if (!app.signedIn) {
        app.render();
        return;
      }
    } catch (err) {
      if (!container.isConnected) return;
      if (err instanceof ApiError && err.isSessionEnded) {
        app.sessionEnded();
        return;
      }
      const readError = el("div", { id: "login-read-error" });
      readError.append(apiErrorNotice(err, lang));
      container.append(readError);
    }

    let gpus = countedGpus(versions);
    const selected = new Set<string>();
    if (gpus.length === 1 && gpus[0]) selected.add(gpus[0].address);
    let chosen: string | null = null;

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
      text: expandText("update.upload_button", {}, lang),
    });
    const link = el("a", {
      href: "https://www.nvidia.com/en-us/drivers/unix/#linux64",
      text: expandText("update.link_text", {}, lang),
    });
    const buttonArea = el("div", { class: "actions" });
    buttonArea.append(updateBtn, uploadBtn, link);

    container.append(gpuArea, selectArea, describe, buttonArea, errorBox);
    container.append(
      renderNotice(
        "upload.help",
        {},
        lang,
        { class: "upload-help" },
      ),
    );

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
      if (!conclusion) {
        describe.append(el("p", { class: "hint", text: expandText("catalog.help_upload", {}, lang) }));
      }
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

    let planInFlight = false;
    updateBtn.addEventListener("click", () => {
      if (chosen === null || loginReadFailed(app) || planInFlight) return;
      // Ignore double clicks while the first plan request is in flight.
      planInFlight = true;
      updateBtn.disabled = true;
      clear(errorBox);
      const stopCheckStatus = armUpdateCheckStatus(updateBtn, lang);
      void (async () => {
        try {
          const res = await postPlan({
            kind: "official",
            version: chosen as string,
            gpus: [...selected],
          });
          // A late plan response for a departed page cannot open a dialog.
          if (!container.isConnected) return;
          if (res.result === "ready") {
            openConfirmDialog(app, res.plan_id, res.dialog, updateBtn);
          } else if (res.result === "blocked") {
            openCheckDialog(app, res.failures ?? [], [], res.gpu, updateBtn);
          } else {
            openCheckDialog(app, [], res.items ?? [], null, updateBtn);
          }
        } catch (err) {
          if (!container.isConnected) return;
          if (err instanceof ApiError) {
            renderPlanError(app, errorBox, err, true);
            if (err.key === "plan.changed") {
              // A rejected plan may mean the GPU set changed; refresh versions and rebuild the
              // selection while reporting any read failure.
              try {
                versions = await getVersions();
                if (!container.isConnected) return;
                await app.refreshLogin(() => container.isConnected);
                if (!container.isConnected) return;
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
                if (!container.isConnected) return;
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
        } finally {
          stopCheckStatus();
          planInFlight = false;
          if (container.isConnected) refreshButton();
        }
      })();
    });

    uploadBtn.addEventListener("click", () => {
      if (loginReadFailed(app)) return;
      openUploadDialog(app, versions, selected, uploadBtn, () => {
        rebuild();
      });
    });
  })();
}

interface UploadState {
  group: GroupSummary | null;
  version: string | null;
  // Only a confirmed response changes the group that owns the receipts.
  receiptGroup: string | null;
  received: Map<string, UploadFileInfo>;
  // A newer read or operation invalidates outstanding reads.
  readGen: number;
  busy: boolean;
}

// Keep manually entered versions across dialog closure and language redraws.
const uploadManualVersion: { value: string | null } = { value: null };

function openUploadDialog(
  app: App,
  versions: VersionsResponse,
  selected: Set<string>,
  trigger: HTMLElement,
  onSelectionChange: () => void,
): void {
  // The dialog outlives page redraws; its closures must read the current language.
  let lang = app.lang;
  let gpus = countedGpus(versions);
  const state: UploadState = {
    group: null,
    version: null,
    receiptGroup: null,
    received: new Map(),
    readGen: 0,
    busy: false,
  };

  const dlg = el("dialog", { class: "nvm-dialog" });
  const title = el("h3", { class: "dialog-title", text: expandText("dialog.upload", {}, lang) });
  dlg.append(title);
  const help = el("p", { class: "hint", text: expandText("toolkit.help", {}, lang) });
  dlg.append(help);

  const gpuArea = el("div", { class: "gpu-area" });
  const conclusionBox = el("div", { class: "conclusion-area" });
  const filesLabel = el("label", { for: "upload-files", text: expandText("upload.files_label", {}, lang) });
  const files = el("input", {
    id: "upload-files",
    type: "file",
    accept: ".run,.deb",
    multiple: true,
  });
  const startBtn = el("button", { type: "button", text: expandText("upload.start", {}, lang) });
  const restartBtn = el("button", {
    type: "button",
    text: expandText("upload.restart", {}, lang),
  });
  const summaryBox = el("div", { class: "upload-summary" });
  const versionBox = el("div", { class: "upload-version" });
  // Progress stays outside alert/status regions to avoid repeated announcements.
  const progressBox = el("div", { class: "upload-progress" });
  const errorBox = el("div", { class: "dialog-error" });
  const updateBtn = el("button", { type: "button", text: expandText("upload.update", {}, lang) });
  const closeBtn = el("button", { type: "button", text: expandText("upload.close", {}, lang) });

  dlg.append(gpuArea, conclusionBox, filesLabel, files, startBtn, restartBtn, progressBox, summaryBox, versionBox, errorBox, updateBtn, closeBtn);

  const selectedGpus = (): GpuReading[] => gpus.filter((g) => selected.has(g.address));

  const rebuildGpu = (): void => {
    const activeId =
      document.activeElement instanceof HTMLElement ? document.activeElement.id : "";
    clear(gpuArea);
    const box = gpuSelection(
      app,
      gpus,
      selected,
      () => {
        rebuildGpu();
        rebuildConclusion();
        refreshUpdateBtn();
        onSelectionChange();
      },
      "upload-",
    );
    if (box) gpuArea.append(box);
    if (activeId !== "") document.getElementById(activeId)?.focus();
  };

  const rebuildConclusion = (): void => {
    clear(conclusionBox);
    if (conflict(selectedGpus())) {
      const line = renderNotice("catalog.no_usable_version", {}, lang, { class: "conclusion" });
      line.id = "upload-conclusion";
      conclusionBox.append(line);
    }
  };

  const refreshUpdateBtn = (): void => {
    const g = state.group;
    const selectedOk = gpus.length < 2 || selected.size > 0;
    const conflictNow = conflict(selectedGpus());
    const groupOk = g !== null && g.run !== null && g.missing.length === 0 && g.problems.length === 0;
    const versionOk = state.version !== null && versionFormatOk(state.version);
    const toolkitOk = !(state.version !== null && toolkitTooOld(state.version, g?.toolkit_version ?? null));
    const blocked = loginReadFailed(app) || (app.login.update_blocked ?? []).length > 0;
    updateBtn.disabled = !(
      groupOk &&
      selectedOk &&
      !conflictNow &&
      versionOk &&
      toolkitOk &&
      !blocked &&
      !state.busy
    );
    if (gpus.length >= 2 && selected.size === 0) {
      updateBtn.setAttribute("aria-describedby", "upload-gpu-select");
    } else if (conflictNow) {
      updateBtn.setAttribute("aria-describedby", "upload-conclusion");
    } else {
      updateBtn.removeAttribute("aria-describedby");
    }
  };

  const setBusy = (busy: boolean): void => {
    state.busy = busy;
    files.disabled = busy;
    startBtn.disabled = busy;
    restartBtn.disabled = busy;
    closeBtn.disabled = busy;
    refreshUpdateBtn();
  };

  // Closing the dialog aborts the active upload so later batch files are never sent.
  const uploadAbort = new AbortController();
  dlg.addEventListener("close", () => uploadAbort.abort());

  const rebuildVersion = (): void => {
    clear(versionBox);
    const toolkitNote = el("div", { class: "upload-toolkit" });
    const refreshToolkitNote = (): void => {
      clear(toolkitNote);
      const g2 = state.group;
      if (state.version !== null && g2 && toolkitTooOld(state.version, g2.toolkit_version)) {
        toolkitNote.append(
          renderNotice(
            "upload.group.toolkit_too_old",
            { driver_version: state.version, toolkit_version: g2.toolkit_version ?? "" },
            lang,
          ),
        );
      }
      refreshUpdateBtn();
    };
    const g = state.group;
    const candidates = g?.run?.candidates ?? [];
    if (!g || !g.run) {
      versionBox.append(toolkitNote);
      refreshToolkitNote();
      return;
    }
    if (candidates.length === 1) {
      state.version = candidates[0] ?? null;
      versionBox.append(
        renderNotice("upload.version.detected", { version: state.version ?? "" }, lang),
      );
    } else if (candidates.length > 1) {
      versionBox.append(el("p", { class: "hint", text: expandText("upload.version.choose", {}, lang) }));
      if (state.version === null || !candidates.includes(state.version)) {
        state.version = candidates[0] ?? null;
      }
      for (const c of candidates) {
        const radio = el("input", {
          id: `version-${c}`,
          name: "upload-version",
          type: "radio",
          value: c,
          checked: state.version === c,
        });
        radio.addEventListener("change", () => {
          state.version = c;
          rebuildVersion();
          refreshUpdateBtn();
          // Restore radio focus after rebuilding to preserve the arrow-key chain.
          document.getElementById(`version-${c}`)?.focus();
        });
        const label = el("label", { for: `version-${c}`, text: c });
        versionBox.append(el("div", { class: "version-option" }, [radio, label]));
      }
    } else {
      versionBox.append(el("p", { class: "hint", text: expandText("upload.version.enter", {}, lang) }));
      if (state.version === null && uploadManualVersion.value !== null) {

        state.version = uploadManualVersion.value;
      }
      const label = el("label", { for: "upload-version-input", text: expandText("upload.version_label", {}, lang) });
      const input = el("input", { id: "upload-version-input", type: "text", value: state.version ?? "" });
      const invalid = el("p", { class: "hint", id: "upload-version-invalid" });
      input.addEventListener("input", () => {
        state.version = input.value === "" ? null : input.value;
        uploadManualVersion.value = state.version;
        invalid.textContent =
          state.version !== null && !versionFormatOk(state.version)
            ? expandText("upload.version.invalid", {}, lang)
            : "";

        refreshToolkitNote();
      });
      versionBox.append(label, input, invalid);
    }
    versionBox.append(toolkitNote);
    refreshToolkitNote();
  };

  const rebuildSummary = (): void => {
    clear(summaryBox);
    const g = state.group;
    if (!g) {
      refreshUpdateBtn();
      rebuildVersion();
      return;
    }
    if (g.run) {
      summaryBox.append(
        el("p", {
          class: "summary-line",
          text: `${g.run.fingerprint} ${g.run.sha256.slice(0, 12)}`,
        }),
      );
    }
    for (const [pkg, deb] of Object.entries(g.debs ?? {})) {
      summaryBox.append(el("p", { class: "summary-line", text: `${pkg} ${deb.version}` }));
    }
    for (const p of g.problems) {
      summaryBox.append(renderNotice(p.key, (p.params ?? {}) as Params, lang));
    }
    if (g.below_security_floor && g.toolkit_version !== null) {
      summaryBox.append(
        renderNotice("toolkit.below_floor", { toolkit_version: g.toolkit_version }, lang),
      );
    }
    if (state.received.size > 0 && g.group_id === state.receiptGroup) {
      for (const info of state.received.values()) {
        summaryBox.append(
          renderNotice("upload.file_ok", { file_name: info.client_name }, lang),
        );
      }
    }
    rebuildVersion();
  };

  // Only the newest read of a live dialog may adopt a group; an earlier read cannot restore a group
  // whose final read failed.
  const newestRead = (gen: number): boolean => gen === state.readGen && dlg.isConnected;

  // A confirmed null or replacement group invalidates earlier receipts.
  const adoptConfirmed = (g: GroupSummary | null): void => {
    const id = g?.group_id ?? null;
    if (id !== state.receiptGroup) {
      state.received.clear();
    }
    state.receiptGroup = id;
    state.group = g;
  };

  // Opening a group confirms its id immediately and invalidates prior receipts even if the final
  // summary read fails.
  const adoptAcknowledged = (id: string): void => {
    if (id !== state.receiptGroup) {
      state.received.clear();
    }
    state.receiptGroup = id;
    state.group = state.group?.group_id === id ? state.group : null;
  };

  const loadGroup = async (): Promise<void> => {
    // A read cannot update a dialog that has been removed.
    if (!dlg.isConnected) return;
    const gen = (state.readGen += 1);
    let group: GroupSummary | null = null;
    try {
      group = await getCurrentUpload();
    } catch (err) {
      if (!newestRead(gen)) return;
      // A failed group read proves nothing about receipts. Drop only the displayed summary and keep Check disabled.
      state.group = null;
      if (err instanceof ApiError) {
        renderPlanError(app, errorBox, err, false);
      } else {
        errorBox.append(
          renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang),
        );
      }
      rebuildSummary();
      return;
    }
    if (!newestRead(gen)) return;
    adoptConfirmed(group);
    rebuildSummary();
  };

  const uploadOne = async (file: File, groupId: string): Promise<void> => {
    if (fileTooLarge(file.size)) {
      errorBox.append(renderNotice("upload.rejected.too_large", { file_name: file.name }, lang));
      return;
    }
    const total = file.size;
    try {
      const res = await putUploadFile(
        groupId,
        file.name,
        file,
        (sent) => {
          clear(progressBox);
          progressBox.append(
            renderNotice(
              "upload.receiving",
              { file_name: file.name, done: formatMB(sent), total: formatMB(total) },
              lang,
            ),
          );
        },
        uploadAbort.signal,
      );
      clear(progressBox);
      // Confirm the response file and group before adoption. Invalidate prior receipts before adding
      // this request receipt so it is not cleared accidentally.
      if (res.group.group_id !== groupId || res.file.client_name !== file.name) {
        errorBox.append(
          renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang),
        );
        return;
      }
      adoptAcknowledged(res.group.group_id);
      state.group = res.group;
      state.received.set(file.name, res.file);
      rebuildSummary();
    } catch (err) {
      clear(progressBox);
      if (err instanceof ApiError) {
        if (err.status === 0) {
          errorBox.append(renderNotice("upload.interrupted", { file_name: file.name }, lang));
        } else if (err.key === "upload.rejected") {
          errorBox.append(
            renderNotice(
              `upload.rejected.${String(err.params["reason"] ?? "write_failed")}`,
              err.params,
              lang,
            ),
          );
        } else {
          renderPlanError(app, errorBox, err, false);
        }
      } else {
        errorBox.append(
          renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang),
        );
      }
      return;
    }
  };

  startBtn.addEventListener("click", () => {
    void (async () => {
      clear(errorBox);
      const picked = files.files;
      if (!picked || picked.length === 0) return;
      // A read started before this operation cannot land afterwards.
      state.readGen += 1;
      setBusy(true);
      let groupId = state.group?.group_id ?? null;
      try {
        if (groupId === null) {
          const opened = await openUploadGroup();
          groupId = opened.group_id;
          // Every successful group-open response invalidates prior receipts immediately, even if
          // uploads or the final read fail.
          adoptAcknowledged(groupId);
        }
        for (const file of Array.from(picked)) {
          // Do not send remaining batch files after the owning dialog closes.
          if (!dlg.isConnected) break;
          await uploadOne(file, groupId);
        }
      } catch (err) {
        if (err instanceof ApiError) renderPlanError(app, errorBox, err, false);
        else {

          errorBox.append(
            renderNotice(
              "internal",
              { reason: { key: "internal.request_failed", params: {} } },
              lang,
            ),
          );
        }
      } finally {
        // Keep the batch busy through its single final group read; controls cannot act on an unknown summary.
        await loadGroup();
        setBusy(false);
      }
    })();
  });

  restartBtn.addEventListener("click", () => {
    void (async () => {
      clear(errorBox);
      state.readGen += 1;
      setBusy(true);
      try {
        const opened = await openUploadGroup();
        // The confirmed group id invalidates prior receipts even if the following read fails.
        adoptAcknowledged(opened.group_id);
        state.version = null;
        rebuildSummary();
      } catch (err) {
        if (err instanceof ApiError) renderPlanError(app, errorBox, err, false);
        else {

          errorBox.append(
            renderNotice(
              "internal",
              { reason: { key: "internal.request_failed", params: {} } },
              lang,
            ),
          );
        }
      } finally {
        await loadGroup();
        setBusy(false);
      }
    })();
  });

  let planInFlight = false;
  updateBtn.addEventListener("click", () => {
    if (
      state.version === null ||
      state.group === null ||
      loginReadFailed(app) ||
      planInFlight
    ) {
      return;
    }
    // Ignore double clicks while the first plan request is in flight.
    planInFlight = true;
    updateBtn.disabled = true;
    clear(errorBox);
    void (async () => {
      try {
        const res = await postPlan({
          kind: "upload",
          group_id: state.group?.group_id ?? "",
          version: state.version as string,
          gpus: [...selected],
        });
        // A response belongs only to the still-live dialog.
        if (!dlg.isConnected) return;
        if (res.result === "ready") {
          closeDialog(dlg);
          dlg.remove();
          openConfirmDialog(app, res.plan_id, res.dialog, updateBtn);
        } else if (res.result === "blocked") {
          openCheckDialog(app, res.failures ?? [], [], res.gpu, updateBtn);
        } else {
          openCheckDialog(app, [], res.items ?? [], null, updateBtn);
        }
      } catch (err) {
        if (!dlg.isConnected) return;
        if (err instanceof ApiError) {
          renderPlanError(app, errorBox, err, true);
          if (err.key === "plan.changed") {
            // Refresh versions after a rejected plan and report any refresh failure.
            try {
              const fresh = await getVersions();
              if (!dlg.isConnected) return;
              Object.assign(versions, fresh);
              gpus = countedGpus(fresh);
              selected.clear();
              if (gpus.length === 1 && gpus[0]) selected.add(gpus[0].address);
              rebuildGpu();
              rebuildConclusion();
              refreshUpdateBtn();
              onSelectionChange();
              await loadGroup();
            } catch (refreshErr) {
              if (!dlg.isConnected) return;
              if (refreshErr instanceof ApiError && refreshErr.isSessionEnded) {
                app.sessionEnded();
                return;
              }
              errorBox.append(apiErrorNotice(refreshErr, lang));
            }
          }
        } else {
          errorBox.append(apiErrorNotice(err, lang));
        }
      } finally {
        planInFlight = false;
        if (dlg.isConnected) refreshUpdateBtn();
      }
    })();
  });

  closeBtn.addEventListener("click", () => {
    closeDialog(dlg);
    dlg.remove();
  });

  // Language redraws relabel the open dialog while preserving its typed version and staged files.
  const relabel = (): void => {
    lang = app.lang;
    title.textContent = expandText("dialog.upload", {}, lang);
    help.textContent = expandText("toolkit.help", {}, lang);
    filesLabel.textContent = expandText("upload.files_label", {}, lang);
    startBtn.textContent = expandText("upload.start", {}, lang);
    restartBtn.textContent = expandText("upload.restart", {}, lang);
    updateBtn.textContent = expandText("upload.update", {}, lang);
    closeBtn.textContent = expandText("upload.close", {}, lang);
    rebuildGpu();
    rebuildConclusion();
    rebuildSummary();
  };
  const onRedraw = (): void => {
    if (!dlg.isConnected) return;
    relabel();
    app.addCleanup(onRedraw);
  };
  app.addCleanup(onRedraw);

  document.body.append(dlg);
  openDialog(dlg, title, { trigger });
  rebuildGpu();
  rebuildConclusion();
  void loadGroup();
}
