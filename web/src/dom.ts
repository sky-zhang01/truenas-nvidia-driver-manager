// Message encoding is documented beside the catalog in messages.ts. Unknown values render as UNKNOWN.

import { ApiError } from "./api.js";
import { message, type Lang } from "./messages.js";

export type Level = "info" | "warning" | "error";
export type SegmentKind = "title" | "body" | "action" | "list" | "note";
export type Params = Record<string, unknown>;

export type InlineNode =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; text: string }
  | { kind: "link"; text: string; href: string };

export interface Segment {
  kind: SegmentKind;
  nodes: InlineNode[];
}

export interface Expanded {
  level: Level;
  segments: Segment[];
}

const LEVEL_TAGS: readonly string[] = ["info", "warning", "error"];
const KIND_TAGS: readonly string[] = ["title", "body", "action", "list", "note"];

const UNKNOWN = "UNKNOWN";

export function isReason(v: unknown): v is { key: string; params?: Params } {
  return (
    typeof v === "object" &&
    v !== null &&
    typeof (v as { key?: unknown }).key === "string"
  );
}

export function resolveKey(key: string, variant?: string): string {
  if (key === "code.explanation") {
    return "code.explanation";
  }
  if (variant && message(`${key}.${variant}`)) {
    return `${key}.${variant}`;
  }
  return key;
}

function codeExplanationKey(params: Params): string {
  const code = typeof params["code"] === "string" ? (params["code"] as string) : "";
  return `code.${code}`;
}

// A runtime flag selects a wording variant only when the catalog defines that key.
function runtimeVariant(key: string, params: Params): string {
  const waited = params["waited_in_use"] === true;
  if (waited && params["reload_failed"] === true && message(`${key}.waited.reload_failed`)) {
    return `${key}.waited.reload_failed`;
  }
  if (waited && params["not_unloaded"] === true && message(`${key}.waited.not_unloaded`)) {
    return `${key}.waited.not_unloaded`;
  }
  if (waited && message(`${key}.waited`)) {
    return `${key}.waited`;
  }
  if (params["reload_failed"] === true && message(`${key}.reload_failed`)) {
    return `${key}.reload_failed`;
  }
  if (params["not_unloaded"] === true && message(`${key}.not_unloaded`)) {
    return `${key}.not_unloaded`;
  }
  if (params["empty_list"] === true && message(`${key}.empty_list`)) {
    return `${key}.empty_list`;
  }
  return key;
}

export function paramText(value: unknown, lang: Lang): string {
  if (value === null || value === undefined) return UNKNOWN;
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (Array.isArray(value)) {
    const parts = value.map((v) => paramText(v, lang));
    return parts.join(lang === "zh" ? "、" : ", ");
  }
  if (isReason(value)) {
    return expandText(value.key, (value.params ?? {}) as Params, lang);
  }
  // Command-failure reasons may contain verbatim stderr.
  if (typeof (value as { stderr?: unknown }).stderr === "string") {
    return (value as { stderr: string }).stderr;
  }
  const reading = value as { value?: unknown; not_loaded?: unknown; unknown?: unknown };
  if ("value" in reading) return paramText(reading.value, lang);
  if (reading.not_loaded === true) return UNKNOWN;
  if ("unknown" in reading) return UNKNOWN;
  return UNKNOWN;
}

export function expandText(
  key: string,
  params: Params,
  lang: Lang,
  variant?: string,
): string {
  return expand(key, params, lang, variant)
    .segments.map((s) => s.nodes.map((n) => n.text).join(""))
    .join(" ");
}

function nodesForReason(reason: { key: string; params?: Params }, lang: Lang): InlineNode[] {
  const exp = expand(reason.key, (reason.params ?? {}) as Params, lang);
  const out: InlineNode[] = [];
  for (const s of exp.segments) {
    if (out.length > 0) out.push({ kind: "text", text: " " });
    out.push(...s.nodes);
  }
  return out;
}

// Placeholders inside code and strong spans are runtime values too.
function interpolate(text: string, params: Params, lang: Lang): string {
  let out = "";
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (ch === "{") {
      const end = text.indexOf("}", i + 1);
      if (end > i) {
        const name = text.slice(i + 1, end);
        if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
          out += paramText(params[name], lang);
          i = end + 1;
          continue;
        }
      }
    }
    out += ch;
    i += 1;
  }
  return out;
}

