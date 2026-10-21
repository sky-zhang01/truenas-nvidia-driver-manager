import {
  ApiError,
  deleteDrivers,
  getDrivers,
  setKeep,
  type DriverRow,
  type DriversResponse,
  type ReasonField,
} from "../api.js";
import {
  el,
  clear,
  expandText,
  formatDate,
  formatSize,
  queryFailed,
  renderNotice,
  apiErrorNotice,
} from "../dom.js";
import type { App } from "../main.js";
import { compareRows, queryObjectKey, type SortColumn, type SortDir } from "../rules.js";
import { closeDialog, makeDialog } from "../dialog.js";
import { armUpdateCheckStatus, openCheckDialog, openConfirmDialog, renderPlanError } from "./plan.js";
import { loginReadFailed, updateBlockedNode } from "./update.js";
import { runPlanFlow } from "../plan-flow.js";
import { guardSignedIn } from "../session-view.js";

function sourceText(app: App, row: DriverRow): string {
  const lang = app.lang;
  if (row.kind === "upload") {
    return expandText("drivers.source.upload", { fingerprint: row.fingerprint ?? "" }, lang);
  }
  if (row.kind === null) {
    // Unreadable records have no confirmed kind; never invent an original-driver source.
    return expandText("status.UNKNOWN", {}, lang);
  }
  const key = `drivers.source.${row.kind}`;
  return expandText(key, {}, lang);
}

function statusKey(status: string): string {
  return status === "UNKNOWN" ? "status.UNKNOWN" : `drivers.status.${status}`;
}

interface DriverPlanControls {
  active: AbortController | null;
  writing: boolean;
  controls: Map<HTMLButtonElement | HTMLInputElement, () => boolean>;
}

function refreshPlanControls(plans: DriverPlanControls): void {
  for (const [control, blocked] of plans.controls) control.disabled = plans.active !== null || plans.writing || blocked();
}

function driverUpdateButton(app: App, container: HTMLElement, row: DriverRow, errorBox: HTMLElement, plans: DriverPlanControls): HTMLButtonElement {
  const lang = app.lang;
  const update = el("button", {
    type: "button", text: expandText(row.kind === "original" ? "drivers.restore_row" : "drivers.update_row", {}, lang),
  });
  const blocked = (): boolean => !row.can_update || (app.login.update_blocked ?? []).length > 0 || loginReadFailed(app);
  plans.controls.set(update, blocked);
  update.disabled = plans.active !== null || plans.writing || blocked();
  if (loginReadFailed(app)) update.setAttribute("aria-describedby", "login-read-error");
  const runPlan = (): void => {
    if (loginReadFailed(app) || plans.active || plans.writing) return;
    clear(errorBox);
    const controller = new AbortController();
    plans.active = controller;
    refreshPlanControls(plans);
    const owns = (): boolean => !controller.signal.aborted && plans.active === controller && container.isConnected;
    const cancel = (): void => {
      if (plans.active !== controller) return;
      controller.abort(); stopCheckStatus(); plans.active = null;
      if (container.isConnected) { refreshPlanControls(plans); update.focus(); }
    };
    const stopCheckStatus = armUpdateCheckStatus(update, lang, app, cancel);
    app.addCleanup(() => controller.abort());
    void runPlanFlow({
      source: { kind: "local", row_id: row.id }, signal: controller.signal, owns,
      ready: (res) => openConfirmDialog(app, res.plan_id, res.dialog, update),
      blocked: (res) => openCheckDialog(app, res.failures ?? [], [], res.gpu, update, runPlan),
      uncomputable: (res) => openCheckDialog(app, [], res.items ?? [], null, update),
      failed: (err) => {
        if (err instanceof ApiError) renderPlanError(app, errorBox, err, true);
        else errorBox.append(renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang));
      },
      finish: () => {
        stopCheckStatus();
        if (plans.active === controller) {
          plans.active = null;
          refreshPlanControls(plans);
        }
      },
    });
  };
  update.addEventListener("click", runPlan);
  return update;
}

interface DriverDeletion {
  errorBox: HTMLElement;
  applyRemovedRows: (ids: readonly string[]) => void;
  refreshOrReport: (target: HTMLElement) => Promise<void>;
  focusAfterDelete: () => void;
  beginWrite: () => boolean;
  endWrite: () => void;
}

