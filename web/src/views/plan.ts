import {
  postTask,
  cancelPlan,
  ApiError,
  type GpuBlock,
  type PlanDialog,
  type PrecheckFailure,
  type UncomputableItem,
} from "../api.js";
import {
  el,
  clear,
  expandText,
  formatDate,
  isReason,
  queryFailed,
  reasonParam,
  renderNotice,
  type Params,
  type ReasonLike,
} from "../dom.js";
import type { App } from "../main.js";
import { queryObjectKey } from "../rules.js";
import { closeDialog, makeDialog } from "../dialog.js";
import type { Lang } from "../messages.js";
import { gpuUsersNode, watchGpuUsers } from "../gpu-users.js";

const UPDATE_CHECK_NOTICE_MS = 1000;

/** Show the condition check and live GPU list beside either update entry point. */
export function armUpdateCheckStatus(anchor: HTMLElement, lang: Lang, app?: App, onCancel?: () => void, announce = true, statusHost?: HTMLElement): () => void {
  let status: HTMLElement | null = null;
  let stopUsers: (() => void) | null = null;
  let detailRow: HTMLElement | null = null;
  let active = true;
  const timer = window.setTimeout(() => {
    const parent = anchor.parentElement;
    if (!active || !anchor.isConnected || !parent) return;
    status = el("div", { class: "update-check-status", role: announce ? "status" : undefined });
    const users = el("div", { class: "gpu-users gpu-users-loading" }, [
      el("p", { text: expandText("gpu.list.loading", {}, lang) }),
    ]);
    const skeleton = el("div", { class: "gpu-list-skeleton" });
    skeleton.setAttribute("aria-hidden", "true");
    users.append(skeleton);
    status.append(users);
    if (onCancel) status.append(el("button", { type: "button", text: expandText("gpu.cancel", {}, lang), onclick: onCancel }));
    const row = anchor.closest?.("tr");
    if (statusHost) {
      statusHost.append(status);
      status.scrollIntoView({ block: "nearest" });
    } else if (row?.parentElement) {
      const cell = el("td", { colSpan: row.children.length }, [status]);
      detailRow = el("tr", { class: "gpu-users-row" }, [cell]);
      row.parentElement.insertBefore(detailRow, row.nextSibling);
    } else parent.insertBefore(status, anchor.nextSibling);
    stopUsers = watchGpuUsers(users, lang, () => {
      users.className = "gpu-users";
    }, () => app?.sessionEnded());
  }, UPDATE_CHECK_NOTICE_MS);
  const stop = (): void => {
    active = false;
    window.clearTimeout(timer);
    stopUsers?.(); stopUsers = null;
    status?.remove(); status = null;
    detailRow?.remove(); detailRow = null;
  };
  app?.addCleanup(stop);
  return stop;
}

function joinReasons(reasons: ReasonLike[], lang: Lang): string {
  const sep = lang === "zh" ? ";" : "; ";
  return reasons.map((r) => expandText(r.key, (r.params ?? {}) as Params, lang)).join(sep);
}

export function failureText(app: App, f: PrecheckFailure): string {
  const lang = app.lang;
  switch (f.item) {
    case "task_running":
      return expandText("update.disabled_task_running_text", {}, lang);
    case "link_missing":
      return expandText("extension.missing", {}, lang);
    case "link_elsewhere":
      return expandText("precheck.item.link_elsewhere", { link_target: f.target ?? "" }, lang);
    case "local_checksum_mismatch":
      return expandText("precheck.item.local_checksum_mismatch", {}, lang);
    case "host_not_restored":
      return expandText(
        "precheck.item.host_not_restored",
        { reason: f.reason ?? { key: "reason.io_error", params: {} } },
        lang,
      );
    case "local_kernel_mismatch": {
      const variant =
        f.row_kind === "upload" ? "upload" : f.row_kind === "original" ? "original" : undefined;
      return expandText(
        "driver.kernel_mismatch",
        { target_kernel: f.target_kernel ?? "", current_kernel: f.current_kernel ?? "" },
        lang,
        variant,
      );
    }
    case "unreadable":
      return expandText(
        "query.failed",
        {
          query_object: { key: queryObjectKey(f.object ?? "running_task"), params: {} },
          reason: f.reason ?? { key: "reason.io_error", params: {} },
        },
        lang,
      );
    default:
      return expandText("precheck.failed", {}, lang);
  }
}

export function precheckNode(app: App, failures: PrecheckFailure[], gpu: GpuBlock | null): HTMLElement {
  const lang = app.lang;
  const wrap = el("div", {});
  if (failures.length > 0) {
    wrap.append(
      renderNotice("precheck.failed", {}, lang, {
        items: failures.map((f) => failureText(app, f)),
      }),
    );
  }
  if (gpu) wrap.append(gpuBlockNode(app, gpu));
  return wrap;
}

