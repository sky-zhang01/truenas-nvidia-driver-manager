import { ApiError, type HomeGpuDevice, type HomeResponse, type HomeTaskRecord, type Reading } from "../api.js";
import {
  el,
  expand,
  expandText,
  appendInline,
  formatDate,
  formatTaskTime,
  queryFailed,
  renderNotice,
  apiErrorNotice,
  type Params,
} from "../dom.js";
import type { App } from "../main.js";
import { stateReadErrorReasonKey, QUERY_OBJECT_RUNNING_TASK } from "../rules.js";
import { interruptCopyKey } from "../task-steps.js";

function readingText<T>(r: Reading<T> | null | undefined): string {
  if (!r) return "UNKNOWN";
  if ("value" in r) return String(r.value);
  return "UNKNOWN";
}

function row(label: string, value: string): HTMLElement {
  const dl = el("div", { class: "reading" });
  dl.append(el("span", { class: "reading-label", text: label }));
  dl.append(el("span", { class: "reading-value", text: value }));
  return dl;
}

function isTaskRecord(value: HomeResponse["latest_task"]): value is HomeTaskRecord {
  return value !== null && "task_id" in value;
}

function sourceParam(task: HomeTaskRecord): Params[string] {
  const source = task.source;
  if (!source) return "UNKNOWN";
  if (source.kind === "upload") {
    return { key: "tasks.source.upload", params: { fingerprint: source.fingerprint ?? "" } };
  }
  if (source.kind === "local") {
    return {
      key: "tasks.source.local",
      params: {
        source: {
          key: `drivers.source.${source.row_kind ?? "official"}`,
          params: { fingerprint: source.fingerprint },
        },
      },
    };
  }
  return { key: "tasks.source.official", params: {} };
}

function taskTimeParams(task: HomeTaskRecord): Params {
  return {
    time: formatTaskTime(task.created_at),
    discovered: formatTaskTime(task.interrupt_discovered_at),
    phase: task.phase ? { key: `phase.${task.phase}`, params: {} } : "UNKNOWN",
    version: task.version ?? "UNKNOWN",
    task_id: task.task_id,
    status: { key: `status.${task.status}`, params: {} },
    source: sourceParam(task),
  };
}

function gpuLine(device: HomeGpuDevice, lang: App["lang"]): string {
  const name = device.name
    ? device.name
    : expandText("home.gpus.unlisted", { device: device.device }, lang);
  const meta = expandText(
    "home.gpus.meta",
    { address: device.address, device: device.device },
    lang,
  );
  const pass = device.passthrough ? ` · ${expandText("home.gpus.passthrough", {}, lang)}` : "";
  return `${name} · ${meta}${pass}`;
}