function openDeleteConfirm(app: App, container: HTMLElement, rows: DriverRow[], trigger: HTMLElement, deletion: DriverDeletion): void {
  const lang = app.lang;
  const { errorBox, applyRemovedRows, refreshOrReport, focusAfterDelete } = deletion;
  if (rows.length === 0 || loginReadFailed(app)) return;
  const msgKey = rows.length === 1 ? "delete.confirm.one" : "delete.confirm";
  const intro = renderNotice(msgKey, { row_count: rows.length }, lang);
  const { dialog: dlg, show } = makeDialog({ title: intro.querySelector(".notice-title")?.textContent ?? "", trigger });
  dlg.append(el("p", { text: intro.querySelector(".notice-body")?.textContent ?? "" }));

  const table = el("table", { class: "confirm-table" });
  const headRow = el("tr", {});
  for (const key of [
    "drivers.col.version",
    "drivers.col.source",
    "drivers.col.kernel",
    "drivers.col.date",
  ]) {
    headRow.append(el("th", { scope: "col", text: expandText(key, {}, lang) }));
  }
  const thead = el("thead", {});
  thead.append(headRow);
  const tbody = el("tbody", {});
  for (const r of rows) {
    const tr = el("tr", {});
    tr.append(el("td", { text: r.version ?? r.id }));
    tr.append(el("td", { text: sourceText(app, r) }));
    tr.append(el("td", { text: r.target_kernel ?? "UNKNOWN" }));
    tr.append(
      el("td", { text: r.created_at !== null ? formatDate(r.created_at) : "UNKNOWN" }),
    );
    tbody.append(tr);
  }
  table.append(thead, tbody);
  dlg.append(table);
  const errBox = el("div", { class: "dialog-error" });
  const confirmBtn = el("button", {
    type: "button",
    class: "btn-danger",
    text: expandText("delete.confirm_button", {}, lang),
  });
  const cancelBtn = el("button", {
    type: "button",
    text: expandText("delete.cancel_button", {}, lang),
  });
  const dismiss = (): void => {
    closeDialog(dlg);
    dlg.remove();
  };
  cancelBtn.addEventListener("click", dismiss);
  confirmBtn.addEventListener("click", () => {
    if (!deletion.beginWrite()) return;
    void (async () => {
      clear(errBox);
      // After successful deletion removes the dialog, report follow-up read failures in the page error area.
      let deleted = false;
      try {
        const res = await deleteDrivers(rows.map((r) => r.id));
        deleted = true;
        dismiss();
        // Apply confirmed deletions before reading the list again so a failed read cannot revive deleted rows.
        applyRemovedRows(res.deleted);
        errorBox.append(
          renderNotice(
            res.deleted.length === 1 ? "delete.done.one" : "delete.done",
            { row_count: res.deleted.length },
            lang,
          ),
        );
        await refreshOrReport(errorBox);
      } catch (err) {
        if (deleted) {

          if (err instanceof ApiError && err.isSessionEnded) {
            app.sessionEnded();
            return;
          }
          errorBox.append(apiErrorNotice(err, lang));
          return;
        }
        if (err instanceof ApiError) {
          if (err.isSessionEnded) {
            app.sessionEnded();
            return;
          }
          if (err.key === "delete.refused") {
            for (const r of (err.params["rows"] ?? []) as {
              id: string;
              reason: string;
              version: string | null;
              kind: string | null;
            }[]) {
              // Missing version or kind uses the row id for the whole label; never fill it from an unconfirmed cached row.
              const wholeId = r.version === null || r.kind === null;
              const row = rows.find((x) => x.id === r.id);
              errBox.append(
                renderNotice(
                  wholeId ? `delete.refused.${r.reason}.id_only` : `delete.refused.${r.reason}`,
                  wholeId
                    ? { version: r.id }
                    : {
                        version: r.version,
                        source:
                          r.kind === "upload"
                            ? expandText(
                                "drivers.source.upload",
                                { fingerprint: row?.fingerprint ?? "UNKNOWN" },
                                lang,
                              )
                            : expandText(`drivers.source.${r.kind}`, {}, lang),
                      },
                  lang,
                ),
              );
            }
          } else if (err.key === "delete.task_state_unknown") {

            errBox.append(renderNotice("delete.task_state_unknown", {}, lang));
            errBox.append(
              queryFailed(
                queryObjectKey("running_task"),
                (err.params["reason"] ?? null) as ReasonField | null,
                null,
                lang,
              ),
            );
          } else if (err.key === "delete.failed") {
            // Partial deletion results are confirmed facts even when the request reports another error.
            const removed = Array.isArray(err.params["deleted"])
              ? (err.params["deleted"] as unknown[]).filter(
                  (v): v is string => typeof v === "string",
                )
              : [];
            applyRemovedRows(removed);
            errBox.append(
              renderNotice(
                "delete.failed",
                { ...err.params, deleted_count: removed.length },
                lang,
              ),
            );
            await refreshOrReport(errBox);
          } else {
            renderPlanError(app, errBox, err, false);
          }
        } else {

          errBox.append(
            renderNotice(
              "internal",
              { reason: { key: "internal.request_failed", params: {} } },
              lang,
            ),
          );
        }
      } finally {
        deletion.endWrite();
        if (deleted && container.isConnected) focusAfterDelete();
      }
    })();
  });
  dlg.append(errBox, confirmBtn, cancelBtn);
  show();

}