export function gpuBlockNode(app: App, gpu: GpuBlock, recheck = false): HTMLElement {
  const lang = app.lang;
  const wrap = el("div", {});
  if (gpu.state === "busy") wrap.append(el("p", { class: "notice-body", text: expandText("gpu.popup.waited", {}, lang) }));
  else wrap.append(renderNotice("notice.gpu_in_use", { read_reason: gpu.read_reason ?? "UNKNOWN" }, lang, {
    variant: "unreadable", omitSegments: ["title", "action", "list", "note"], role: "none",
  }));
  wrap.append(gpuUsersNode(gpu, lang, {
    snapshot: true, headline: recheck ? "gpu.popup.action" : "gpu.popup.action_task",
    unreadableHeadline: "gpu.unreadable.assumed_in_use",
  }));
  return wrap;
}

export function uncomputableNode(app: App, items: UncomputableItem[]): HTMLElement {
  return renderNotice("plan.uncomputable", {}, app.lang, {
    items: items.map((i) =>
      expandText(
        "query.failed",
        { query_object: { key: queryObjectKey(i.object), params: {} }, reason: i.reason },
        app.lang,
      ),
    ),
  });
}

export function openCheckDialog(
  app: App,
  failures: PrecheckFailure[],
  items: UncomputableItem[],
  gpu: GpuBlock | null,
  trigger: HTMLElement,
  recheck?: () => void,
): void {
  const lang = app.lang;
  const { dialog: dlg, show } = makeDialog({
    title: expandText(gpu?.state === "unreadable" ? "gpu.popup.title_unreadable" : gpu ? "gpu.popup.title" : "dialog.precheck", {}, lang),
    role: gpu ? "alertdialog" : undefined, trigger,
  });
  if (failures.length > 0) dlg.append(precheckNode(app, failures, null));
  if (gpu) dlg.append(gpuBlockNode(app, gpu, Boolean(recheck)));
  if (items.length > 0) dlg.append(uncomputableNode(app, items));

  const closeBtn = el("button", { type: "button", text: expandText("dialog.close", {}, lang) });
  closeBtn.addEventListener("click", () => {
    closeDialog(dlg);
    dlg.remove();
  });
  const buttons = el("div", { class: "actions" }, [closeBtn]);
  if (gpu && recheck) buttons.append(el("button", {
    type: "button", class: "btn-primary", text: expandText("gpu.popup.recheck", {}, lang),
    onclick: () => { closeDialog(dlg); dlg.remove(); recheck(); },
  }));
  dlg.append(buttons);
  show();
}

function sourceText(app: App, d: PlanDialog): string {
  const lang = app.lang;
  if (d.source.kind === "official") {
    return expandText("confirm.source.official", {}, lang);
  }
  if (d.source.kind === "upload") {
    return expandText(
      "confirm.source.upload",
      { fingerprint: d.source.fingerprint ?? "", checksum: d.source.sha256 ?? "" },
      lang,
    );
  }
  return expandText(
    "confirm.source.local",
    {
      // Local uploaded builds retain their known fingerprint in the source label.
      source: {
        key: `drivers.source.${d.source.row_kind ?? "official"}`,
        params: { fingerprint: d.source.fingerprint },
      },
      checksum: d.source.sha256 ?? "",
    },
    lang,
  );
}

