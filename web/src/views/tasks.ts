// Polling must not announce raw output or logs. Rewrite the status region only when its facts change.

import {
  ApiError,
  getTask,
  getTasks,
  releaseTask,
  isUnknown,
  type LogLine,
  type Reading,
  type TaskDetail,
  type TaskListItem,
} from "../api.js";
import {
  el,
  clear,
  expandText,
  formatClock,
  formatDate,
  formatMB,
  formatTaskTime,
  queryFailed,
  renderNotice,
  apiErrorNotice,
  type Params,
} from "../dom.js";
import { closeDialog, openDialog } from "../dialog.js";
import type { App } from "../main.js";
import { MESSAGES } from "../messages.js";
import {
  checkFailureItems,
  checkItems,
  checkReasonText,
  stateReadErrorReasonKey,
  noOriginalReasonParam,
  normalizeReason,
  splitRawOutput,
  QUERY_OBJECT_HOST_TASK_UNIT,
  QUERY_OBJECT_RUNNING_TASK,
  queryObjectKey,
  refreshParams,
  restoreFailureReason,
  taskNoticeParams,
  versionObserved,
} from "../rules.js";
import { startPolling, POLL_MS, type Poller } from "../poll.js";
import { plannedSteps, type StepPlan } from "../task-steps.js";

function taskTimeText(ms: number | null | undefined, lang: App["lang"]): string {
  const text = formatTaskTime(ms);
  return text === "UNKNOWN" ? expandText("tasks.time.unavailable", {}, lang) : text;
}

function finishedText(status: string, ms: number | null | undefined, lang: App["lang"]): string {
  if (status === "RUNNING" && (ms === null || ms === undefined)) {
    return expandText("tasks.time.not_finished", {}, lang);
  }
  return taskTimeText(ms, lang);
}

function discoveredText(
  status: string,
  ms: number | null | undefined,
  lang: App["lang"],
): string {
  if (typeof ms === "number") return taskTimeText(ms, lang);
  if (status === "INTERRUPTED") return expandText("tasks.time.unavailable", {}, lang);
  return expandText("tasks.time.not_interrupted", {}, lang);
}

function asStepPlan(plan: TaskDetail["plan"]): StepPlan | null {
  if (!plan) return null;
  const source = plan["source"];
  let kind: string | undefined;
  if (source && typeof source === "object" && "kind" in source) {
    const value = (source as { kind?: unknown }).kind;
    if (typeof value === "string") kind = value;
  }
  const original = plan["original"];
  return {
    source: kind ? { kind } : null,
    original: typeof original === "string" ? original : null,
  };
}

const TERMINAL: readonly string[] = ["SUCCEEDED", "STOCK_RESTORED", "FAILED", "INTERRUPTED"];

function sourceText(app: App, source: TaskListItem["source"]): string {
  const lang = app.lang;
  if (!source) return expandText("task.value.unavailable", {}, lang);
  if (source.kind === "upload") {
    return expandText("tasks.source.upload", { fingerprint: source.fingerprint ?? "" }, lang);
  }
  if (source.kind === "local") {

    return expandText(
      "tasks.source.local",
      {
        source: {
          key: `drivers.source.${source.row_kind ?? "official"}`,
          params: { fingerprint: source.fingerprint },
        },
      },
      lang,
    );
  }
  return expandText("tasks.source.official", {}, lang);
}