export function renderDrivers(app: App, container: HTMLElement): void {
  const lang = app.lang;
  void (async () => {
    let data: DriversResponse;
    try {
      data = await getDrivers();
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

    mountDriverList(app, container, data);
  })();
}


function mountDriverList(app: App, container: HTMLElement, initialData: DriversResponse): void {
  const lang = app.lang;
  let data = initialData;
  container.append(el("h2", { text: expandText("nav.drivers", {}, lang) }));
  container.append(updateBlockedNode(app));

  const errorBox = el("div", { class: "table-error" });
  const tableArea = el("div", { class: "drivers-table" });
  const actions = el("div", { class: "actions" });
  const deleteSelected = el("button", {
    type: "button",
    text: expandText("drivers.delete_selected", {}, lang),
  });
  const deleteReason = el("span", { class: "hint", id: "delete-reason" });
  actions.append(deleteSelected, deleteReason);
  container.append(actions, errorBox, tableArea);

  const selected = new Set<string>();
  let sortCol: SortColumn = "version";
  let sortDir: SortDir = "asc";
  const plans: DriverPlanControls = { active: null, writing: false, controls: new Map() };
  // A plan must keep its row and cancellation control mounted. Finish earlier writes and reads first.
  const beginWrite = (): boolean => {
    if (plans.active || plans.writing || !container.isConnected) return false;
    plans.writing = true;
    refreshPlanControls(plans);
    return true;
  };
  const endWrite = (): void => {
    plans.writing = false;
    refreshPlanControls(plans);
  };

  const sortable = (row: DriverRow) => ({
    version: row.version,
    source: row.kind,
    date: row.created_at,
    size: row.size,
    kernel: row.target_kernel,
    status: row.status === "UNKNOWN" ? "UNKNOWN" : row.status,
    keep: row.keep,
  });

  const renderDeleteReason = (): void => {
    clear(deleteReason);
    const un = data.delete_unavailable;
    if (!un) return;
    if (un.key === "task_running") {
      deleteReason.append(renderNotice("delete.task_running", {}, lang));
    } else if (un.key === "task_state_unreadable") {
      deleteReason.append(renderNotice("delete.task_state_unknown", {}, lang));
      deleteReason.append(
        queryFailed(
          queryObjectKey("running_task"),
          un.reason ?? { key: "reason.io_error", params: {} },
          null,
          lang,
        ),
      );
    } else if (un.key === "canonical_unreadable") {
      deleteReason.append(renderNotice("delete.canonical_unreadable", {}, lang));
    }
  };

  const confirmDelete = (rows: DriverRow[], trigger: HTMLElement): void => {
    if (plans.active || plans.writing) return;
    openDeleteConfirm(app, container, rows, trigger, { errorBox, applyRemovedRows, refreshOrReport, focusAfterDelete, beginWrite, endWrite });
  };

  const renderTable = (): void => {
    plans.controls.clear();
    clear(tableArea);
    const rows = [...data.rows].sort((a, b) =>
      compareRows(sortable(a) as never, sortable(b) as never, sortCol, sortDir),
    );
    const table = el("table", { class: "drivers" });
    const head = el("tr", {});
    const columns: { key: SortColumn; label: string }[] = [
      { key: "version", label: "drivers.col.version" },
      { key: "source", label: "drivers.col.source" },
      { key: "date", label: "drivers.col.date" },
      { key: "size", label: "drivers.col.size" },
      { key: "kernel", label: "drivers.col.kernel" },
      { key: "status", label: "drivers.col.status" },
      { key: "keep", label: "drivers.col.keep" },
    ];
    head.append(el("th", { scope: "col" }));
    for (const c of columns) {
      const th = el("th", { scope: "col" });
      const button = el("button", {
        type: "button",
        text: expandText(c.label, {}, lang),
        ariaLabel: expandText("drivers.sort", { column: expandText(c.label, {}, lang) }, lang),
      });
      plans.controls.set(button, () => false);
      if (sortCol === c.key) {
        th.setAttribute("aria-sort", sortDir === "asc" ? "ascending" : "descending");
        const direction = el("span", {
          class: "sort-direction", text: sortDir === "asc" ? "↑" : "↓",
        });
        direction.setAttribute("aria-hidden", "true");
        button.append(direction);
      } else {
        th.setAttribute("aria-sort", "none");
      }
      button.addEventListener("click", () => {
        if (plans.active || plans.writing) return;
        if (sortCol === c.key) sortDir = sortDir === "asc" ? "desc" : "asc";
        else {
          sortCol = c.key;
          sortDir = "asc";
        }
        renderTable();
        // Restore sort-control focus after rebuilding the table.
        document.getElementById(`sort-${c.key}`)?.focus();
      });
      button.id = `sort-${c.key}`;
      th.append(button);
      head.append(th);
    }
    head.append(el("th", { scope: "col", text: expandText("drivers.col.actions", {}, lang) }));
    const thead = el("thead", {});
    thead.append(head);
    const tbody = el("tbody", {});

    const deletable = rows.filter((r) => r.delete_blocked === null);
    if (rows.length === 0) {
      tableArea.append(el("p", { text: expandText("drivers.empty", {}, lang) }));
    }
    for (const row of rows) {
      const tr = el("tr", {});
      if (row.status === "kernel_mismatch") tr.classList.add("row-mismatch");

      const checkCell = el("td", {});
      const check = el("input", {
        id: `row-${row.id}`,
        type: "checkbox",
        checked: selected.has(row.id),
      });
      check.disabled =
        row.delete_blocked !== null || data.delete_unavailable !== null || loginReadFailed(app);
      check.setAttribute(
        "aria-label",
        expandText("drivers.select_row", { version: row.version ?? row.id, source: sourceText(app, row) }, lang),
      );
      check.addEventListener("change", () => {
        if (check.checked) selected.add(row.id);
        else selected.delete(row.id);
      });
      checkCell.append(check);
      tr.append(checkCell);

      tr.append(el("td", { class: "driver-version", text: row.version ?? row.id }));
      tr.append(el("td", { class: "driver-source", text: sourceText(app, row) }));
      const dateCell = el("td", { class: "driver-date" });
      const dateParts = (row.created_at !== null ? formatDate(row.created_at) : "UNKNOWN").split(" ");
      for (const [index, part] of dateParts.entries()) {
        if (index > 0) dateCell.append(" ");
        dateCell.append(el("span", { text: part }));
      }
      tr.append(dateCell);
      tr.append(
        el("td", {
          class: "driver-size",
          text:
            row.size !== null
              ? expandText("drivers.size_value", { size: formatSize(row.size) }, lang)
              : "UNKNOWN",
        }),
      );
      const kernelCell = el("td", { class: "driver-kernel" });
      const kernelParts = (row.target_kernel ?? "UNKNOWN").split("-");
      for (const [index, part] of kernelParts.entries()) {
        const hasNext = index < kernelParts.length - 1;
        kernelCell.append(el("span", { text: part + (hasNext ? "-" : "") }));
        if (hasNext) kernelCell.append(el("wbr", {}));
      }
      tr.append(kernelCell);
      tr.append(el("td", { class: "driver-status", text: expandText(statusKey(row.status), {}, lang) }));

      const keepCell = el("td", { class: "driver-keep" });
      if (row.kind === "original" && row.delete_blocked !== "unknown") {
        const sw = el("input", { type: "checkbox", checked: row.keep === true, role: "switch" });
        sw.disabled = loginReadFailed(app);
        plans.controls.set(sw, () => loginReadFailed(app));
        if (loginReadFailed(app)) sw.setAttribute("aria-describedby", "login-read-error");
        sw.setAttribute(
          "aria-label",
          expandText(row.keep ? "drivers.keep.on" : "drivers.keep.off", {}, lang),
        );
        sw.addEventListener("change", () => {
          if (loginReadFailed(app) || !beginWrite()) {
            sw.checked = row.keep === true;
            return;
          }
          void (async () => {

            let saved = false;
            try {
              const updated = await setKeep(row.id, sw.checked);
              saved = true;
              // Apply the complete confirmed keep-switch row before reading the list again; a failed
              // read cannot revert protection or selection.
              applyRowFact(updated);
              await refreshOrReport(errorBox);
            } catch (err) {
              if (saved) {
                if (err instanceof ApiError && err.isSessionEnded) {
                  app.sessionEnded();
                  return;
                }
                errorBox.append(apiErrorNotice(err, lang));
                return;
              }

              sw.checked = row.keep === true;
              if (err instanceof ApiError) {
                if (err.isSessionEnded) {
                  app.sessionEnded();
                  return;
                }
                errorBox.append(renderNotice(err.key, { ...err.params }, lang));
              } else {

                errorBox.append(
                  renderNotice(
                    "internal",
                    { reason: { key: "internal.request_failed", params: {} } },
                    lang,
                  ),
                );
              }
              // A follow-up 401 ends the session; other read failures remain next to the confirmed write result.
              await refreshOrReport(errorBox);
            } finally {
              endWrite();
            }
          })();
        });
        keepCell.append(sw);
        keepCell.append(
          el("span", {
            class: "hint",
            text: expandText("drivers.col.keep", {}, lang),
          }),
        );
      }
      tr.append(keepCell);

      const actionCell = el("td", { class: "driver-actions-cell" });
      const rowActions = el("div", { class: "driver-actions" });
      actionCell.append(rowActions);
      if (row.delete_blocked !== "in_use") {
        const del = el("button", { type: "button", text: expandText("drivers.delete_row", {}, lang) });
        del.disabled =
          row.delete_blocked !== null ||
          data.delete_unavailable !== null ||
          loginReadFailed(app);
        plans.controls.set(del, () => row.delete_blocked !== null || data.delete_unavailable !== null || loginReadFailed(app));
        if (loginReadFailed(app)) del.setAttribute("aria-describedby", "login-read-error");
        del.addEventListener("click", () => confirmDelete([row], del));
        rowActions.append(del);
      }
      rowActions.append(driverUpdateButton(app, container, row, errorBox, plans));

      if (row.delete_blocked === "in_use") {
        actionCell.append(
          renderNotice("drivers.delete_disabled_in_use", {}, lang, { class: "inline-reason" }),
        );
      } else if (row.delete_blocked === "kept") {
        actionCell.append(
          renderNotice("drivers.delete_disabled_kept", {}, lang, { class: "inline-reason" }),
        );
      } else if (row.delete_blocked === "unknown") {
        actionCell.append(
          renderNotice("drivers.row_unknown", {}, lang, { class: "inline-reason" }),
        );
      }
      tr.append(actionCell);
      tbody.append(tr);

      if (row.status === "kernel_mismatch") {
        const detail = el("tr", {});
        const cell = el("td", { colSpan: 9 });
        const variant =
          row.kind === "upload" ? "upload" : row.kind === "original" ? "original" : undefined;
        cell.append(
          renderNotice(
            "driver.kernel_mismatch",
            { target_kernel: row.target_kernel ?? "", current_kernel: data.kernel },
            lang,
            { variant },
          ),
        );
        detail.append(cell);
        tbody.append(detail);
      }
    }
    table.append(thead, tbody);
    const scroll = el("div", {
      class: "drivers-scroll", role: "region", ariaLabel: expandText("nav.drivers", {}, lang),
    }, [table]);
    scroll.tabIndex = 0;
    tableArea.append(scroll);

    // Only deletable rows participate in select-all and its indeterminate state.
    const allSelected = deletable.length > 0 && deletable.every((r) => selected.has(r.id));
    const someSelected = deletable.some((r) => selected.has(r.id));
    const selectAll = el("input", { id: "select-all", type: "checkbox", checked: allSelected });
    selectAll.disabled = data.delete_unavailable !== null || loginReadFailed(app);
    plans.controls.set(selectAll, () => data.delete_unavailable !== null || loginReadFailed(app));
    selectAll.indeterminate = someSelected && !allSelected;
    selectAll.setAttribute("aria-label", expandText("drivers.select_all", {}, lang));
    selectAll.addEventListener("change", () => {
      if (plans.active || plans.writing) return;
      if (selectAll.checked) {
        for (const r of deletable) selected.add(r.id);
      } else {
        for (const r of deletable) selected.delete(r.id);
      }
      renderTable();
      // Restore focus after replacing the checkbox during redraw.
      document.getElementById("select-all")?.focus();
    });
    const selectAllLabel = el("label", { for: "select-all", text: expandText("drivers.select_all", {}, lang) });
    const selectAllBox = el("div", { class: "select-all" });
    selectAllBox.append(selectAll, selectAllLabel);
    tableArea.prepend(selectAllBox);

    deleteSelected.disabled =
      data.delete_unavailable !== null || loginReadFailed(app);
    plans.controls.set(deleteSelected, () => data.delete_unavailable !== null || loginReadFailed(app));
    if (loginReadFailed(app)) {
      deleteSelected.setAttribute("aria-describedby", "login-read-error");
    } else if (data.delete_unavailable !== null) {
      deleteSelected.setAttribute("aria-describedby", "delete-reason");
    }
    deleteSelected.onclick = (): void => {
      confirmDelete(
        rows.filter((r) => selected.has(r.id)),
        deleteSelected,
      );
    };
    refreshPlanControls(plans);
  };

  // Update the local model from confirmed writes immediately; later read failures cannot undo
  // server-confirmed facts.
  const focusAfterDelete = (): void => {
    const selectAll = document.getElementById("select-all");
    if (selectAll instanceof HTMLInputElement && !selectAll.disabled) {
      selectAll.focus();
      return;
    }
    if (!deleteSelected.disabled) {
      deleteSelected.focus();
      return;
    }
    const heading = container.querySelector("h2");
    if (heading instanceof HTMLElement) {
      heading.tabIndex = -1;
      heading.focus();
    }
  };

  const applyRemovedRows = (ids: readonly string[]): void => {
    if (ids.length === 0) return;
    const gone = new Set(ids);
    data = { ...data, rows: data.rows.filter((r) => !gone.has(r.id)) };
    for (const id of gone) selected.delete(id);
    renderDeleteReason();
    renderTable();
  };

  const applyRowFact = (updated: DriverRow): void => {
    data = { ...data, rows: data.rows.map((r) => (r.id === updated.id ? updated : r)) };
    // A row that becomes protected must leave the selection.
    if (updated.delete_blocked !== null) selected.delete(updated.id);
    renderDeleteReason();
    renderTable();
  };

  // A follow-up 401 ends the session; other failures stay visible next to the confirmed write result.
  const refreshOrReport = async (target: HTMLElement): Promise<void> => {
    try {
      await refresh();
    } catch (err) {
      if (err instanceof ApiError && err.isSessionEnded) {
        app.sessionEnded();
        return;
      }
      target.append(apiErrorNotice(err, lang));
    }
  };

  const refresh = async (): Promise<void> => {
    data = await getDrivers();
    selected.clear();
    renderDeleteReason();
    renderTable();
  };

  renderDeleteReason();
  renderTable();

}