function inlineNodes(text: string, params: Params, lang: Lang): InlineNode[] {
  const out: InlineNode[] = [];
  let buf = "";
  const flush = (): void => {
    if (buf !== "") {
      out.push({ kind: "text", text: buf });
      buf = "";
    }
  };
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (ch === "`") {
      const end = text.indexOf("`", i + 1);
      if (end > i) {
        flush();
        out.push({ kind: "code", text: interpolate(text.slice(i + 1, end), params, lang) });
        i = end + 1;
        continue;
      }
    }
    if (ch === "*" && text.startsWith("**", i)) {
      const end = text.indexOf("**", i + 2);
      if (end > i) {
        flush();
        // Nested code markup inside a strong span must not expose its backticks.
        out.push({
          kind: "strong",
          text: interpolate(text.slice(i + 2, end).replaceAll("`", ""), params, lang),
        });
        i = end + 2;
        continue;
      }
    }
    if (ch === "[" && text.startsWith("[[", i)) {
      const end = text.indexOf("]]", i + 2);
      if (end > i) {
        const body = text.slice(i + 2, end);
        const bar = body.indexOf("|");
        if (bar >= 0) {
          flush();
          out.push({
            kind: "link",
            text: interpolate(body.slice(0, bar), params, lang),
            href: body.slice(bar + 1),
          });
          i = end + 2;
          continue;
        }
      }
    }
    if (ch === "{") {
      const end = text.indexOf("}", i + 1);
      if (end > i) {
        const name = text.slice(i + 1, end);
        if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
          flush();
          const value = params[name];
          if (isReason(value)) {
            for (const n of nodesForReason(value, lang)) out.push(n);
          } else {
            const rendered = paramText(value, lang);
            if (rendered !== "") out.push({ kind: "text", text: rendered });
          }
          i = end + 1;
          continue;
        }
      }
    }
    buf += ch;
    i += 1;
  }
  flush();
  return out;
}

function parse(text: string, params: Params, lang: Lang): Expanded {
  let level: Level | null = null;
  const order: SegmentKind[] = [];
  const byKind = new Map<SegmentKind, InlineNode[]>();
  for (const line of text.split("\n")) {
    let rest = line;
    let kind: SegmentKind | null = null;
    for (;;) {
      const m = /^([A-Za-z]+)\|/.exec(rest);
      if (!m) break;
      const tag = m[1] as string;
      if (LEVEL_TAGS.includes(tag)) {
        level = tag as Level;
      } else if (KIND_TAGS.includes(tag)) {
        kind = tag as SegmentKind;
      } else {
        break;
      }
      rest = rest.slice(m[0].length);
    }
    const k: SegmentKind = kind ?? "body";
    if (!byKind.has(k)) {
      byKind.set(k, []);
      order.push(k);
    }
    const nodes = byKind.get(k) as InlineNode[];
    // Same-kind lines share one segment; list templates are rendered once per list element.
    if (nodes.length > 0) nodes.push({ kind: "text", text: " " });
    for (const n of inlineNodes(rest, params, lang)) nodes.push(n);
  }
  const segments: Segment[] = order.map((k) => ({
    kind: k,
    nodes: byKind.get(k) ?? [],
  }));
  return { level: level ?? "info", segments };
}

export function expand(
  key: string,
  params: Params,
  lang: Lang,
  variant?: string,
): Expanded {
  const base = key === "code.explanation" ? codeExplanationKey(params) : resolveKey(key, variant);
  const resolved = runtimeVariant(base, params);
  const msg = message(resolved) ?? message(base) ?? message(key);
  if (!msg) {
    // Unknown catalog keys remain visible with their parameters so diagnostics are not lost.
    const parts = [key];
    if (Object.keys(params).length > 0) {
      parts.push(JSON.stringify(params));
    }
    return {
      level: "info",
      segments: [{ kind: "body", nodes: [{ kind: "text", text: parts.join(" ") }] }],
    };
  }
  return parse(msg[lang], params, lang);
}

export function messageLine(
  key: string,
  kind: SegmentKind,
  variant?: string,
): string | null {
  const msg = message(resolveKey(key, variant));
  if (!msg) return null;
  for (const line of msg.en.split("\n")) {
    const m = new RegExp(`^([A-Za-z]+\\|)*(${kind})\\|(.*)$`).exec(line);
    if (m) return m[3] as string;
  }
  return null;
}