export function renderHome(app: App, container: HTMLElement): void {
  const lang = app.lang;
  const epoch = app.homeEpoch;
  void (async () => {
    let home: HomeResponse;
    try {
      home = await app.loadHome();
    } catch (err) {
      // Late responses cannot update a replacement page or end a session started since the request.
      if (!container.isConnected || epoch !== app.homeEpoch) return;
      if (err instanceof ApiError && err.isSessionEnded) {
        app.sessionEnded();
        return;
      }
      // A failed home read confirms nothing about the active task.
      app.reportTaskState(null, "global");

      container.append(apiErrorNotice(err, lang));
      return;
    }

    // Only confirmed RUNNING state claims the navigation dot. An unreadable response or a response for
    // a departed page cannot revive it.
    if (!container.isConnected || epoch !== app.homeEpoch || !app.signedIn) return;
    if (home.interrupted) {
      app.reportTaskState("interrupted", "global");
    } else if (home.active_task !== null && "task_id" in home.active_task && home.active_task.status === "RUNNING") {
      app.reportTaskState("running", "global", home.active_task.task_id);
    } else {
      app.reportTaskState(null, "global");
    }

    if (app.passwordChanged) {
      container.append(renderNotice("password.changed", {}, lang));
      app.clearPasswordChanged();
    }

    if (home.interrupted) {
      const task = home.interrupted;
      container.append(renderNotice(interruptCopyKey(task.phase), taskTimeParams(task), lang));
      container.append(
        el("a", {
          href: "#/update",
          class: "interrupt-next",
          text: expandText("home.interrupted.next", {}, lang),
        }),
      );
      const details = el("details", { class: "interrupt-detail" });
      details.append(el("summary", { text: expandText("home.interrupted.details", {}, lang) }));
      details.append(
        el("p", { text: expandText("home.interrupted.task", { task_id: task.task_id }, lang) }),
      );
      details.append(el("p", { text: expandText("home.interrupted.basis", {}, lang) }));
      const drivers = el("p");
      appendInline(
        drivers,
        expand("home.interrupted.artifact", {}, lang).segments.flatMap((segment) => segment.nodes),
      );
      details.append(drivers);
      container.append(details);
    } else if (home.latest_task === null) {
      container.append(el("p", { class: "hint", text: expandText("home.latest.none", {}, lang) }));
    } else if (isTaskRecord(home.latest_task)) {
      container.append(
        el("p", {
          class: "hint",
          text: expandText("home.latest.record", taskTimeParams(home.latest_task), lang),
        }),
      );
    } else {
      container.append(renderNotice("task.latest_unknown", {}, lang));
    }

    const list = el("div", { class: "readings" });

    const loaded =
      home.loaded_version && "not_loaded" in home.loaded_version
        ? expandText("home.loaded_version.none", {}, lang)
        : readingText(home.loaded_version);
    list.append(row(expandText("home.loaded_version", {}, lang), loaded));

    list.append(
      row(
        expandText("home.canonical_version", {}, lang),
        readingText(home.canonical?.version),
      ),
    );
    list.append(
      row(
        expandText("home.canonical_checksum", {}, lang),
        readingText(home.canonical?.sha256),
      ),
    );
    list.append(
      row(expandText("home.truenas_version", {}, lang), readingText(home.truenas_version)),
    );
    list.append(row(expandText("home.kernel", {}, lang), readingText(home.kernel)));

    const gpuValue = el("div", { class: "gpu-readout" });
    if ("unknown" in home.gpus && home.gpus.unknown) {
      gpuValue.append(el("p", { class: "reading-value", text: "UNKNOWN" }));
    } else {
      const devices = home.gpus.value?.devices ?? [];
      if (devices.length === 0) {
        gpuValue.append(el("p", { class: "reading-value", text: expandText("home.gpus.none", {}, lang) }));
      } else {
        for (const device of devices) {
          gpuValue.append(el("p", { class: "reading-value", text: gpuLine(device, lang) }));
        }
      }
      gpuValue.append(el("p", { class: "hint", text: expandText("home.gpus.basis", {}, lang) }));
    }
    const gpuRow = el("div", { class: "reading" });
    gpuRow.append(el("span", { class: "reading-label", text: expandText("home.gpus", {}, lang) }));
    gpuRow.append(gpuValue);
    list.append(gpuRow);

    let linkText = "UNKNOWN";
    if (home.link && home.link.exists) {
      linkText = home.link.points_to_canonical
        ? expandText("home.link.ok", {}, lang)
        : expandText("home.link.elsewhere", { link_target: home.link.target ?? "" }, lang);
    } else if (home.link && !home.link.unknown) {
      linkText = expandText("home.link.missing", {}, lang);
    }
    list.append(row(expandText("home.link", {}, lang), linkText));

    let originalText = "UNKNOWN";
    if (home.original_saved && home.original_saved.saved) {
      originalText = expandText(
        "home.original.saved",
        {
          original: home.original_saved.version ?? "UNKNOWN",
          date:
            home.original_saved.saved_at !== undefined
              ? formatDate(home.original_saved.saved_at)
              : "UNKNOWN",
        },
        lang,
      );
    } else if (home.original_saved && home.original_saved.saved === false) {
      originalText = expandText("home.original.none", {}, lang);
    }
    list.append(row(expandText("home.original", {}, lang), originalText));

    container.append(list);

    if (home.link && !home.link.exists && !home.link.unknown) {
      container.append(renderNotice("extension.missing", {}, lang));
    }

    const status = el("div", { class: "task-state", role: "status" });
    if (home.active_task && "task_id" in home.active_task) {
      const key = home.active_task.status === "UNKNOWN" ? "home.active_task.unknown" : "home.active_task";
      status.append(renderNotice(key, { task_id: home.active_task.task_id }, lang));
      if (home.active_task.status === "UNKNOWN") {
        const code = home.active_task.state_read_error ?? "missing";
        status.append(
          renderNotice("task.state_unreadable", {
            reason: { key: stateReadErrorReasonKey(code), params: {} },
          }, lang),
        );
      }
      const link = el("a", {
        href: `#/tasks/${home.active_task.task_id}`,
        text: expandText("home.open_task", {}, lang),
      });
      status.append(link);
    } else if (home.active_task && "unknown" in home.active_task) {
      const unknown = home.active_task.unknown;
      status.append(queryFailed(QUERY_OBJECT_RUNNING_TASK, unknown, null, lang));
    }
    container.append(status);
  })();
}
