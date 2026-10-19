import {
  postTask,
  ApiError,
  type GpuBlock,
  type ObjectEntry,
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
import { closeDialog, openDialog } from "../dialog.js";
import type { Lang } from "../messages.js";

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

export function gpuBlockNode(app: App, gpu: GpuBlock): HTMLElement {
  const lang = app.lang;
  const wrap = el("div", {});
  const users: ObjectEntry[] = gpu.users ?? [];
  const items = users.map((u) =>
    expandText("list.object", { name: u.name, type: { key: queryObjectKey(u.type), params: {} } }, lang),
  );
  if (gpu.state === "unreadable") {
    // Unreadable GPU usage preserves the actual reason and application list. Empty lists hide Action
    // and List without claiming the GPU is in use.
    const emptyUsers = items.length === 0;
    wrap.append(
      renderNotice(
        "notice.gpu_in_use",
        { read_reason: gpu.read_reason ?? "UNKNOWN" },
        lang,
        {
          variant: "unreadable",
          items: emptyUsers ? undefined : items,
          emptyList: emptyUsers,
        },
      ),
    );
  } else {
    const emptyUsers = items.length === 0;
    wrap.append(
      renderNotice("notice.gpu_in_use", {}, lang, {
        items: emptyUsers ? undefined : items,
        // An empty application list uses the dedicated empty-list note.
        emptyList: emptyUsers,
      }),
    );
    if (emptyUsers) {
      wrap.append(renderNotice("notice.gpu_in_use.empty_note", {}, lang));
    }
  }
  for (const e of gpu.errors ?? []) {
    wrap.append(queryFailed(queryObjectKey(e.type), e.reason, e.raw ?? null, lang));
  }
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
): void {
  const lang = app.lang;
  const dlg = el("dialog", { class: "nvm-dialog" });
  const title = el("h3", {
    class: "dialog-title",
    text: expandText("dialog.precheck", {}, lang),
  });
  dlg.append(title);
  if (failures.length > 0 || gpu) dlg.append(precheckNode(app, failures, gpu));
  if (items.length > 0) dlg.append(uncomputableNode(app, items));

  const closeBtn = el("button", { type: "button", text: expandText("precheck.close", {}, lang) });
  closeBtn.addEventListener("click", () => {
    closeDialog(dlg);
    dlg.remove();
  });
  dlg.append(closeBtn);
  document.body.append(dlg);
  openDialog(dlg, title, { trigger });
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
): void {
  const lang = app.lang;
  const dlg = el("dialog", { class: "nvm-dialog" });
  const title = el("h3", { class: "dialog-title", text: expandText("dialog.confirm", {}, lang) });
  dlg.append(title);

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
  const closeBtn = el("button", {
    type: "button",
    text: expandText("confirm.close", {}, lang),
    hidden: true,
  });
  const dismiss = (): void => {
    closeDialog(dlg);
    dlg.remove();
  };
  back.addEventListener("click", dismiss);
  closeBtn.addEventListener("click", dismiss);

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
    back.hidden = true;
    closeBtn.hidden = false;
    closeBtn.focus();
  };

  let submitting = false;
  const submit = async (button: "save" | "skip" | "start"): Promise<void> => {
    if (submitting) return;
    submitting = true;
    submitBtn.disabled = true;
    if (skipBtn) skipBtn.disabled = true;
    clear(errorBox);
    try {
      const res = await postTask(planId, button);
      closeDialog(dlg);
      dlg.remove();
      app.go(`#/tasks/${res.task_id}`);
    } catch (err) {
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
  buttons.append(back, closeBtn);
  dlg.append(buttons);

  document.body.append(dlg);
  openDialog(dlg, title, { trigger });
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
      box.append(renderNotice("upload.busy.text", {}, lang));
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