export function renderTasks(app: App, container: HTMLElement): void {
  const lang = app.lang;
  void (async () => {
    let data: Awaited<ReturnType<typeof getTasks>>;
    try {
      data = await getTasks();
    } catch (err) {
      // Late responses cannot update a departed page or end a subsequent session.
      if (!container.isConnected) return;
      if (err instanceof ApiError && err.isSessionEnded) {
        app.sessionEnded();
        return;
      }
      // A failed task-list read confirms nothing about host task state.
      app.reportTaskState(null, "global");

      container.append(apiErrorNotice(err, lang));
      return;
    }
    container.append(el("h2", { text: expandText("nav.tasks", {}, lang) }));

    // Only confirmed RUNNING tasks claim the dot by task id; late responses cannot revive a departed page.
    if (!container.isConnected) return;
    if (data.active_task !== null && "task_id" in data.active_task && data.active_task.status === "RUNNING") {
      app.reportTaskState("running", "global", data.active_task.task_id);
    } else {
      app.reportTaskState(null, "global");
    }

    if (data.unit_query) {
      container.append(
        queryFailed(QUERY_OBJECT_HOST_TASK_UNIT, data.unit_query.reason, null, lang),
      );
    }
    if (data.state_dir) {
      container.append(
        queryFailed(QUERY_OBJECT_RUNNING_TASK, data.state_dir.reason, null, lang),
      );
    }
    if (data.active_task && "unknown" in data.active_task) {
      container.append(
        queryFailed(QUERY_OBJECT_RUNNING_TASK, data.active_task.unknown, null, lang),
      );
    }

    if (data.tasks.length === 0) {
      container.append(el("p", { text: expandText("task.none", {}, lang) }));
      return;
    }

    const table = el("table", { class: "tasks" });
    const head = el("tr", {});
    for (const key of [
      "tasks.col.id",
      "tasks.col.status",
      "tasks.col.phase",
      "tasks.col.source",
      "tasks.col.version",
      "tasks.col.started",
      "tasks.col.finished",
      "tasks.col.discovered",
    ]) {
      head.append(el("th", { scope: "col", text: expandText(key, {}, lang) }));
    }
    const thead = el("thead", {});
    thead.append(head);
    const tbody = el("tbody", {});
    for (const t of data.tasks) {
      const tr = el("tr", {});
      const idCell = el("td", {});
      idCell.append(el("a", { href: `#/tasks/${t.task_id}`, text: t.task_id }));
      tr.append(idCell);
      tr.append(el("td", { text: expandText(`status.${t.status}`, {}, lang) }));
      tr.append(
        el("td", {
          text: t.phase ? expandText(`phase.${t.phase}`, {}, lang) : expandText("task.value.unavailable", {}, lang),
        }),
      );
      tr.append(el("td", { text: sourceText(app, t.source) }));
      tr.append(el("td", { text: t.version ?? expandText("task.value.unavailable", {}, lang) }));
      tr.append(el("td", { text: taskTimeText(t.created_at, lang) }));
      tr.append(el("td", { text: finishedText(t.status, t.finished_at, lang) }));
      tr.append(el("td", { text: discoveredText(t.status, t.interrupt_discovered_at, lang) }));
      tbody.append(tr);
      if (t.state_read_error) {
        const detail = el("tr", {});
        const cell = el("td", { colSpan: 8 });
        cell.append(
          renderNotice("task.state_unreadable", {
            reason: { key: stateReadErrorReasonKey(t.state_read_error), params: {} },
          }, lang),
        );
        detail.append(cell);
        tbody.append(detail);
      }
    }
    table.append(thead, tbody);
    container.append(table);
    container.append(el("p", { class: "hint", text: expandText("tasks.time.basis", {}, lang) }));
  })();
}

function exitField(exit: unknown): unknown {
  const e = logRecord(exit);
  if (e === null || Object.keys(e).length !== 1) return "UNKNOWN";
  if (typeof e["code"] === "number" && Number.isInteger(e["code"])) {
    return { key: "log.exit.code", params: { code: e["code"] } };
  }
  if (typeof e["signal"] === "number" && Number.isInteger(e["signal"])) {
    return { key: "log.exit.signal", params: { signal: e["signal"] } };
  }
  if (e["timed_out"] === true) return { key: "log.exit.timed_out", params: {} };
  if (e["spawn_failed"] === true) return { key: "log.exit.spawn_failed", params: {} };
  return "UNKNOWN";
}

// Only explicit not_loaded means absent. Missing, malformed and unknown readings must not invent absence.
function logReadingNumber(v: unknown): Reading<number> {
  if (v && typeof v === "object" && "value" in v && typeof (v as { value?: unknown }).value === "number") {
    return { value: (v as { value: number }).value };
  }
  if (isUnknown(v)) {
    return { unknown: v.unknown };
  }
  if (v && typeof v === "object" && (v as { not_loaded?: unknown }).not_loaded === true) {
    return { not_loaded: true };
  }
  return { unknown: { key: "reason.io_error", params: {} } };
}

function logReadingString(v: unknown): Reading<string> {
  if (v && typeof v === "object" && "value" in v && typeof (v as { value?: unknown }).value === "string") {
    return { value: (v as { value: string }).value };
  }
  if (isUnknown(v)) {
    return { unknown: v.unknown };
  }
  if (v && typeof v === "object" && (v as { not_loaded?: unknown }).not_loaded === true) {
    return { not_loaded: true };
  }
  return { unknown: { key: "reason.io_error", params: {} } };
}

const CLEANUP_STEP_KEYS: Record<string, string> = {
  delete_temp_file: "log.cleanup_step.delete_temp",
  unmount_writable_mount: "log.cleanup_step.unmount",
  restore_readonly: "log.cleanup_step.readonly",
  remove_build_container: "log.cleanup_step.remove_container",
  delete_state_temps: "log.cleanup_step.state_tmp",
  delete_task_dir: "log.cleanup_step.task_dir",
};

function logRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function logKey(prefix: string, value: unknown): unknown {
  if (typeof value !== "string") return "UNKNOWN";
  const key = `${prefix}${value}`;
  return Object.hasOwn(MESSAGES, key) ? { key, params: {} } : "UNKNOWN";
}