export type Child = Node | string;
export interface ElProps {
  class?: string;
  id?: string;
  type?: string;
  text?: string;
  href?: string;
  name?: string;
  value?: string;
  for?: string;
  ariaLabel?: string;
  ariaDescribedBy?: string;
  ariaSort?: string;
  role?: string;
  disabled?: boolean;
  checked?: boolean;
  title?: string;
  accept?: string;
  autocomplete?: string;
  multiple?: boolean;
  hidden?: boolean;
  min?: string;
  max?: string;
  step?: string;
  rowSpan?: number;
  colSpan?: number;
  scope?: string;
  onclick?: (ev: MouseEvent) => void;
  onchange?: (ev: Event) => void;
  oninput?: (ev: Event) => void;
  onsubmit?: (ev: Event) => void;
  onkeydown?: (ev: KeyboardEvent) => void;
}

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props?: ElProps,
  children?: Child[],
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (props) {
    if (props.class !== undefined) node.className = props.class;
    if (props.id !== undefined) node.id = props.id;
    if (props.role !== undefined) node.setAttribute("role", props.role);
    if (props.type !== undefined) node.setAttribute("type", props.type);
    if (props.href !== undefined) node.setAttribute("href", props.href);
    if (props.name !== undefined) node.setAttribute("name", props.name);
    if (props.value !== undefined) (node as unknown as { value: string }).value = props.value;
    if (props.for !== undefined) node.setAttribute("for", props.for);
    if (props.ariaLabel !== undefined) node.setAttribute("aria-label", props.ariaLabel);
    if (props.ariaDescribedBy !== undefined) {
      node.setAttribute("aria-describedby", props.ariaDescribedBy);
    }
    if (props.ariaSort !== undefined) node.setAttribute("aria-sort", props.ariaSort);
    if (props.title !== undefined) node.setAttribute("title", props.title);
    if (props.accept !== undefined) node.setAttribute("accept", props.accept);
    if (props.autocomplete !== undefined) {
      node.setAttribute("autocomplete", props.autocomplete);
    }
    if (props.min !== undefined) node.setAttribute("min", props.min);
    if (props.max !== undefined) node.setAttribute("max", props.max);
    if (props.step !== undefined) node.setAttribute("step", props.step);
    if (props.rowSpan !== undefined) node.setAttribute("rowspan", String(props.rowSpan));
    if (props.colSpan !== undefined) node.setAttribute("colspan", String(props.colSpan));
    if (props.scope !== undefined) node.setAttribute("scope", props.scope);
    if (props.disabled !== undefined) (node as unknown as { disabled: boolean }).disabled = props.disabled;
    if (props.checked !== undefined) (node as unknown as { checked: boolean }).checked = props.checked;
    if (props.multiple !== undefined) (node as unknown as { multiple: boolean }).multiple = props.multiple;
    if (props.hidden === true) node.setAttribute("hidden", "");
    if (props.text !== undefined) node.textContent = props.text;
    if (props.onclick) node.addEventListener("click", props.onclick as EventListener);
    if (props.onchange) node.addEventListener("change", props.onchange as EventListener);
    if (props.oninput) node.addEventListener("input", props.oninput as EventListener);
    if (props.onsubmit) node.addEventListener("submit", props.onsubmit as EventListener);
    if (props.onkeydown) node.addEventListener("keydown", props.onkeydown as EventListener);
  }
  if (children) {
    for (const c of children) node.append(typeof c === "string" ? text(c) : c);
  }
  return node;
}

export function text(s: string): Text {
  return document.createTextNode(s);
}

export function clear(node: Element): void {
  while (node.firstChild) node.removeChild(node.firstChild);
}

// Runtime parameters enter the DOM as text nodes, never markup.
export function appendInline(target: Element, nodes: InlineNode[]): void {
  for (const n of nodes) {
    switch (n.kind) {
      case "text":
        target.append(text(n.text));
        break;
      case "code":
        target.append(el("code", { text: n.text }));
        break;
      case "strong":
        target.append(el("strong", { text: n.text }));
        break;
      case "link":
        target.append(el("a", { href: n.href, text: n.text }));
        break;
    }
  }
}

export interface ReasonLike {
  key: string;
  params?: Record<string, unknown>;
}

export function reasonText(
  field: ReasonLike | null | undefined,
  raw: string | null | undefined,
  lang: Lang,
): string {
  if (!field) return UNKNOWN;
  const base = expandText(field.key, (field.params ?? {}) as Params, lang);
  return raw ? `${base}:${raw}` : base;
}

export function reasonParam(
  field: ReasonLike | null | undefined,
  raw: string | null | undefined,
  lang: Lang,
): unknown {
  if (!field) return UNKNOWN;
  return raw ? reasonText(field, raw, lang) : field;
}

