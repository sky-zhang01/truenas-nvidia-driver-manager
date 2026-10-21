import { ApiError, getGpuUsers, type GpuSnapshot, type ObjectEntry } from "./api.js";
import { clear, el, expandText, formatClock, renderNotice } from "./dom.js";
import type { Lang } from "./messages.js";
import { startPolling } from "./poll.js";
import { queryObjectKey } from "./rules.js";

interface ListOptions {
  headline?: string;
  unreadableHeadline?: string;
  snapshot?: boolean;
}
interface StoppedUser { user: ObjectEntry; timer: number; }
const identity = (user: ObjectEntry): string => user.id ?? `${user.type}:${user.name}`;

export function gpuUserText(user: ObjectEntry, lang: Lang): string {
  const object = expandText("list.object", {
    name: user.name, type: { key: queryObjectKey(user.type), params: {} },
  }, lang);
  return user.usage ? `${expandText(`gpu.badge.${user.usage}`, {}, lang)} · ${object}` : object;
}

function userRow(user: ObjectEntry, lang: Lang, stopped = false): HTMLElement {
  const usage = stopped ? "stopped" : user.usage;
  const row = el("li", { class: `gpu-user${stopped ? " gpu-user-stopped" : ""}` });
  const line = el("div", { class: "gpu-user-line" });
  if (usage) {
    const shape = el("span", { text: { in_use: "●", requested: "○", unknown: "?", stopped: "✓" }[usage] });
    shape.setAttribute("aria-hidden", "true");
    line.append(el("span", { class: "gpu-badge" }, [shape, " ", el("span", { text: expandText(`gpu.badge.${usage}`, {}, lang) })]));
  }
  line.append(" ", el("span", { class: "gpu-user-name", text: user.name }), " ");
  line.append(el("span", { class: "gpu-user-kind", text: expandText(user.usage ? `gpu.kind.${user.type}` : queryObjectKey(user.type), {}, lang) }));
  row.append(line);
  if (usage === "requested" || usage === "unknown") {
    row.append(el("p", { class: "gpu-user-hint", text: expandText(`gpu.badge.${usage}.hint`, {}, lang) }));
  }
  if (stopped) row.append(el("span", { class: "sr-only", text: expandText("gpu.announce.stopped", { name: user.name }, lang) }));
  return row;
}

/** The same list, hints and stopping instructions in live checks, tasks and snapshots. */
function populate(target: HTMLElement, snapshot: GpuSnapshot, lang: Lang, options: ListOptions, stopped: ObjectEntry[] = []): void {
  clear(target);
  const users = snapshot.users ?? [];
  const unreadable = !options.snapshot && (snapshot.state === "unreadable" || (users.length === 0 && (snapshot.errors?.length ?? 0) > 0));
  const headline = unreadable || (snapshot.state === "unreadable" && users.length === 0) ? options.unreadableHeadline ?? "gpu.unreadable"
    : !options.snapshot && snapshot.state === "idle" ? "gpu.free"
    : users.length === 0 ? (options.snapshot ? "notice.gpu_in_use.empty_note" : "gpu.empty")
    : options.headline ?? "gpu.wait.headline";
  if (headline) target.append(el("p", { class: `gpu-list-headline${users.length > 0 && !unreadable ? " notice-action" : options.snapshot && snapshot.state === "busy" ? " notice-note" : ""}`, text: expandText(headline, {}, lang) }));
  if (!unreadable && (users.length > 0 || stopped.length > 0)) {
    const list = el("ul", { class: "gpu-user-list notice-list" });
    for (const user of users) list.append(userRow(user, lang));
    for (const user of stopped) list.append(userRow(user, lang, true));
    target.append(list);
    if (users.some((user) => user.type === "docker")) target.append(el("p", { class: "gpu-howto", text: expandText("gpu.howto.docker", {}, lang) }));
    if (users.some((user) => user.type === "incus")) target.append(el("p", { class: "gpu-howto", text: expandText("gpu.howto.incus", {}, lang) }));
  }
  if (unreadable && snapshot.read_reason) target.append(renderNotice("query.failed", {
    query_object: { key: "query.object.gpu_usage" }, reason: snapshot.read_reason,
  }, lang, { role: "none" }));
  for (const error of snapshot.errors ?? []) target.append(renderNotice("query.failed", {
    query_object: { key: queryObjectKey(error.type) }, reason: error.reason,
  }, lang, { role: "none" }));
  if (options.snapshot && users.length > 0 && snapshot.checked_at !== undefined) target.append(el("p", {
    class: "gpu-snapshot-time", text: expandText("gpu.popup.snapshot", { time: formatClock(snapshot.checked_at) }, lang),
  }));
  if (options.snapshot && snapshot.state === "busy" && users.length > 0) target.append(el("p", {
    class: "notice-note", text: expandText("gpu.popup.fallback", {}, lang),
  }));
}

export function gpuUsersNode(snapshot: GpuSnapshot, lang: Lang, options: ListOptions = {}): HTMLElement {
  const node = el("div", { class: "gpu-users" });
  populate(node, snapshot, lang, options);
  return node;
}

/** Checked-at alone is not a fact change. Removed apps briefly retain a stopped row. */
export function watchGpuUsers(
  target: HTMLElement,
  lang: Lang,
  onSnapshot?: (snapshot: GpuSnapshot) => void,
  onSessionEnded?: () => void,
  options: ListOptions = {},
): () => void {
  let active = true;
  let signature = "";
  let previous: GpuSnapshot | null = null;
  const stopped = new Map<string, StoppedUser>();
  const render = (): void => {
    if (active && target.isConnected && previous) populate(target, previous, lang, options, [...stopped.values()].map((entry) => entry.user));
  };
  const poller = startPolling(2000, async () => {
    if (!target.isConnected) { stop(); return; }
    try {
      const snapshot = await getGpuUsers();
      if (!active || !target.isConnected) return;
      const next = JSON.stringify([snapshot.state, snapshot.users, snapshot.errors, snapshot.read_reason]);
      if (next === signature) return;
      signature = next;
      const current = new Set((snapshot.users ?? []).map(identity));
      for (const [id, entry] of stopped) if (current.has(id)) { window.clearTimeout(entry.timer); stopped.delete(id); }
      const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;
      const failedKinds = new Set((snapshot.errors ?? []).map((error) => error.type));
      if (!reduced && snapshot.state !== "unreadable") for (const user of previous?.users ?? []) {
        const id = identity(user);
        if (!current.has(id) && !stopped.has(id) && !failedKinds.has(user.type)) {
          const timer = window.setTimeout(() => { stopped.delete(id); render(); }, 4000);
          stopped.set(id, { user, timer });
        }
      }
      previous = snapshot;
      onSnapshot?.(snapshot);
      render();
    } catch (error) {
      if (!active || !target.isConnected) return;
      if (error instanceof ApiError && error.isSessionEnded) { stop(); onSessionEnded?.(); return; }
      const next = `error:${error instanceof ApiError ? JSON.stringify([error.key, error.params]) : "transport"}`;
      if (next === signature) return;
      signature = next;
      previous = null;
      clear(target);
      target.append(el("p", { class: "gpu-list-headline", text: expandText("gpu.unreadable", {}, lang) }));
    }
  });
  const stop = (): void => {
    active = false; poller.stop();
    for (const entry of stopped.values()) window.clearTimeout(entry.timer);
    stopped.clear();
    if (target.firstChild) clear(target);
  };
  void poller.tick();
  return stop;
}
