import {
  ApiError,
  deleteDrivers,
  getDrivers,
  postPlan,
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
  formatMB,
  queryFailed,
  renderNotice,
  apiErrorNotice,
} from "../dom.js";
import type { App } from "../main.js";
import { compareRows, queryObjectKey, type SortColumn, type SortDir } from "../rules.js";
import { closeDialog, openDialog } from "../dialog.js";
import { openCheckDialog, openConfirmDialog, renderPlanError } from "./plan.js";
import { loginReadFailed, updateBlockedNode } from "./update.js";

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
    // A successful driver-list read confirms the session; a failed login-status follow-up must not blank the page.
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

    const openDeleteConfirm = (rows: DriverRow[], trigger: HTMLElement): void => {
      if (rows.length === 0 || loginReadFailed(app)) return;
      const msgKey = rows.length === 1 ? "delete.confirm.one" : "delete.confirm";
      const intro = renderNotice(msgKey, { row_count: rows.length }, lang);
      const dlg = el("dialog", { class: "nvm-dialog" });
      const title = el("h3", {
        class: "dialog-title",
        text: intro.querySelector(".notice-title")?.textContent ?? "",
      });
      dlg.append(title);
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
            if (container.isConnected) focusAfterDelete();
          } catch (err) {
            if (deleted) {

              if (err instanceof ApiError && err.isSessionEnded) {
                app.sessionEnded();
                return;
              }
              errorBox.append(apiErrorNotice(err, lang));
              if (container.isConnected) focusAfterDelete();
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
          }
        })();
      });
      dlg.append(errBox, confirmBtn, cancelBtn);
      document.body.append(dlg);
      openDialog(dlg, title, { trigger });
    };

    const renderTable = (): void => {
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
                ? expandText("drivers.size_value", { size: formatMB(row.size) }, lang)
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
          if (loginReadFailed(app)) sw.setAttribute("aria-describedby", "login-read-error");
          sw.setAttribute(
            "aria-label",
            expandText(row.keep ? "drivers.keep.on" : "drivers.keep.off", {}, lang),
          );
          sw.addEventListener("change", () => {
            if (loginReadFailed(app)) {
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
          if (loginReadFailed(app)) del.setAttribute("aria-describedby", "login-read-error");
          del.addEventListener("click", () => openDeleteConfirm([row], del));
          rowActions.append(del);
        }
        const update = el("button", {
          type: "button",
          text: expandText("drivers.update_row", {}, lang),
        });
        const updateBlockedNow = (): boolean =>
          !row.can_update ||
          (app.login.update_blocked ?? []).length > 0 ||
          loginReadFailed(app);
        update.disabled = updateBlockedNow();
        if (loginReadFailed(app)) update.setAttribute("aria-describedby", "login-read-error");
        let planInFlight = false;
        update.addEventListener("click", () => {
          if (loginReadFailed(app) || planInFlight) return;
          // Ignore double clicks while the first plan request is in flight.
          planInFlight = true;
          update.disabled = true;
          clear(errorBox);
          void (async () => {
            try {
              const res = await postPlan({ kind: "local", row_id: row.id });
              // A late response for a departed page must not open a dialog.
              if (!container.isConnected) return;
              if (res.result === "ready") {
                openConfirmDialog(app, res.plan_id, res.dialog, update);
              } else if (res.result === "blocked") {
                openCheckDialog(app, res.failures ?? [], [], res.gpu, update);
              } else {
                openCheckDialog(app, [], res.items ?? [], null, update);
              }
            } catch (err) {
              if (!container.isConnected) return;
              if (err instanceof ApiError) renderPlanError(app, errorBox, err, true);
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
              planInFlight = false;
              update.disabled = updateBlockedNow();
            }
          })();
        });
        rowActions.append(update);

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
      selectAll.indeterminate = someSelected && !allSelected;
      selectAll.setAttribute("aria-label", expandText("drivers.select_all", {}, lang));
      selectAll.addEventListener("change", () => {
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
      if (loginReadFailed(app)) {
        deleteSelected.setAttribute("aria-describedby", "login-read-error");
      } else if (data.delete_unavailable !== null) {
        deleteSelected.setAttribute("aria-describedby", "delete-reason");
      }
      deleteSelected.onclick = (): void => {
        openDeleteConfirm(
          rows.filter((r) => selected.has(r.id)),
          deleteSelected,
        );
      };
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
  })();
}