export function openConfirmDialog(
  app: App,
  planId: string,
  d: PlanDialog,
  trigger: HTMLElement,
  navigation?: { onBack: () => void; onStarted: () => void },
): void {
  const lang = app.lang;
  const { dialog: dlg, show } = makeDialog({ title: expandText("dialog.confirm", {}, lang), trigger });
  if (d.source.kind === "upload") dlg.append(el("p", { class: "upload-complete", text: `✓ ${expandText("plan.upload_passed", {}, lang)}` }));

  const table = el("div", { class: "confirm-items" });
  const row = (label: string, value: string): void => {
    const r = el("div", { class: "confirm-row" });
    r.append(el("span", { class: "confirm-label", text: label }));
    r.append(el("span", { class: "confirm-value", text: value }));
    table.append(r);
  };
  row(expandText("confirm.source", {}, lang), sourceText(app, d));
  row(expandText("confirm.version", {}, lang), d.version);
  row(expandText("confirm.kernel", {}, lang), d.kernel);
  row(expandText("confirm.truenas", {}, lang), d.truenas_version);

  const confirmation = d.confirmation;
  const originalNote = el("div", { class: "confirm-original" });
  const originalLabel = el("span", { class: "confirm-label", text: expandText("confirm.original", {}, lang) });
  const originalValue = el("span", { class: "confirm-value" });
  if (confirmation.original === "saved" && d.original && "version" in d.original) {
    originalValue.textContent = expandText(
      "original.already_saved",
      {
        original: d.original.version ?? "UNKNOWN",
        date:
          d.original.saved_at !== undefined ? formatDate(d.original.saved_at) : "UNKNOWN",
      },
      lang,
    );
  } else {
    originalValue.textContent = expandText("confirm.original.ask", {}, lang);
  }
  originalNote.append(originalLabel, originalValue);

  const unsavable = el("p", { class: "hint", id: "confirm-unsavable" });
  originalNote.append(unsavable);

  const skipWarning = el("p", { class: "hint", id: "confirm-skip-warning" });
  originalNote.append(skipWarning);
  dlg.append(table);
  if (d.source.kind === "upload") {
    const details = el("details");
    details.append(el("summary", { text: expandText("confirm.file_details", {}, lang) }), el("p", {
      class: "summary-line", text: expandText("confirm.file_values", { fingerprint: d.source.fingerprint ?? "", checksum: d.source.sha256 ?? "" }, lang),
    }));
    dlg.append(details);
  }
  dlg.append(el("p", { class: "hint", id: "confirm-how", text: expandText(confirmation.how.key, (confirmation.how.params ?? {}) as Params, lang) }));
  for (const warning of confirmation.warnings) {
    const text = expandText(warning.key, (warning.params ?? {}) as Params, lang);
    if (warning.key === "confirm.original.unsavable") unsavable.textContent = text;
    else if (warning.key === "original.skip_warning") skipWarning.textContent = text;
    else dlg.append(el("p", { class: "hint", text }));
  }
  dlg.append(originalNote);

  const errorBox = el("div", { class: "dialog-error" });
  dlg.append(errorBox);

  const buttons = el("div", { class: "dialog-buttons" });
  const back = el("button", { type: "button", text: expandText("confirm.back", {}, lang) });
  let dismissing = false;
  const dismiss = (): void => {
    if (submitting || dismissing) return;
    dismissing = true;
    back.disabled = true;
    void cancelPlan(planId).then(() => {
      closeDialog(dlg);
      dlg.remove();
      navigation?.onBack();
    }).catch((err: unknown) => {
      if (err instanceof ApiError && err.isSessionEnded) app.sessionEnded();
      else errorBox.append(renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang));
    }).finally(() => { dismissing = false; back.disabled = false; });
  };
  back.addEventListener("click", dismiss);

  const submitBtn = el("button", {
    type: "button",
    class: "btn-primary",
    text: expandText(`confirm.${confirmation.buttons[0]?.button ?? "save"}`, {}, lang),
  });
  let skipBtn: HTMLButtonElement | null = null;

  // A rejected submission spends the plan; only Close remains. Move focus there so it is not left on a
  // hidden control.
  const retire = (): void => {
    submitBtn.hidden = true;
    if (skipBtn) skipBtn.hidden = true;
    back.textContent = expandText("dialog.close", {}, lang);
    back.disabled = false;
    back.focus();
  };

  let submitting = false;
  dlg.addEventListener("cancel", (event) => { event.preventDefault(); event.stopImmediatePropagation(); dismiss(); });
  const submit = async (button: "save" | "skip" | "start"): Promise<void> => {
    if (submitting || dismissing) return;
    submitting = true;
    back.disabled = true;
    submitBtn.disabled = true;
    if (skipBtn) skipBtn.disabled = true;
    clear(errorBox);
    try {
      const res = await postTask(planId, button);
      closeDialog(dlg);
      dlg.remove();
      navigation?.onStarted();
      app.go(`#/tasks/${res.task_id}`);
    } catch (err) {
      submitting = false;
      if (err instanceof ApiError) {
        renderPlanError(app, errorBox, err, false);
        retire();
      } else {
        errorBox.append(
          renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang),
        );
        retire();
      }
    }
  };

  for (const [index, choice] of confirmation.buttons.entries()) {
    const control = index === 0 ? submitBtn : el("button", {
      type: "button", text: expandText(`confirm.${choice.button}`, {}, lang),
    });
    control.disabled = !choice.enabled;
    if (choice.button === "skip") {
      control.setAttribute("aria-describedby", "confirm-skip-warning");
      skipBtn = control;
    } else if (!choice.enabled) {
      control.setAttribute("aria-describedby", "confirm-unsavable");
    }
    control.addEventListener("click", () => void submit(choice.button));
    buttons.append(control);
  }
  buttons.append(back);
  dlg.append(buttons);

  show();
}