export function queryFailed(
  objectKey: string,
  reason: ReasonLike | null | undefined,
  raw: string | null | undefined,
  lang: Lang,
): DocumentFragment {
  // External command output stays outside alert/status regions to avoid announcing arbitrary diagnostics.
  const frag = document.createDocumentFragment();
  frag.append(
    renderNotice(
      "query.failed",
      {
        query_object: { key: objectKey, params: {} },
        reason: reasonParam(reason, null, lang),
      },
      lang,
    ),
  );
  if (raw) frag.append(el("pre", { class: "raw-output", text: raw }));
  return frag;
}

// API rejections keep their real error and params; callers own the sign-in transition for auth.required.
export function apiErrorNotice(err: unknown, lang: Lang): DocumentFragment {
  const frag = document.createDocumentFragment();
  if (err instanceof ApiError) {
    frag.append(renderNotice(err.key, { ...err.params }, lang));
    if (err.raw) frag.append(el("pre", { class: "raw-output", text: err.raw }));
  } else {
    frag.append(
      renderNotice("internal", { reason: { key: "internal.request_failed", params: {} } }, lang),
    );
  }
  return frag;
}

export function renderNotice(
  key: string,
  params: Params,
  lang: Lang,
  opts?: {
    variant?: string;
    items?: string[];
    // Use "none" when the enclosing container already owns the alert region.
    role?: "alert" | "status" | "none";
    class?: string;
    listKey?: string;
    // An empty application list suppresses Action, List and Note; callers supply the empty-list note.
    emptyList?: boolean;
    // The same catalog key may require an error level inside a rejected plan.
    level?: "info" | "warning" | "error";
  },
): HTMLElement {
  const exp = expand(key, params, lang, opts?.variant);
  const level = opts?.level ?? exp.level;
  const role = opts?.role ?? (level === "error" ? "alert" : undefined);
  const roleAttr = role === "none" ? undefined : role;
  const box = el("div", {
    class: `notice level-${level}${opts?.class ? ` ${opts.class}` : ""}`,
    role: roleAttr,
  });
  box.append(
    el("span", { class: "sr-only", text: expandText(`level.${level}`, {}, lang) }),
  );
  // CSS supplies the decorative level icon; screen readers announce the separate level word.
  const icon = el("span", { class: "notice-icon" });
  icon.setAttribute("aria-hidden", "true");
  box.append(icon);
  for (const seg of exp.segments) {
    if (opts?.emptyList && (seg.kind === "action" || seg.kind === "list" || seg.kind === "note")) {
      continue;
    }
    if (seg.kind === "title") {
      const h = el("h3", { class: "notice-title" });
      appendInline(h, seg.nodes);
      box.append(h);
    } else if (seg.kind === "list") {
      const ul = el("ul", { class: "notice-list" });
      const items = opts?.items ?? [seg.nodes.map((n) => n.text).join("")];
      for (const item of items) {
        ul.append(el("li", { text: item }));
      }
      box.append(ul);
    } else {
      const p = el("p", { class: `notice-${seg.kind}` });
      appendInline(p, seg.nodes);
      box.append(p);
    }
  }
  return box;
}

function pad(n: number, width: number): string {
  return String(n).padStart(width, "0");
}

// Task times use formatTaskTime; SINCE values append seconds to this zone-free value.
export function formatDate(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1, 2)}-${pad(d.getDate(), 2)} ${pad(
    d.getHours(),
    2,
  )}:${pad(d.getMinutes(), 2)}`;
}

function zoneLabel(d: Date): string {
  const offset = -d.getTimezoneOffset();
  const sign = offset >= 0 ? "+" : "-";
  const abs = Math.abs(offset);
  return `UTC${sign}${pad(Math.floor(abs / 60), 2)}:${pad(abs % 60, 2)}`;
}

// Task timestamps include seconds and a zone; invalid or absent values are UNKNOWN.
export function formatTaskTime(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return "UNKNOWN";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "UNKNOWN";
  return `${formatDate(ms)}:${pad(d.getSeconds(), 2)} ${zoneLabel(d)}`;
}

export function formatDateMicros(us: number): string {
  const d = new Date(Math.floor(us / 1000));
  const micros = ((us % 1000000) + 1000000) % 1000000;
  return `${formatDate(d.getTime())}:${pad(d.getSeconds(), 2)}.${pad(micros, 6)}`;
}

// MB uses decimal bytes: 1,000,000 bytes, with one decimal place.
export function formatMB(bytes: number): string {
  return (bytes / 1000000).toFixed(1);
}

export function formatClock(ms: number): string {
  const d = new Date(ms);
  return `${pad(d.getHours(), 2)}:${pad(d.getMinutes(), 2)}:${pad(d.getSeconds(), 2)}`;
}