function logReasonKey(prefix: string, key: unknown): key is string {
  return typeof key === "string" && key.startsWith(prefix) && Object.hasOwn(MESSAGES, key);
}

function gpuUserType(kind: unknown): unknown {
  return kind === "docker" || kind === "incus"
    ? { key: queryObjectKey(kind), params: {} }
    : "UNKNOWN";
}

export function logParams(key: string, payload: unknown): Params {
  // Log payloads may be arbitrary JSON; only objects can supply named placeholders, and missing values
  // remain UNKNOWN.
  const p = logRecord(payload);
  if (p === null) return {};
  const params: Params = { ...p };
  switch (key) {
    case "log.cmd": {
      const argv = p["argv"];
      params["command"] =
        Array.isArray(argv) && argv.length > 0 && argv.every((arg) => typeof arg === "string")
          ? argv.join(" ")
          : "UNKNOWN";
      params["exit"] = exitField(p["exit"]);
      break;
    }
    case "log.phase_enter":
      params["phase"] = logKey("phase.", p["phase"]);
      break;
    case "log.finished":
      params["status"] = logKey("status.", p["status"]);
      break;
    case "log.build_step":

      params["build_step"] = logKey("log.build_step.", p["build_step"]);
      break;
    case "log.build_install_mode":
      // The build script writes two-pass; the catalog suffix is two_pass.
      params["install_mode"] = logKey(
        "log.install_mode.",
        p["mode"] === "two-pass" ? "two_pass" : p["mode"],
      );
      break;
    case "log.occupancy":
      params["usage_result"] = logKey("log.usage.", p["usage_result"]);
      break;
    case "log.occupancy_wait":
      params["result"] = logKey("log.occupancy_wait.", p["result"]);
      break;
    case "log.module_unload_wait":
      params["result"] = logKey("log.unload_wait.", p["result"]);
      break;
    case "log.build_deadline_set":
      // The runner deadline_ms field renders as the same local time format as other dates.
      params["time"] =
        typeof p["deadline_ms"] === "number" && Number.isFinite(p["deadline_ms"])
          ? formatDate(p["deadline_ms"] as number)
          : "UNKNOWN";
      break;
    case "log.gpu_users": {

      const apps = Array.isArray(p["applications"]) ? (p["applications"] as unknown[]) : null;
      const errors = Array.isArray(p["errors"]) ? (p["errors"] as unknown[]) : null;
      const list: unknown[] = [];
      if (apps === null) list.push("UNKNOWN");
      for (const item of apps ?? []) {
        const a = logRecord(item);
        if (a === null) {
          list.push("UNKNOWN");
          continue;
        }
        list.push({
          key: "log.applications.entry",
          params: {
            name: typeof a["name"] === "string" && a["name"] !== "" ? a["name"] : "UNKNOWN",
            type: gpuUserType(a["kind"]),
          },
        });
      }
      if (errors === null) list.push("UNKNOWN");
      for (const item of errors ?? []) {
        const e = logRecord(item);
        if (e === null) {
          list.push("UNKNOWN");
          continue;
        }
        list.push({
          key: "log.applications.query_failed",
          params: { type: gpuUserType(e["kind"]) },
        });
      }
      params["applications"] = list.length > 0 ? list : { key: "log.applications.none", params: {} };
      break;
    }
    case "log.readback":
      // A null readback_ok means hashing failed, never a checksum mismatch.
      params["readback_result"] =
        typeof p["readback_result"] === "boolean"
          ? {
              key: p["readback_result"] ? "log.readback.match" : "log.readback.mismatch",
              params: {},
            }
          : "UNKNOWN";
      break;
    case "log.check_item": {

      const refresh = p["check_item"] === "refresh";
      const version = p["check_item"] === "version";
      params["check_item"] = refresh || version
        ? { key: refresh ? "check.item.refresh" : "check.item.version", params: {} }
        : "UNKNOWN";
      params["check_result"] =
        typeof p["passed"] === "boolean"
          ? { key: p["passed"] ? "check.pass" : "check.fail", params: {} }
          : "UNKNOWN";
      if (refresh) {
        Object.assign(params, refreshParams({
          since_before: logReadingNumber(p["since_before"]),
          since_after: logReadingNumber(p["since_after"]),
          refresh_ok: typeof p["passed"] === "boolean" ? p["passed"] : undefined,
        }));
      } else if (version) {
        params["observed"] = versionObserved(
          logReadingString(p["nvidia_smi"]),
          logReadingString(p["proc_version"]),
        );
        params["expected"] = {
          key: "check.expected.version",
          params: {
            target: typeof p["expected"] === "string" && p["expected"] !== "" ? p["expected"] : "UNKNOWN",
          },
        };
      } else {
        params["observed"] = "UNKNOWN";
        params["expected"] = "UNKNOWN";
      }
      break;
    }
    case "log.loaded_version_unknown": {
      // Expand string read-reason codes through the reason catalog instead of exposing internal keys.
      const code = p["read_reason"];
      params["read_reason"] =
        logReasonKey("reason.", code) ? normalizeReason(code, {}) : "UNKNOWN";
      break;
    }
    case "log.restore_none": {
      // Restore reasons belong to restore.no_original.*, separate from state-read reasons. Preserve
      // nested read_reason in either string-key or structured form.
      const reason = p["reason"];
      const field =
        typeof reason === "string" && reason !== ""
          ? { key: reason, params: {} as Params }
          : reason && typeof reason === "object" && typeof (reason as { key?: unknown }).key === "string"
            ? {
                key: String((reason as { key: string }).key),
                params: logRecord((reason as { params?: unknown }).params) ?? {},
              }
            : null;
      const read = p["read_reason"];
      if (field === null || !logReasonKey("restore.no_original.", field.key)) {
        params["reason"] = "UNKNOWN";
      } else {
        const readReason = field.params["read_reason"] ?? read;
        if (readReason !== undefined) {
          const readField = logRecord(readReason);
          field.params["read_reason"] =
            logReasonKey("reason.", readReason)
              ? normalizeReason(readReason, {})
              : readField !== null && logReasonKey("reason.", readField["key"])
                ? { key: readField["key"], params: logRecord(readField["params"]) ?? {} }
                : "UNKNOWN";
        }
        params["reason"] = field;
      }
      break;
    }
    case "log.artifact_check_failed": {
      // Build reasons preserve producer parameters, including observed versions and kernels; bare
      // codes use the same reason catalog.
      const field = p["build_reason"];
      if (typeof field === "string" && field !== "") {
        const reason = normalizeReason(field, {});
        params["build_reason"] = logReasonKey("build.artifact.", reason.key) ? reason : "UNKNOWN";
      } else if (
        field &&
        typeof field === "object" &&
        typeof (field as { key?: unknown }).key === "string"
      ) {
        const reason = normalizeReason(
          String((field as { key: string }).key),
          logRecord((field as { params?: unknown }).params) ?? {},
        );
        params["build_reason"] = logReasonKey("build.artifact.", reason.key) ? reason : "UNKNOWN";
      } else {
        params["build_reason"] = "UNKNOWN";
      }
      break;
    }
    case "log.build_kit_cache": {
      const field = logRecord(p["reason"]);
      const reasonParams = logRecord(field?.["params"]);
      params["reason"] =
        field && reasonParams && logReasonKey("reason.", field["key"])
          ? { key: field["key"], params: reasonParams }
          : "UNKNOWN";
      break;
    }
    case "log.cleanup_failed": {
      // Cleanup step names come from the closed cleanup catalog; command reasons use the general reason catalog.
      const step = typeof p["cleanup_step"] === "string" ? (p["cleanup_step"] as string) : "";
      const stepKey = CLEANUP_STEP_KEYS[step];
      params["cleanup_step"] =
        stepKey !== undefined ? { key: stepKey, params: {} } : "UNKNOWN";
      const field = logRecord(p["reason"]);
      const reasonParams = logRecord(field?.["params"]);
      params["reason"] = field && reasonParams && logReasonKey("reason.", field["key"])
        ? { key: field["key"], params: reasonParams }
        : typeof p["reason"] === "string" && p["reason"] !== "" ? p["reason"] : "UNKNOWN";
      break;
    }
    case "log.row_created": {
      // Use the published row kind and upload fingerprint; missing sources must not be guessed as official.
      const kind = typeof p["source"] === "string" ? (p["source"] as string) : "";
      params["source"] =
        kind === "official" || kind === "upload"
          ? { key: `drivers.source.${kind}`, params: { fingerprint: kind === "official" ? "" : typeof p["fingerprint"] === "string" && p["fingerprint"] !== "" ? p["fingerprint"] : "UNKNOWN" } }
          : "UNKNOWN";
      break;
    }
    default:
      break;
  }
  return params;
}