export function renderPlanError(app: App, box: HTMLElement, err: ApiError, afterOpen: boolean): void {
  const lang = app.lang;
  if (err.isSessionEnded) {

    app.sessionEnded();
    return;
  }
  const params: Params = { ...err.params };
  switch (err.key) {
    case "submit.failed": {
      // The catalog expects submit_reason; the API sends reason. Render raw command output outside the live region.
      const field = params["reason"] ?? params["submit_reason"];
      params["submit_reason"] =
        typeof field === "string"
          ? field
          : reasonParam(isReason(field) ? field : null, null, lang);
      box.append(renderNotice("submit.failed", params, lang));
      if (err.raw) box.append(el("pre", { class: "raw-output", text: err.raw }));
      break;
    }
    case "plan.changed": {
      const changed = (params["changed"] ?? []) as string[];
      const items = changed.map((c) => ({ key: `plan.changed.item.${c}`, params: {} }));
      const variant = afterOpen ? "after_open" : changed.length === 1 ? "one" : undefined;
      box.append(renderNotice("plan.changed", { changed_items: items }, lang, { variant }));
      break;
    }
    case "plan.source_invalid.upload_incomplete": {
      // Fill packages from the real missing-package list.
      box.append(
        renderNotice(
          "plan.source_invalid.upload_incomplete",
          { packages: params["missing"] ?? [] },
          lang,
        ),
      );
      break;
    }
    case "plan.source_invalid.branch_unbuildable": {
      const reasons = (params["reasons"] ?? []) as ReasonLike[];
      box.append(
        renderNotice(
          "plan.source_invalid.branch_unbuildable",
          { branch: params["branch"] ?? "", unbuildable_reason: joinReasons(reasons, lang) },
          lang,
        ),
      );
      break;
    }
    case "plan.source_invalid.branch_undecidable":
      box.append(renderNotice("plan.source_invalid.branch_undecidable", {}, lang));
      appendUnreadable(app, box, params);
      break;
    case "plan.source_invalid.gpu_undecidable":
      appendUnreadable(app, box, params);
      break;
    case "plan.source_invalid.gpu_unsupported": {
      const gpus = (params["gpus"] ?? []) as {
        device: string;
        name?: string;
        last_branch?: number;
        first_branch?: number;
      }[];
      for (const g of gpus) {
        const gpuName = g.name && g.name !== "" ? g.name : `10de:${g.device}`;
        // A catalog key used in a plan rejection must remain in the error region.
        if (g.last_branch !== undefined) {
          box.append(
            renderNotice(
              "catalog.gpu.not_listed",
              { gpu_name: gpuName, branch: g.last_branch },
              lang,
              { level: "error" },
            ),
          );
        }
        if (g.first_branch !== undefined) {
          box.append(
            renderNotice(
              "catalog.gpu.not_listed",
              { gpu_name: gpuName, branch: g.first_branch },
              lang,
              { variant: "from", level: "error" },
            ),
          );
        }
      }
      // A single unsupported branch still leaves other versions usable; only a true module-kind
      // conflict blocks them all.
      if (params["conflict"] === true) {
        box.append(renderNotice("catalog.no_usable_version", {}, lang));
      }
      break;
    }
    case "plan.precheck_failed": {
      const failures = (params["failures"] ?? []) as PrecheckFailure[];
      box.append(
        renderNotice("precheck.failed", {}, lang, {
          items: failures.map((f) => failureText(app, f)),
        }),
      );
      break;
    }
    case "plan.uncomputable":
      box.append(uncomputableNode(app, (params["items"] ?? []) as UncomputableItem[]));
      break;
    case "task.running":
      box.append(renderNotice("update.disabled_task_running_text", {}, lang));
      break;
    case "upload.busy":
      box.append(renderNotice("upload.busy", {}, lang));
      break;
    case "plan.source_invalid.toolkit_too_old":
      box.append(renderNotice("upload.group.toolkit_too_old", params, lang));
      break;
    default:
      box.append(renderNotice(err.key, params, lang));
      if (err.raw) box.append(el("pre", { class: "raw-output", text: err.raw }));
      break;
  }
}

function appendUnreadable(app: App, box: HTMLElement, params: Params): void {
  for (const u of (params["unreadable"] ?? []) as {
    object: string;
    reason: ReasonLike;
    raw?: string;
  }[]) {
    box.append(queryFailed(queryObjectKey(u.object), u.reason, u.raw ?? null, app.lang));
  }
}