function logLineText(app: App, line: LogLine): string {
  let body: string;
  try {
    // A log key outside the task-log catalog cannot borrow another page message.
    body = line.key.startsWith("log.") && Object.hasOwn(MESSAGES, line.key)
      ? expandText(line.key, logParams(line.key, line.p), app.lang)
      : "UNKNOWN";
    if (line.key === "log.cmd") {
      const p = logRecord(line.p);
      for (const stream of [p?.["stdout"], p?.["stderr"]]) {
        if (typeof stream === "string" && stream !== "") body += `\n${stream}`;
      }
    }
  } catch {
    // A malformed log payload must not hide task status or later log entries.
    body = "UNKNOWN";
  }
  return `${formatClock(line.t)} ${body}`;
}

export function gpuDiagnosticText(reading: TaskDetail["nvidia_smi_gpus"], lang: App["lang"]): string {
  if (!reading) return expandText("task.gpus.not_read", {}, lang);
  if (isUnknown(reading)) return expandText("task.gpus.unreadable", { reason: normalizeReason(reading.unknown.key, reading.unknown.params) }, lang);
  if (!("value" in reading) || reading.value.length === 0) return expandText("task.gpus.none", {}, lang);
  return reading.value.map((gpu) => `${gpu.name} (${gpu.pci_address})`).join("; ");
}

/** The waiting line for a running task, or null when the task is not waiting. */
export function waitingGpuMessageKey(detail: {
  status: string;
  phase: string | null;
  waiting_gpu?: boolean;
}): string | null {
  if (detail.status !== "RUNNING" || detail.waiting_gpu !== true) return null;
  if (detail.phase === "align_modules") return "task.waiting_gpu.align";
  if (detail.phase === "restore_unload" || detail.phase === "restore_align_modules") {
    return "task.waiting_gpu.restore";
  }
  return "task.waiting_gpu";
}

export function renderTask(app: App, container: HTMLElement, taskId: string): void {
  const lang = app.lang;
  let poller: Poller | null = null;

  const heading = el("h2", { text: expandText("page.task", { task_id: taskId }, lang) });
  const statusArea = el("div", { class: "task-state", role: "status" });
  const statusLabel = el("span", { class: "reading-label", text: expandText("task.status", {}, lang) });
  const statusValue = el("span", { class: "reading-value" });
  const phaseLabel = el("span", { class: "reading-label", text: expandText("task.phase", {}, lang) });
  const phaseValue = el("span", { class: "reading-value" });
  const waitLine = el("p", { class: "task-wait", hidden: true });
  statusArea.append(
    el("div", { class: "reading" }, [statusLabel, statusValue]),
    el("div", { class: "reading" }, [phaseLabel, phaseValue]),
    waitLine,
  );
  const startedValue = el("span", { class: "reading-value" });
  const finishedValue = el("span", { class: "reading-value" });
  const discoveredValue = el("span", { class: "reading-value" });
  const times = el("div", { class: "task-times" });
  for (const [label, value] of [
    ["tasks.col.started", startedValue],
    ["tasks.col.finished", finishedValue],
    ["tasks.col.discovered", discoveredValue],
  ] as const) {
    const line = el("div", { class: "reading" });
    line.append(el("span", { class: "reading-label", text: expandText(label, {}, lang) }));
    line.append(value);
    times.append(line);
  }
  const basis = el("p", { class: "hint", text: expandText("tasks.time.basis", {}, lang) });
  const gpuValue = el("span", { class: "reading-value" });
  const gpuDiagnostic = el("div", { class: "reading task-gpu-diagnostic" }, [
    el("span", { class: "reading-label", text: expandText("task.gpus", {}, lang) }),
    gpuValue,
  ]);
  const stepArea = el("div", { class: "task-steps-wrap" });

  const recoveryArea = el("div", { class: "task-recovery" });
  const downloadArea = el("div", { class: "task-download" });
  const noticeArea = el("div", { class: "task-notice" });
  // Read errors stay next to status notices until a later read succeeds.
  const readErrorArea = el("div", { class: "task-read-error" });
  const logArea = el("div", { class: "task-log" });
  const logPre = el("pre", { class: "log-tail" });
  logArea.append(el("h3", { text: expandText("task.log", {}, lang) }), logPre);

  container.append(heading, statusArea, times, basis, gpuDiagnostic, stepArea, recoveryArea, downloadArea, noticeArea, readErrorArea, logArea);
  const terminalExtra = el("div", { class: "task-terminal" });
  container.append(terminalExtra);

  statusArea.tabIndex = -1;
  statusArea.focus();
  // Page redraws stop this page poller.
  let releaseDialog: HTMLDialogElement | null = null;
  app.addCleanup(() => {
    poller?.stop();
    if (releaseDialog?.open) releaseDialog.close();
    releaseDialog?.remove();
  });

  let recoverySignature: string | null = null;
  const renderRecovery = (detail: TaskDetail): void => {
    const signature = JSON.stringify([detail.status, detail.recovery_material_kept, detail.released_by, detail.released_at]);
    if (signature === recoverySignature) return;
    recoverySignature = signature;
    clear(recoveryArea);
    if (detail.released_by) {
      recoveryArea.append(renderNotice(
        detail.recovery_material_kept ? "task.release.pending" : "task.release.done",
        { task_id: detail.released_by, time: taskTimeText(detail.released_at, lang) }, lang,
      ));
    }
    if (!TERMINAL.includes(detail.status) || detail.recovery_material_kept !== true) return;
    const releaseButton = el("button", {
      type: "button", text: expandText("task.release.button", {}, lang),
    });
    releaseButton.addEventListener("click", () => {
      if (releaseDialog?.isConnected) return;
      const dlg = el("dialog", { class: "nvm-dialog" });
      releaseDialog = dlg;
      const title = el("h3", {
        class: "dialog-title", text: expandText("task.release.confirm_title", {}, lang),
      });
      const errorArea = el("div", { class: "dialog-error" });
      const confirm = el("button", {
        type: "button", class: "btn-danger", text: expandText("task.release.button", {}, lang),
      });
      const cancel = el("button", {
        type: "button", text: expandText("delete.cancel_button", {}, lang),
      });
      let pending = false;
      const dismiss = (): void => {
        closeDialog(dlg);
        dlg.remove();
        releaseDialog = null;
      };
      cancel.addEventListener("click", dismiss);
      dlg.addEventListener("cancel", (event) => {
        if (pending) {
          event.preventDefault();
          event.stopImmediatePropagation();
        }
      });
      confirm.addEventListener("click", () => {
        if (pending) return;
        pending = true;
        confirm.disabled = true;
        cancel.disabled = true;
        clear(errorArea);
        void (async () => {
          try {
            const released = await releaseTask(taskId);
            if (!container.isConnected) return;
            dismiss();
            renderRecovery(released);
            statusArea.focus();
          } catch (err) {
            if (!container.isConnected) return;
            if (err instanceof ApiError && err.isSessionEnded) {
              dismiss();
              app.sessionEnded();
              return;
            }
            errorArea.append(apiErrorNotice(err, lang));
          } finally {
            pending = false;
            confirm.disabled = false;
            cancel.disabled = false;
          }
        })();
      });
      dlg.append(title, el("p", { text: expandText("task.release.confirm_body", {}, lang) }), errorArea, confirm, cancel);
      document.body.append(dlg);
      openDialog(dlg, title, { trigger: releaseButton });
    });
    recoveryArea.append(releaseButton);
  };

  void (async () => {
    let after: number | undefined;
    let loginRefreshed = false;
    let noticeSignature: string | null = null;
    let terminalSignature: string | null = null;
    let stepSignature: string | null = null;
    const setText = (node: HTMLElement, value: string): void => {
      if (node.textContent !== value) node.textContent = value;
    };
    const read = async (): Promise<void> => {
      let detail: TaskDetail;
      try {
        detail = await getTask(taskId, after);
      } catch (err) {
        // Late responses cannot update a departed page or end a subsequent session.
        if (!container.isConnected) return;
        if (err instanceof ApiError && err.isSessionEnded) {
          poller?.stop();
          app.sessionEnded();
          return;
        }
        if (err instanceof ApiError && err.status === 404) {
          clear(noticeArea);
          noticeArea.append(renderNotice("not_found", {}, lang));
          clear(readErrorArea);
          poller?.stop();

          app.reportTaskState(null, "task", taskId);
          return;
        }

        clear(readErrorArea);
        readErrorArea.append(apiErrorNotice(err, lang));
        app.reportTaskState(null, "task", taskId);
        return;
      }
      if (readErrorArea.firstChild) clear(readErrorArea);
      // All response side effects, including the terminal login refresh, belong to the still-live page.
      if (!container.isConnected) return;
      renderRecovery(detail);
      setText(gpuValue, gpuDiagnosticText(detail.nvidia_smi_gpus, lang));
      after = detail.log.next;
      // Only RUNNING claims the dot for this task. Uncertainty clears this task claim without
      // affecting a different task.
      app.reportTaskState(detail.status === "RUNNING" ? "running" : null, "task", taskId);
      // An unchanged poll must not rewrite the status live region.
      setText(statusValue, expandText(`status.${detail.status}`, {}, lang));
      setText(phaseValue, detail.phase ? expandText(`phase.${detail.phase}`, {}, lang) : expandText("task.value.unavailable", {}, lang));
      const waitKey = waitingGpuMessageKey(detail);
      const waitText = waitKey ? expandText(waitKey, {}, lang) : "";
      if (waitLine.textContent !== waitText) {
        setText(waitLine, waitText);
        if (waitText === "") waitLine.setAttribute("hidden", "");
        else waitLine.removeAttribute("hidden");
      }
      setText(startedValue, taskTimeText(detail.created_at, lang));
      setText(finishedValue, finishedText(detail.status, detail.finished_at, lang));
      setText(discoveredValue, discoveredText(detail.status, detail.interrupt_discovered_at, lang));
      const restoring =
        (detail.phase ?? "").startsWith("restore_") || detail.restore?.decision === "restore";
      const steps = plannedSteps(asStepPlan(detail.plan), restoring, {
        update: detail.phase === "align_modules" || detail.replace.some((r) => r.pass === "update" && r.module_correction),
        restore: detail.phase === "restore_align_modules" || detail.replace.some((r) => r.pass === "restore" && r.module_correction),
      });
      const stepSig = JSON.stringify([steps, detail.phase]);
      if (stepSig !== stepSignature) {
        stepSignature = stepSig;
        clear(stepArea);
        if (steps.length > 0) {
          stepArea.append(el("h3", { text: expandText("tasks.steps", {}, lang) }));
          const list = el("ol", { class: "task-steps" });
          for (const step of steps) {
            const item = el("li", { text: expandText(`phase.${step}`, {}, lang) });
            if (detail.phase === step) item.setAttribute("aria-current", "step");
            list.append(item);
          }
          stepArea.append(list);
        }
      }

      if (detail.download && detail.download.total_bytes !== undefined) {
        downloadArea.textContent = expandText(
          "download.progress",
          {
            done: formatMB(detail.download.done_bytes ?? 0),
            total: formatMB(detail.download.total_bytes),
          },
          lang,
        );
      }

      if (detail.log.reset) clear(logPre);
      for (const line of detail.log.lines) {
        logPre.append(document.createTextNode(`${logLineText(app, line)}\n`));
      }

      // Rebuild notices only when the rendered facts change.
      const users = detail.gpu_users?.users ?? [];
      const items = users.map((u) =>
        expandText(
          "list.object",
          { name: u.name, type: { key: `query.object.${u.type}`, params: {} } },
          lang,
        ),
      );
      const noticeSig = JSON.stringify([
        detail.notice,
        detail.gpu_users,
        detail.version_change,
        detail.loaded_version_start,
        detail.loaded_version_end,
      ]);
      if (noticeSig !== noticeSignature) {
        noticeSignature = noticeSig;
        clear(noticeArea);
        if (detail.notice) {
          const noticeKey = detail.notice.key;
          const variant = String(detail.notice.variant ?? "");
          const unreadable = variant.endsWith("unreadable");
          const gpuListNotice = [
            "notice.gpu_in_use",
            "notice.unload_refused",
            "notice.restore_stopped",
          ].includes(noticeKey);
          const emptyUsers = items.length === 0;
          // Empty GPU-use and unload-refused notices suppress Action, List and Note; restore-stopped
          // keeps its Local-drivers action. Unreadable notices never show the empty-list note.
          const hideList = gpuListNotice && emptyUsers && noticeKey !== "notice.restore_stopped";
          const showEmptyNote = hideList && !unreadable;
          // Raw command output stays in pre.raw-output outside every alert/status region.
          const split = splitRawOutput(taskNoticeParams(detail, lang), detail.error?.exit);
          noticeArea.append(
            renderNotice(noticeKey, split.params, lang, {
              variant: detail.notice.variant,
              items: emptyUsers ? undefined : items,
              emptyList: hideList,
            }),
          );
          for (const raw of split.raw) {
            if (raw === detail.error?.raw) continue; // already in the page's raw-output block
            noticeArea.append(el("pre", { class: "raw-output", text: raw }));
          }
          if (showEmptyNote) {
            noticeArea.append(renderNotice("notice.gpu_in_use.empty_note", {}, lang));
          } else if (
            noticeKey === "notice.restore_stopped" &&
            emptyUsers &&
            !unreadable &&
            !variant.startsWith("unload_refused")
          ) {

            noticeArea.append(renderNotice("notice.gpu_in_use.empty_note", {}, lang));
          }
        }
        if (detail.version_change && detail.loaded_version_start && detail.loaded_version_end) {
          const oldV =
            "value" in detail.loaded_version_start
              ? String(detail.loaded_version_start.value)
              : "UNKNOWN";
          const newV =
            "value" in detail.loaded_version_end
              ? String(detail.loaded_version_end.value)
              : "UNKNOWN";
          noticeArea.append(
            renderNotice("notice.version_changed", { old: oldV, new: newV }, lang, { items }),
          );
        }
        for (const e of detail.gpu_users?.errors ?? []) {
          noticeArea.append(queryFailed(`query.object.${e.type}`, e.reason, e.raw ?? null, lang));
        }
      }

      // The terminal section depends on notice content too, because the notice determines whether a
      // separate code section is needed.
      const terminalSig = JSON.stringify([
        detail.error,
        detail.notice,
        detail.unit_query,
        detail.state_read_error,
        detail.state_note,
        detail.restore,
        detail.checks,
        detail.replace,
        detail.loaded_version_start,
        detail.loaded_version_end,
        detail.plan,
      ]);
      if (terminalSig !== terminalSignature) {
        terminalSignature = terminalSig;
        clear(terminalExtra);
        if (detail.error) {
          // Suppress the standalone code only for this exact known code.explanation notice. Compare
          // original params before rendering overwrites params.code.
          const notice = detail.notice;
          const noticeCode = (notice?.params ?? {})["code"];
          const codeShownByNotice =
            notice?.key === "code.explanation" &&
            typeof noticeCode === "string" &&
            noticeCode === detail.error.code &&
            Object.hasOwn(MESSAGES, `code.${noticeCode}`);
          if (!codeShownByNotice) {
            terminalExtra.append(el("h3", { text: expandText("task.error_code", {}, lang) }));

            terminalExtra.append(
              el("p", { class: "error-code" }, [el("code", { text: detail.error.code })]),
            );
          }
          if (detail.error.code === "DOWNLOAD_FAILED" || detail.error.code === "BUILD_FAILED") {
            const reason = normalizeReason(
              String((detail.error.params["reason"] as { key?: string } | undefined)?.key ?? ""),
              ((detail.error.params["reason"] as { params?: Params } | undefined)?.params ?? {}) as Params,
            );
            terminalExtra.append(renderNotice("task.error_reason", { reason }, lang));
          }
          if (detail.error.raw) {
            terminalExtra.append(el("h3", { text: expandText("task.raw_output", {}, lang) }));
            terminalExtra.append(el("pre", { class: "raw-output", text: detail.error.raw }));
          }
        }
        if (detail.unit_query) {
          terminalExtra.append(
            queryFailed(QUERY_OBJECT_HOST_TASK_UNIT, detail.unit_query.reason, null, lang),
          );
        }

        const details = el("div", {});
        // Reconciliation preserves the original read failure in state_note.state_read_error; keep showing it.
        const state_read_error = detail.state_read_error ?? detail.state_note?.state_read_error ?? null;
        if (state_read_error) {
          details.append(
            renderNotice("task.state_unreadable", {
              reason: { key: stateReadErrorReasonKey(state_read_error), params: {} },
            }, lang),
          );
        }
        if (detail.state_note && detail.state_note.kept_as) {
          details.append(
            renderNotice("task.state_file_kept", { file_name: detail.state_note.kept_as }, lang),
          );
        }
        if (detail.restore && detail.restore.decision === "none" && detail.restore.reason) {
          details.append(
            renderNotice(
              "restore.no_original",
              // The restore reason already holds a complete key; preserve its nested read_reason.
              { no_original_reason: noOriginalReasonParam(detail.restore) },
              lang,
            ),
          );
        }
        if (detail.checks.length > 0) {
          details.append(el("h3", { text: expandText("task.checks", {}, lang) }));
          // Failing check records show failing items; passing records show both actual readings.
          for (const c of detail.checks) {
            for (const p of c.passed ? checkItems(c) : checkFailureItems(c)) {
              details.append(
                el("p", { text: expandText("check.reason", p as unknown as Params, lang) }),
              );
            }
          }
        }
        if (detail.replace.length > 0) {
          const failed = detail.replace.flatMap((r) => (r.module_correction?.reloaded ?? r.reloaded).filter((x) => !x.ok));
          for (const f of failed) {
            details.append(
              renderNotice("log.module_reload_failed", { module: f.module, raw: f.reason ?? "" }, lang),
            );
          }
        }
        if (
          detail.error &&
          (detail.error.code === "RESTORE_NOT_WRITTEN" || detail.error.code === "RESTORE_CHECK_FAILED")
        ) {
          // Keep each restore occurrence separate and retain nested stderr until the split boundary outside the alert.
          const split = splitRawOutput(
            {
              check_reason: checkReasonText(detail, "update", lang),
              original: detail.restore?.original?.version ?? "UNKNOWN",
              restore_reason: restoreFailureReason(detail, lang),
            },
            detail.error.exit,
          );
          details.append(renderNotice("restore.two_failures", split.params, lang));
          for (const raw of split.raw) {
            if (raw === detail.error.raw) continue; // already in the page's raw-output block
            details.append(el("pre", { class: "raw-output", text: raw }));
          }
        }
        if (details.firstChild) {
          terminalExtra.append(el("section", { class: "task-details" }, [
            el("h3", { text: expandText("task.details", {}, lang) }), details,
          ]));
        }
      }

      if (TERMINAL.includes(detail.status) && poller) {
        poller.stop();
        // Refresh the write gate once when the task reaches a terminal state.
        if (!loginRefreshed) {
          loginRefreshed = true;
          // Failed terminal follow-ups stay next to the finished task. Responses from departed pages
          // cannot change global or session state.
          void app
            .refreshLogin(() => container.isConnected)
            .catch((err: unknown) => {
            if (!container.isConnected) return;
            if (err instanceof ApiError && err.isSessionEnded) {
              app.sessionEnded();
              return;
            }
            clear(readErrorArea);
            readErrorArea.append(apiErrorNotice(err, lang));
          });
        }
      }
    };
    poller = startPolling(POLL_MS, read);
    // Route the first read through the timer guard so a tick cannot start a duplicate request.
    await poller.tick();
  })();
}
