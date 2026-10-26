import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";

import { expandText } from "../src/dom.js";
import { ApiError } from "../src/api.js";
import { renderPlanError } from "../src/views/plan.js";
import { FakeElement, installDom } from "./dom-shim.js";
import { MESSAGES, messageKeys } from "../src/messages.js";

const here = dirname(fileURLToPath(import.meta.url));

// The checked-in `src/views`. `node --test` runs the compiled `web/dist-test/test`, whose sibling
// `src/views` holds only `.js`, so walk up to the first `src/views` that still has sources: a scan of
// a directory with no `.ts` reads nothing and would pass without looking at a single view.
function sourceViewsDir(): string {
  let dir = here;
  for (let up = 0; up < 4; up += 1) {
    const candidate = join(dir, "..", "src", "views");
    try {
      if (readdirSync(candidate).some((name) => name.endsWith(".ts"))) return candidate;
    } catch {

    }
    dir = join(dir, "..");
  }
  throw new Error(`no src/views with .ts sources above ${here}`);
}

const viewsDir = sourceViewsDir();

// The compile root `web/src`: `tsconfig.json` includes every `.ts` file under `src` recursively, so
// the guard follows the same walk instead of listing the top-level view files.
const srcDir = join(viewsDir, "..");

// Every `.ts` source under `root`, recursively (the shape of `tsconfig.json`'s `include`, so a file
// the compiler builds cannot hide in a subdirectory the guard never walks).
function tsSourcesUnder(root: string): string[] {
  const out: string[] = [];
  const walk = (dir: string): void => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.name.endsWith(".ts")) out.push(path);
    }
  };
  walk(root);
  return out;
}

// A TypeScript fixture kept under `web/test/fixtures/` as `.txt` so the test compiler does not compile
// the probe module.
function fixture(name: string): string {
  let dir = here;
  for (let up = 0; up < 4; up += 1) {
    const candidate = join(dir, "..", "test", "fixtures", "visible-text-probes", `${name}.ts.txt`);
    try {
      return readFileSync(candidate, "utf8");
    } catch {

    }
    dir = join(dir, "..");
  }
  throw new Error(`no fixture ${name} above ${here}`);
}

function placeholders(text: string): Set<string> {
  const out = new Set<string>();
  const re = /\{([A-Za-z_][A-Za-z0-9_]*)\}/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) out.add(m[1] as string);
  return out;
}

test("every key carries both languages with matching placeholders", () => {
  for (const key of messageKeys()) {
    const msg = MESSAGES[key];
    assert.ok(msg, key);
    assert.equal(typeof msg.zh, "string", `${key}.zh`);
    assert.equal(typeof msg.en, "string", `${key}.en`);
    assert.ok((msg.zh ?? "").length > 0, `${key}.zh is empty`);
    assert.ok((msg.en ?? "").length > 0, `${key}.en is empty`);
    const zh = placeholders(msg.zh);
    const en = placeholders(msg.en);
    assert.deepEqual(
      [...zh].sort(),
      [...en].sort(),
      `placeholder mismatch in ${key}: zh=${[...zh].join(",")} en=${[...en].join(",")}`,
    );
  }
});

const REQUIRED: string[] = [

  "level.info",
  "level.warning",
  "level.error",

  "code.explanation",
  "code.SAVE_ORIGINAL_FAILED",
  "code.WRITE_FAILED",
  "code.REPLACE_DEADLINE_EXCEEDED",
  "task.error_reason",

  "reason.file_missing",
  "reason.timed_out",
  "reason.http_status",
  "save.facts_unreadable",
  "write.rename_failed",
  "check.reason",
  "build.kernel_module_type_unsupported",
  "build.community_patch_failed.archive",
  "build.community_patch_failed",
  "build.kernel_module_build_failed",
  "build.kernel_module_missing",
  "build.kernel_module_install_failed",
  "build.artifact.deliver_failed",
  "download.checksum_mismatch",
  "branch.nvidia_drm_not_loaded",
  "branch.extra_cflags_removed",
  "branch.fb_create_format_info",
  "restore.no_original.not_saved",
  "restore.no_original.checksum_mismatch",
  "restore.no_original.unreadable",

  "status.SUCCEEDED",
  "status.STOCK_RESTORED",
  "status.FAILED",
  "status.INTERRUPTED",
  "status.RUNNING",
  "status.UNKNOWN",
  "phase.read_loaded_version",
  "phase.read_loaded_version_end",
  "phase.restore_check",
  "home.interrupted",
  "home.interrupted.phase_unknown",
  "home.interrupted.next",

  "log.phase_enter",
  "log.cmd",
  "log.finished",
  "log.build_step.install_kernel_module",
  "log.build_step.community_patch",
  "log.install_mode.userspace-make",

  "login.failed",
  "login.rate_limited",
  "logout.done",

  "setup.title",
  "setup.intro",
  "setup.step.app",
  "setup.step.logs",
  "setup.step.copy",
  "setup.code",
  "setup.administrator",
  "setup.repeat",
  "setup.password_hint",
  "setup.submit",
  "setup.help.summary",
  "setup.help.body",
  "setup.note",
  "setup.code_invalid",
  "setup.code_renewed",
  "setup.code_help",
  "setup.not_available",
  "login.intro",
  "login.help.summary",
  "login.help.hint",
  "login.help.reset_location_unknown",
  "login.help.no_account",
  "login.help.after_reinstall",
  "login.help.forgot_password",
  "password.title_voluntary",
  "password.section",
  "password.rule.too_short",
  "password.changed",
  "setup.blocked",
  "plan.source_invalid.version_format",
  "upload.group.toolkit_too_old",
  "plan.changed",
  "plan.changed.after_open",
  "plan.changed.item.gpus",
  "plan.uncomputable",
  "precheck.failed",
  "submit.failed",
  "submit.unit_not_created",
  "submit.state_unwritable",
  "submit.task_dir_failed",
  "submit.exec_failed",
  "submit.runner_copy_failed",
  "upload.busy",
  "upload.group.toolkit_too_old",
  "upload.rejected",
  "upload.interrupted",
  "delete.done",
  "delete.failed",
  "keep.unsaved",
  "keep.row_unreadable",
  "auth.required",
  "internal",

  "app.title",
  "app.purpose",
  "nav.logout",
  "page.task",
  "lang.control",
  "home.loaded_version",
  "update.catalog_label",
  "update.gpu_select",
  "update.gpu_option",
  "catalog.help",
  "catalog.gpu.not_listed",
  "catalog.no_usable_version",
  "confirm.original.unsavable",
  "confirm.community_patch",
  "confirm.how.official",
  "confirm.how.upload",
  "confirm.how.local",
  "drivers.col.version",
  "drivers.empty",
  "tasks.col.id",
  "task.state_file_kept",

  "setting.invalid_utf8",
  "setting.rule.username",
  "setting.rule.port",
  "setting.rule.build_timeout",
  "startup.mapping.no_hostname_line",
  "startup.mapping.nsenter_failed",
  "startup.mapping.inspect_failed",
  "startup.mapping.mount_count",
  "startup.mapping.mount_under",
  "startup.mapping.path_characters",
  "startup.mapping.check_write_failed",
  "startup.mapping.check_read_failed",
  "startup.mapping.check_mismatch",
  "startup.pid_host",
  "startup.runtime",
  "startup.data_mapping",
  "internal.request_failed",

  "notice.version_changed",
  "notice.gpu_in_use",
  "notice.unload_refused",
  "notice.update_complete",
  "notice.restored",
  "notice.restore_stopped",
  "notice.restore_failed",
  "notice.no_restore",
  "download.progress",
  "extension.missing",
  "driver.kernel_mismatch",
  "original.already_saved",
  "original.skip_warning",
  "setting.unknown",
  "credentials.unreadable",
  "query.failed",
  "query.object.host_task_unit",
  "query.object.gpus",
  "help.reset_account",
  "task.state_unreadable",
  "restore.no_original",
  "restore.two_failures",
  "startup.failed",
  "setting.invalid",
  "credentials.unsaved",
  "toolkit.below_floor",
  "delete.confirm",

  "notice.gpu_in_use.task",
  "notice.gpu_in_use.task_unreadable",
  "notice.unload_refused.reload_failed",
  "notice.restore_stopped.busy",
  "notice.restore_stopped.unreadable",
  "notice.restore_stopped.unload_refused",
  "notice.restore_failed.before_write",
  "notice.restore_failed.written",
];

test("the keys the interface calls for are all present", () => {
  const missing = REQUIRED.filter((k) => MESSAGES[k] === undefined);
  assert.deepEqual(missing, []);
});

// The checkout root: the compiled test runs from `web/dist-test/test`, so walk up to the tree that
// holds both the Rust declaration of the reason keys and the web catalog.
function repoRoot(): string {
  let dir = here;
  for (let up = 0; up < 5; up += 1) {
    const candidate = join(dir, "..");
    try {
      readFileSync(join(candidate, "src", "codes.rs"), "utf8");
      readFileSync(join(candidate, "web", "src", "messages.ts"), "utf8");
      return candidate;
    } catch {

    }
    dir = candidate;
  }
  throw new Error(`no checkout root with src/codes.rs above ${here}`);
}

// The reason-key families the interface renders directly as message keys. A key without a catalog
// entry is shown as its raw text by the fallback of `dom.ts`. `runner.*` is deliberately absent: those
// refusals are written to the log and never become a page notice.
const INTERFACE_REASON_FAMILIES = [
  "reason.",
  "save.",
  "write.",
  "download.",
  "branch.",
  "restore.",
  "admission.",
  "drain.",
  "host.",
];

test("every reason key the interface renders has a message", () => {
  const codes = readFileSync(join(repoRoot(), "src", "codes.rs"), "utf8");
  const values = [...codes.matchAll(/pub const [A-Z0-9_]+: &str = "([^"]*)";/g)].map(
    (m) => m[1] as string,
  );
  const interfaceKeys = values.filter((value) =>
    INTERFACE_REASON_FAMILIES.some((family) => value.startsWith(family)),
  );
  assert.ok(
    interfaceKeys.length >= 40,
    `too few reason keys to be meaningful: ${interfaceKeys.length}`,
  );
  const catalog = new Set(messageKeys());
  const missing = interfaceKeys.filter((key) => !catalog.has(key));
  assert.deepEqual(missing, [], `reason keys without a message: ${missing.join(", ")}`);
});

test("interrupt sentences name the recorded start and leave the task id out", () => {
  const params = {
    time: "2026-10-05 02:28:11 UTC+09:00",
    source: "NVIDIA website",
    version: "595.104.02",
    phase: "Building",
    discovered: "2026-10-05 02:40:00 UTC+09:00",
    task_id: "should-not-appear",
  };
  for (const key of [
    "home.interrupted",
    "home.interrupted.file_kept",
    "home.interrupted.unload",
    "home.interrupted.phase_unknown",
  ]) {
    for (const lang of ["zh", "en"] as const) {
      const text = expandText(key, params, lang);
      assert.equal(text.includes("should-not-appear"), false, `${key} ${lang}`);
      assert.equal(text.includes("recorded start time"), false, `${key} ${lang}`);
      assert.equal(text.includes("开始时间选择"), false, `${key} ${lang}`);
      assert.match(text, /2026-10-05 02:28:11/);
      assert.match(text, /595\.104\.02/);
    }
  }
  const unknown = expandText("home.interrupted.phase_unknown", params, "en");
  assert.equal(unknown.includes("Building"), false);
  assert.equal(unknown.includes("at or after the driver file is written"), false);
  assert.match(expandText("home.interrupted.file_kept", params, "en"), /was not replaced/);
  assert.match(expandText("home.interrupted", params, "en"), /Whether the driver file was replaced cannot be determined/);
  assert.match(expandText("home.interrupted.unload", params, "zh"), /驱动模块可能已卸载/);
  assert.match(expandText("home.interrupted.artifact", {}, "zh"), /到本地驱动查看已保存的驱动/);
  assert.match(expandText("home.interrupted.artifact", {}, "en"), /See saved drivers in Local drivers/);
  assert.equal(expandText("home.interrupted.artifact", {}, "zh").includes("可能已经"), false);
  assert.equal(expandText("home.interrupted.artifact", {}, "en").includes("may already"), false);
});

test("only the last registered key warns that SSH sign-in will close", () => {
  for (const lang of ["zh", "en"] as const) {
    const last = expandText("ssh.keys.delete_last", {}, lang);
    const confirm = expandText("ssh.keys.delete_confirm", {}, lang);
    assert.match(confirm, lang === "zh" ? /本浏览器保持登录/ : /this browser stays signed in/);
    assert.match(confirm, lang === "zh" ? /其他浏览器会退出登录/ : /Other browsers sign out/);
    assert.equal(confirm.includes("最后一把"), false);
    assert.equal(confirm.includes("不再提供 SSH 登录"), false);
    assert.equal(/last registered/i.test(confirm), false);
    assert.equal(/no longer offers SSH/i.test(confirm), false);
    if (lang === "zh") {
      assert.match(last, /最后一把已登记的 SSH 公钥/);
      assert.match(last, /登录页不再提供 SSH 登录/);
    } else {
      assert.match(last, /last registered SSH public key/);
      assert.match(last, /no longer offers SSH sign-in/);
    }
  }
});

// A visible literal is a string or template literal in `src/views/` that the user reads: CJK prose, a
// multi-word Latin sentence, a single capitalised Latin word (`append("Delete")`), or a single word
// used where a node's text or content is set (`append(existing, "DELETE")`, `text: "delete"`,
// `node.textContent = "delete"`). Everything the user reads comes from the catalog; element tags,
// class names, ids, hrefs, message keys, API keys and enum values do not match these shapes.
// `literal` is already decoded, so an escape such as `\u5220\u9664`  or `\u0044ELETE` ("DELETE") is
// judged as the text it really renders. `textPosition` is the scanner's structural answer: the literal
// is an argument of a text sink, the value of a `text` property, the right-hand side of a `textContent
// =`, or the label after `"aria-label",`. `before` is the raw source text immediately before the
// literal and is read only when no structural answer was supplied: a fixed-width prefix cannot tell a
// comment, a later argument or a `${...}` from an identifier. `htmlPosition` says the browser parses
// the literal as HTML, so the guard judges the text it renders (elements, character references,
// text-bearing attributes), not its source characters.
function visibleLiteral(
  literal: string,
  before: string,
  textPosition?: boolean,
  htmlPosition = false,
): boolean {
  // An HTML position renders the string's text content: tags are markup, an entity is the character it
  // names, and a text-bearing attribute is read.
  const text = htmlPosition ? htmlText(literal) : literal;
  // UNKNOWN is a value shown as it is, in every position; it is not a missing translation, so the
  // guard never rejects it.
  if (text === "UNKNOWN") return false;
  // CJK prose in any plane, including the supplementary ideographs (U+20000–U+2FA1F): a Chinese
  // literal in `src/**/*.ts` is a missing key.
  if (/[\u3400-\u9fff\uf900-\ufaff\u{20000}-\u{2fa1f}]/u.test(text)) return true;
  // The scanner's structural answer is authoritative when it is supplied: `textPosition === false`
  // means the literal is not read where it sits (a class name, an enum value, a comparison operand),
  // so the prose shapes do not make it user text. Only an absent answer falls back to `before`.
  if (textPosition === false) return false;
  // Two Latin words: prose rather than an identifier. A newline or other whitespace between them
  // renders as a space, so it counts as a word break.
  if (/[A-Za-z][A-Za-z'.,:;!?-]*\s[A-Za-z]/.test(text)) return true;
  const word = text.trim();
  // One capitalised Latin word: a display word such as "Delete".
  if (/^[A-Z][a-z]+$/.test(word)) return true;
  // One word used as node text/content: `append("delete")`, `text: "DELETE"`. An all-caps word is a
  // display word only in a text position; `method: "POST"`, "UNKNOWN" and other technical constants
  // stay accepted. Display punctuation that belongs to the phrase (`"delete!"`) does not turn the word
  // into an identifier.
  if (!/^[a-z][a-z'-]*[.,:;!?]*$/.test(word) && !/^[A-Z][A-Z0-9]*[.,:;!?]*$/.test(word)) {
    return false;
  }
  return textPosition === undefined ? PREFIX_POSITION.test(before) : textPosition;
}

/** Positions a raw prefix can still show; the scanner supplies its own answer. */
const PREFIX_POSITION =
  /(?:\.(?:append|appendChild|createTextNode)\(\s*|textContent\s*=\s*|text\s*:\s*|"aria-label"\s*,\s*)$/;

// The text an HTML string renders: elements are markup, character references are the characters they
// name, and the value of a text-bearing attribute is read by the user. Only positions the browser
// parses as HTML (`innerHTML`, `insertAdjacentHTML`) use this: there the JavaScript literal is not the
// text the user reads.
function htmlText(source: string): string {
  let out = "";
  let i = 0;
  while (i < source.length) {
    const ch = source[i];
    if (ch === "<") {
      const end = htmlTagEnd(source, i);
      if (end === -1) {
        out += ch;
        i += 1;
        continue;
      }
      for (const value of visibleAttributeValues(source.slice(i, end + 1))) {
        out += ` ${value}`;
      }
      // The markup itself is not text; keep the words on either side apart.
      out += " ";
      i = end + 1;
      continue;
    }
    if (ch === "&") {
      const entity = htmlEntity(source, i);
      if (entity !== null) {
        out += entity.text;
        i = entity.end;
        continue;
      }
    }
    out += ch;
    i += 1;
  }
  return out;
}

/** The index of the `>` closing the tag that starts at `start`, or -1. */
function htmlTagEnd(source: string, start: number): number {
  let quote = "";
  for (let i = start + 1; i < source.length; i += 1) {
    const ch = source[i];
    if (quote !== "") {
      if (ch === quote) quote = "";
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (ch === ">") return i;
  }
  return -1;
}

/** The values of the text-bearing attributes of one HTML tag. */
function visibleAttributeValues(tag: string): string[] {
  const values: string[] = [];
  const re = /([A-Za-z][A-Za-z0-9:-]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(tag)) !== null) {
    const name = (m[1] ?? "").toLowerCase();
    if (VISIBLE_ATTRS.has(name)) values.push(m[2] ?? m[3] ?? m[4] ?? "");
  }
  return values;
}

/** The character named by the character reference at `start`, or null. */
function htmlEntity(source: string, start: number): { text: string; end: number } | null {
  const rest = source.slice(start);
  const named = /^&([A-Za-z][A-Za-z0-9]*);/.exec(rest);
  if (named !== null) {
    const value = NAMED_ENTITIES.get((named[1] ?? "").toLowerCase());
    if (value === undefined) return null;
    return { text: value, end: start + named[0].length };
  }
  const numeric = /^&#(?:([0-9]+)|[xX]([0-9a-fA-F]+));/.exec(rest);
  if (numeric === null) return null;
  const code =
    numeric[1] !== undefined ? parseInt(numeric[1], 10) : parseInt(numeric[2] ?? "", 16);
  if (!Number.isFinite(code) || code <= 0 || code > 0x10ffff) return null;
  if (code >= 0xd800 && code <= 0xdfff) return null;
  return { text: String.fromCodePoint(code), end: start + numeric[0].length };
}

/** The named character references that can appear in HTML text. */
const NAMED_ENTITIES = new Map<string, string>([
  ["amp", "&"],
  ["lt", "<"],
  ["gt", ">"],
  ["quot", '"'],
  ["apos", "'"],
  ["nbsp", " "],
  ["copy", "©"],
  ["reg", "®"],
  ["trade", "™"],
  ["deg", "°"],
  ["plusmn", "±"],
  ["mdash", "—"],
  ["ndash", "–"],
  ["hellip", "…"],
  ["middot", "·"],
  ["bull", "•"],
  ["lsquo", "‘"],
  ["rsquo", "’"],
  ["ldquo", "“"],
  ["rdquo", "”"],
  ["laquo", "«"],
  ["raquo", "»"],
  ["times", "×"],
  ["divide", "÷"],
  ["rarr", "→"],
  ["larr", "←"],
  ["uarr", "↑"],
  ["darr", "↓"],
  ["check", "✓"],
  ["cross", "✗"],
  ["euro", "€"],
  ["pound", "£"],
  ["yen", "¥"],
  ["cent", "¢"],
  ["frac12", "½"],
  ["sup2", "²"],
  ["sup3", "³"],
  ["sect", "§"],
  ["para", "¶"],
  ["permil", "‰"],
]);

/** Decode the body of a JavaScript string literal to its runtime text. */
function decodeLiteral(body: string): string {
  let out = "";
  for (let i = 0; i < body.length; i += 1) {
    const ch = body[i];
    if (ch !== "\\") {
      out += ch;
      continue;
    }
    i += 1;
    const esc = body[i];
    switch (esc) {
      case "n":
        out += "\n";
        break;
      case "t":
        out += "\t";
        break;
      case "r":
        out += "\r";
        break;
      case "b":
        out += "\b";
        break;
      case "f":
        out += "\f";
        break;
      case "v":
        out += "\v";
        break;
      case "0":
        out += "\0";
        break;
      case "x":
        out += String.fromCharCode(parseInt(body.slice(i + 1, i + 3), 16));
        i += 2;
        break;
      case "u": {
        if (body[i + 1] === "{") {
          const end = body.indexOf("}", i + 2);
          out += String.fromCodePoint(parseInt(body.slice(i + 2, end), 16));
          i = end;
        } else {
          out += String.fromCharCode(parseInt(body.slice(i + 1, i + 5), 16));
          i += 4;
        }
        break;
      }
      case "\n":
        // A line continuation contributes nothing.
        break;
      default:
        out += esc;
    }
  }
  return out;
}

interface ScannedLiteral {
  /** The decoded runtime text. */
  text: string;
  /** The raw source text immediately before the literal (diagnostics). */
  before: string;
  /** True when the code renders this literal to the user. */
  textPosition: boolean;
  /** True when the browser parses this literal as HTML before rendering. */
  html: boolean;
}

// Calls whose literal arguments reach the DOM as nodes or text: the argument indexes the user reads,
// or "all" when every argument is read.
const TEXT_SINK_CALLS = new Map<string, "all" | readonly number[]>([
  ["append", "all"],
  // appendChild takes one argument; counting every argument stays conservative for the malformed call as well.
  ["appendChild", "all"],
  ["createTextNode", [0]],
  ["insertAdjacentText", [1]],
  ["text", [0]],
]);

/** Calls whose named argument is HTML the browser parses and renders. */
const HTML_SINK_CALLS = new Map<string, number>([["insertAdjacentHTML", 1]]);

// Calls that only stringify: their value reaches the user exactly where the call's own value does, so
// the argument inherits the enclosing position.
const PASS_THROUGH_CALLS = new Set(["String"]);

/** How much source text the diagnostic `before` keeps. */
const BEFORE_WINDOW = 48;

interface Token {
  kind: "ident" | "number" | "string" | "chunk" | "regex" | "punct";
  /** Decoded runtime text for a string or template chunk; source text otherwise. */
  text: string;
  /** Offset of the token's first character in the source. */
  pos: number;
}

/** Keywords after which a `/` starts a regular expression, not a division. */
const REGEX_AFTER = new Set([
  "return",
  "typeof",
  "instanceof",
  "in",
  "of",
  "new",
  "delete",
  "void",
  "throw",
  "case",
  "do",
  "else",
  "yield",
  "await",
]);

/** Keywords whose `{` opens a block, not an object literal. */
const BLOCK_AFTER = new Set(["else", "do", "try", "finally"]);

/** Characters that form one operator token (`===`, `=>`, `?.`, `...`). */
const OPERATOR_CHARS = /[=!<>+\-*/%&|^?~.]/;

// Operators whose result is a boolean: a literal immediately after one is an operand, never the value
// the browser renders (`typeof x === "string"`, the DOM helper's own shape test).
const COMPARISON_OPERATORS = new Set(["===", "!==", "==", "!=", "<", ">", "<=", ">="]);

// Split `source` into tokens, following JavaScript's lexical grammar for the parts the guard depends
// on: line and block comments, single- and double-quoted strings with their escapes, template literals
// including the code and the nested templates of every `${...}`, and regular expressions (a `/` is
// read as a regex only where the previous token allows one). A template chunk is a `chunk` token
// because the text it carries renders like a string.
function lexJavaScript(source: string): Token[] {
  const tokens: Token[] = [];
  let i = 0;

  function push(kind: Token["kind"], text: string, pos: number): void {
    tokens.push({ kind, text, pos });
  }

  /** Skip whitespace and comments. */
  function skipTrivia(): void {
    while (i < source.length) {
      const ch = source[i];
      if (ch === " " || ch === "\t" || ch === "\r" || ch === "\n") {
        i += 1;
        continue;
      }
      if (ch === "/" && source[i + 1] === "/") {
        while (i < source.length && source[i] !== "\n") i += 1;
        continue;
      }
      if (ch === "/" && source[i + 1] === "*") {
        i += 2;
        while (i < source.length && !(source[i] === "*" && source[i + 1] === "/")) i += 1;
        i += 2;
        continue;
      }
      return;
    }
  }

  /** Read a quoted string; `quote` is the opening character. */
  function readString(quote: string): void {
    const pos = i;
    i += 1;
    let body = "";
    while (i < source.length && source[i] !== quote) {
      if (source[i] === "\\") {
        body += source[i] + (source[i + 1] ?? "");
        i += 2;
      } else {
        body += source[i];
        i += 1;
      }
    }
    i += 1;
    push("string", decodeLiteral(body), pos);
  }

  /** Emit the text between two positions of a template as a literal chunk. */
  function pushChunk(start: number, end: number): void {
    if (end > start) push("chunk", decodeLiteral(source.slice(start, end)), start);
  }

  /** True when a `/` here starts a regular expression rather than a division. */
  function regexAllowed(): boolean {
    const previous: Token | undefined = tokens[tokens.length - 1];
    if (previous === undefined) return true;
    if (previous.kind === "ident") return REGEX_AFTER.has(previous.text);
    if (previous.kind === "number" || previous.kind === "string" || previous.kind === "chunk") {
      return false;
    }
    return previous.text !== ")" && previous.text !== "]" && previous.text !== "}";
  }

  /** Read a regex literal; false when this `/` is a division after all. */
  function readRegex(): boolean {
    const pos = i;
    i += 1;
    let inClass = false;
    while (i < source.length && source[i] !== "\n" && source[i] !== "\r") {
      const ch = source[i];
      if (ch === "\\") {
        i += 2;
        continue;
      }
      if (ch === "[") inClass = true;
      else if (ch === "]") inClass = false;
      else if (ch === "/" && !inClass) {
        i += 1;
        while (i < source.length && /[a-z]/i.test(source[i])) i += 1;
        push("regex", source.slice(pos, i), pos);
        return true;
      }
      i += 1;
    }
    i = pos;
    return false;
  }

  // Read a run of code. Inside a `${...}` (`stopAtInterpolationEnd`) a `}` that closes no nested `{`
  // ends the interpolation and is left to the caller, so object literals and nested templates inside
  // it stay balanced.
  function readCode(stopAtInterpolationEnd: boolean): void {
    let depth = 0;
    while (i < source.length) {
      skipTrivia();
      if (i >= source.length) return;
      const ch = source[i];
      if (stopAtInterpolationEnd && (ch === "{" || ch === "}")) {
        if (ch === "}" && depth === 0) return;
        depth += ch === "{" ? 1 : -1;
        push("punct", ch, i);
        i += 1;
        continue;
      }
      if (!readToken()) return;
    }
  }

  /** Read a template literal, including the code of every `${...}`. */
  function readTemplate(): void {
    i += 1;
    let chunkStart = i;
    while (i < source.length) {
      const ch = source[i];
      if (ch === "\\") {
        i += 2;
        continue;
      }
      if (ch === "`") {
        pushChunk(chunkStart, i);
        i += 1;
        return;
      }
      if (ch === "$" && source[i + 1] === "{") {
        pushChunk(chunkStart, i);
        i += 2;
        readCode(true);
        if (source[i] === "}") i += 1;
        chunkStart = i;
        continue;
      }
      i += 1;
    }
    pushChunk(chunkStart, i);
  }

  /** Read one significant token; false at the end of the source. */
  function readToken(): boolean {
    skipTrivia();
    if (i >= source.length) return false;
    const pos = i;
    const ch = source[i];
    if (ch === '"' || ch === "'") {
      readString(ch);
      return true;
    }
    if (ch === "`") {
      readTemplate();
      return true;
    }
    if (ch === "/" && regexAllowed() && readRegex()) return true;
    if (/[A-Za-z_$]/.test(ch)) {
      while (i < source.length && /[A-Za-z0-9_$]/.test(source[i])) i += 1;
      push("ident", source.slice(pos, i), pos);
      return true;
    }
    if (/[0-9]/.test(ch)) {
      while (i < source.length && /[0-9A-Za-z_.]/.test(source[i])) i += 1;
      push("number", source.slice(pos, i), pos);
      return true;
    }
    while (i < source.length && OPERATOR_CHARS.test(source[i])) i += 1;
    if (i === pos) i += 1;
    push("punct", source.slice(pos, i), pos);
    return true;
  }

  readCode(false);
  return tokens;
}

// Where a literal sits, as the code renders it. `textPosition` is true when the literal belongs to an
// expression whose value the DOM helpers write to the user: an argument of a text sink, the value of a
// text-bearing property (`text`, `ariaLabel`, `title`, `placeholder`), an assignment to a text-bearing
// property (`node.textContent = …`, `node["textContent"] = …`), or an attribute of `setAttribute`
// whose value the user reads. The whole expression counts, so `condition ? "delete" : "keep"`,
// `nullable ?? "…"`, `"" + "…"`, an array of children and a parenthesised value are all read.
type FrameKind = "call" | "group" | "array" | "index" | "object" | "block" | "key";

interface Frame {
  kind: FrameKind;
  /** The callee of a call, or the property key of an object literal. */
  name: string | null;
  /** call: the argument being read (0-based); object: the element being read. */
  arg: number;
  /** True when the value being read here reaches the user as text. */
  text: boolean;
  /** True when the browser parses the value being read here as HTML. */
  html: boolean;
  /** object: the next top-level `:` ends this element's key. */
  keyPending: boolean;
  /** object: the current element's single-token key, when it has one. */
  keyText: string | null;
  /** call: the literal text of the first argument, when it is one literal. */
  firstArg: string | null;
  /** Significant tokens read in the current element. */
  elements: number;
}

function newFrame(kind: FrameKind, name: string | null = null, text = false, html = false): Frame {
  return {
    kind,
    name,
    arg: 0,
    text,
    html,
    keyPending: kind === "object",
    keyText: null,
    firstArg: null,
    elements: 0,
  };
}

/** DOM properties and object keys whose value the user reads as text. */
const VISIBLE_KEYS = new Set(["text", "ariaLabel", "title", "placeholder", "alt"]);

/** `setAttribute` names whose value the user reads as text. */
const VISIBLE_ATTRS = new Set(["aria-label", "title", "placeholder", "alt"]);

/** Property names whose assignment writes text the user reads. */
const VISIBLE_PROPS = new Set([
  "textContent",
  "innerText",
  "innerHTML",
  "outerHTML",
  "ariaLabel",
  "title",
  "placeholder",
]);

/** Assignment operators whose right-hand side is the assigned value. */
const ASSIGN = /^(=|\+=|\|\|=|&&=|\?\?=)$/;

/** What an assignment target renders: HTML text or plain text. */
type AssignTarget = "text" | "html";

/** Whether `name`'s `arg`-th argument is rendered to the user as text. */
function callArgVisible(name: string | null, arg: number, firstArg: string | null): boolean {
  if (name === null) return false;
  const sink = TEXT_SINK_CALLS.get(name);
  if (sink === "all") return true;
  if (sink !== undefined && sink.includes(arg)) return true;
  // el(tag, props, children): only the children are appended as nodes.
  if (name === "el") return arg >= 2;
  // setAttribute(name, value): only text-bearing attributes.
  if (name === "setAttribute") return arg === 1 && firstArg !== null && VISIBLE_ATTRS.has(firstArg);
  return false;
}

/** Whether `name`'s `arg`-th argument is HTML the browser renders as text. */
function isHtmlArg(name: string | null, arg: number): boolean {
  return name !== null && HTML_SINK_CALLS.get(name) === arg;
}

/** The visible property an assignment target names (`innerHTML` is HTML). */
function visibleTarget(name: string): AssignTarget {
  return name === "innerHTML" || name === "outerHTML" ? "html" : "text";
}

/** The name of a call written as `node["append"](…)` or `node[`append`](…)`. */
function computedCallee(tokens: Token[], paren: number): string | null {
  const key = tokens[paren - 2];
  const open = tokens[paren - 3];
  if (open?.text !== "[" || key === undefined) return null;
  if (key.kind === "string" || key.kind === "chunk") return key.text;
  return null;
}

/** The text-bearing target the token at `t` assigns, or null. */
function assignTarget(tokens: Token[], t: number): AssignTarget | null {
  const property = tokens[t - 1];
  if (property === undefined) return null;
  if (property.kind === "ident" && VISIBLE_PROPS.has(property.text)) {
    return visibleTarget(property.text);
  }
  // node["textContent"] = … / node[`textContent`] = …
  const key = tokens[t - 2];
  const open = tokens[t - 3];
  if (
    property.text === "]" &&
    open !== undefined &&
    open.text === "[" &&
    key !== undefined &&
    (key.kind === "string" || key.kind === "chunk") &&
    VISIBLE_PROPS.has(key.text)
  ) {
    return visibleTarget(key.text);
  }
  return null;
}

// Every string and template literal of a source file, comments excluded, each with the position the
// code gives it. The walk below follows the tokens of `lexJavaScript` and tracks, for every nesting
// level, whether the value being read reaches the user, so a literal is judged by where it really
// sits.
function literalsIn(source: string): ScannedLiteral[] {
  const tokens = lexJavaScript(source);
  const found: ScannedLiteral[] = [];
  const frames: Frame[] = [newFrame("block")];

  /** The frame that decides what a value is for; a group is only parentheses. */
  function top(): Frame {
    const frame = frames[frames.length - 1];
    if (!frame) throw new Error("empty frame stack");
    return frame;
  }

  /** Note one significant token in the element being read. */
  function note(token: Token): void {
    const frame = top();
    if (
      frame.kind === "object" &&
      frame.elements === 0 &&
      (token.kind === "ident" || token.kind === "string")
    ) {
      frame.keyText = token.text;
    }
    frame.elements += 1;
  }

  /** A top-level comma ends the current element of the frame. */
  function comma(frame: Frame): void {
    frame.elements = 0;
    if (frame.kind === "call") {
      frame.arg += 1;
      frame.html = isHtmlArg(frame.name, frame.arg);
      frame.text = callArgVisible(frame.name, frame.arg, frame.firstArg) || frame.html;
    } else if (frame.kind === "object") {
      frame.keyPending = true;
      frame.keyText = null;
      frame.text = false;
      frame.html = false;
    } else if (frame.kind !== "array") {
      frame.text = false;
      frame.html = false;
    }
  }

  for (let t = 0; t < tokens.length; t += 1) {
    const token = tokens[t];
    if (!token) continue;
    const previous: Token | undefined = tokens[t - 1];
    const literal = token.kind === "string" || token.kind === "chunk";
    const frame = top();
    if (literal) {
      if (
        frame.kind === "call" &&
        frame.arg === 0 &&
        frame.elements === 0 &&
        frame.firstArg === null
      ) {
        frame.firstArg = token.text;
      }
      if (frame.kind === "key" && frame.keyText === null) {
        // `{["text"]: …}` / `{[`text`]: …}`: the computed key is the name.
        frame.keyText = token.text;
      }
      // An operand of a comparison is never rendered: the comparison yields a boolean and the literal
      // stays in the expression being tested.
      const compared = previous !== undefined && COMPARISON_OPERATORS.has(previous.text);
      found.push({
        text: token.text,
        before: source.slice(Math.max(0, token.pos - BEFORE_WINDOW), token.pos),
        textPosition: compared ? false : frame.text,
        html: compared ? false : frame.html,
      });
      note(token);
      continue;
    }
    if (token.kind === "ident" || token.kind === "number" || token.kind === "regex") {
      note(token);
      continue;
    }
    switch (token.text) {
      case "(": {
        const optionalCall = previous !== undefined && previous.text === "?.";
        const call =
          previous !== undefined &&
          (previous.kind === "ident" ||
            previous.text === ")" ||
            previous.text === "]" ||
            optionalCall);
        let name: string | null = null;
        if (call && previous?.kind === "ident") {
          name = previous.text;
        } else if (optionalCall) {
          // `node.append?.(…)`
          const base = tokens[t - 2];
          if (base?.kind === "ident") name = base.text;
        } else if (call && previous?.text === "]") {
          // `node["append"](…)` / `node[`append`](…)`
          name = computedCallee(tokens, t);
        }
        note(token);
        const outer = top();
        const html = call && isHtmlArg(name, 0);
        let text = outer.text;
        if (call) {
          text =
            name !== null && PASS_THROUGH_CALLS.has(name)
              ? outer.text
              : callArgVisible(name, 0, null) || html;
        }
        frames.push(newFrame(call ? "call" : "group", name, text, call ? html : outer.html));
        break;
      }
      case ")":
      case "]":
      case "}": {
        const closing = top();
        if (frames.length > 1) frames.pop();
        if (closing.kind === "key") {
          const owner = top();
          if (owner.kind === "object" && owner.keyPending && owner.keyText === null) {
            owner.keyText = closing.keyText;
          }
        }
        break;
      }
      case "[": {
        const outer = top();
        // A computed property name (`{["text"]: …}`) is a key, not an index.
        const computedKey =
          outer.kind === "object" &&
          outer.keyPending &&
          outer.elements === 0 &&
          previous !== undefined &&
          (previous.text === "{" || previous.text === ",");
        note(token);
        if (computedKey) {
          frames.push(newFrame("key"));
          break;
        }
        const index =
          previous !== undefined &&
          (previous.kind === "ident" ||
            previous.kind === "number" ||
            previous.kind === "string" ||
            previous.kind === "chunk" ||
            previous.text === ")" ||
            previous.text === "]");
        frames.push(
          newFrame(index ? "index" : "array", null, !index && outer.text, !index && outer.html),
        );
        break;
      }
      case "{": {
        const outer = top();
        // A `:` after a one-token element is an object key only when the frame reads an object: `{ a:
        // { b: 1 } }`, not `case "x": { … }`.
        const value = outer.kind === "object" && outer.name !== null && outer.elements === 0;
        const block =
          !value &&
          (previous === undefined ||
            (previous.kind === "ident" && BLOCK_AFTER.has(previous.text)) ||
            (previous.kind === "punct" &&
              (previous.text === ")" ||
                previous.text === "=>" ||
                previous.text === ";" ||
                previous.text === "{" ||
                previous.text === "}" ||
                previous.text === ":")));
        note(token);
        frames.push(newFrame(block ? "block" : "object"));
        break;
      }
      case ",":
        comma(top());
        break;
      case ":": {
        const object = top();
        if (object.kind === "object" && object.keyPending && object.keyText !== null) {
          object.name = object.keyText;
          object.keyPending = false;
          object.keyText = null;
          object.elements = 0;
          object.text = VISIBLE_KEYS.has(object.name);
        } else {
          note(token);
        }
        break;
      }
      case ";": {
        const block = top();
        if (block.kind === "block") {
          block.text = false;
          block.html = false;
          block.elements = 0;
          block.keyText = null;
          block.keyPending = false;
        } else if (block.kind === "object") {
          // Interface fields use semicolons: the next field must not inherit title/text visibility.
          comma(block);
        } else {
          note(token);
        }
        break;
      }
      default:
        note(token);
        if (ASSIGN.test(token.text)) {
          const target = assignTarget(tokens, t);
          if (target !== null) {
            top().text = true;
            top().html = target === "html";
          }
        }
        break;
    }
  }
  return found;
}
/** The literals of `source` that the guard rejects: the full scan body. */
function offendersIn(source: string): ScannedLiteral[] {
  return literalsIn(source).filter((literal) =>
    visibleLiteral(literal.text, literal.before, literal.textPosition, literal.html),
  );
}

test("typed dialog policy fields do not inherit title visibility", () => {
  const declaration = 'interface DialogOptions { title: string; onCancel?: "remove" | "ignore" | ((event: Event) => void); }';
  assert.deepEqual(offendersIn(declaration).map((literal) => literal.text), []);
  assert.deepEqual(offendersIn(declaration + 'el("button", { text: "remove" }); el("button", { text: "ignore" });')
    .map((literal) => literal.text), ["remove", "ignore"]);
});

test("the visible-literal predicate rejects visible strings and accepts technical ones", () => {

  for (const sample of ["Delete", "Sign out", "Delete local drivers", "删除", "保存并继续"]) {
    assert.equal(visibleLiteral(sample, ""), true, `visible sample accepted: ${sample}`);
  }
  assert.equal(visibleLiteral("delete", 'document.body.append('), true);
  assert.equal(visibleLiteral("delete", '{ text: '), true);
  assert.equal(visibleLiteral("delete", 'node.textContent = '), true);
  // A single all-caps word in a text position is visible, and an escaped literal is judged by the text
  // it really renders.
  assert.equal(visibleLiteral("DELETE", 'document.body.append('), true);
  assert.equal(
    visibleLiteral(decodeLiteral("\\u5220\\u9664"), 'document.body.append('),
    true,
    "the escaped CJK literal must be decoded first",
  );
  assert.equal(visibleLiteral(decodeLiteral("\\u0044ELETE"), 'document.body.append('), true);

  for (const sample of [
    "div",
    "button",
    "aria-label",
    "checkbox",
    "application/json",
    "Content-Type",
    "POST",
    "drivers.source.upload",
    "status.UNKNOWN",
    "#/home",
    "10de:",
    "nvm-dialog",
    "reason.io_error",
    "list.object",
    "alert",
    "status",
    "UNKNOWN",
    "select-all",
    "row-${row.id}",
  ]) {
    assert.equal(visibleLiteral(sample, ""), false, `technical sample rejected: ${sample}`);
  }
  assert.equal(visibleLiteral("button", "el("), false);
  assert.equal(visibleLiteral("div", "el("), false);
  assert.equal(visibleLiteral("upload", 'kind: '), false);
  // Uppercase technical constants outside a text position stay accepted.
  assert.equal(visibleLiteral("POST", 'method: '), false);
  assert.equal(visibleLiteral("UNKNOWN", "?? "), false);
});

test("the literal scanner decodes escapes and skips comments", () => {
  const found = literalsIn(
    '// "ignored"\ndocument.body.append("\\u5220\\u9664"); // "also ignored"\nconst s = \'DELETE\';',
  );
  assert.deepEqual(
    found.map((f) => f.text),
    ["删除", "DELETE"],
  );
});

test("escaped and direct visible text are rejected through the full scan body", () => {
  // Escaped CJK and uppercase text must be caught by the same scan that runs over `src/views/`.
  for (const source of [
    'document.body.append("DELETE");',
    'document.body.append("\\u5220\\u9664");',
  ]) {
    const found = literalsIn(source);
    assert.equal(found.length, 1, source);
    const lit = found[0];
    assert.ok(lit);
    assert.equal(visibleLiteral(lit.text, lit.before), true, `not rejected: ${source}`);
  }

  const technical = literalsIn('el("button");');
  const lit = technical[0];
  assert.ok(lit);
  assert.equal(visibleLiteral(lit.text, lit.before), false);
});

test("the full scan checks visible and technical text positions", () => {
  const rejected = [
    'document.body.append(/* accessible text */ "DELETE");',
    'document.body.append(existingNode, "DELETE");',
    'document.body.append(existingNode, item, "DELETE");',
    'document.body.append(existingNode, "\\u5220\\u9664");',
    'document.body.append(/* comment */ el("p"), "DELETE");',
    'document.body.append(`${ "DELETE" }`);',
    'document.body.append(existingNode, `${ "\\u5220\\u9664" }`);',
    'document.body.append(`DELETE`);',
    'document.body.append(`\\u5220\\u9664`);',
    'node.textContent = /* accessible text */ "delete";',
    'el("span", { text: /* accessible text */ "delete" });',
    'el("input").setAttribute("aria-label", "delete");',
    'document.body.append(// accessible text\n  "DELETE");',
    'document.body.append(`${/* accessible text */ "DELETE"}`);',
    'el("span", { text /* accessible text */ : "delete" });',
    'node.textContent /* accessible text */ = "delete";',
    'parent.appendChild(existing, "DELETE");',
    'document.body.append(existingNode, ("DELETE"));',
    'document.body.append(`\n  ${\n    "DELETE"\n  },\n`);',
    'el("span", { text: `${"delete"}` });',
    'el("span", { text: `delete` });',
    'document.body.append("\u{20000}");',
    'document.body.append("\\u{20000}");',
    'document.body.append(el("span", { text: condition ? "delete" : "keep" }));',
    'document.body.append(el("span", { text: nullable ?? "delete" }));',
    'node.textContent = "" + "delete";',
    'document.body.append(text("delete"));',
    'document.body.append(el("span", {}, ["delete"]));',
    'document.body.append(el("button", { ariaLabel: "delete" }));',
    'document.body.append(el("button", { title: "delete" }));',
    'node.setAttribute("title", "delete");',
    'input.setAttribute("placeholder", "delete");',
    'document.body.append("delete\nselected");',
    'node["textContent"] = "delete";',
    'document.body.append(/* accessible */ "DELETE");',
    'document.body.append(document.createElement("span"), "DELETE");',
    'node.textContent = flag ? "delete" : "keep";',
    'node.textContent = nullable ?? "delete";',
    'node.textContent = ("delete");',
    'node.innerHTML = "<b>delete</b>";',
    'node.textContent = "delete!";',
    'node["append"]("delete");',
    'el("span", {["text"]: "delete"});',
    'el("span", {[`text`]: "delete"});',
    'node.textContent = String("delete");',
    'node.textContent = (() => "delete")();',
    'node.textContent = `de${"lete"}`;',
    'node.innerHTML = "&#100;elete";',
    'node.insertAdjacentHTML("beforeend", "<b>delete</b>");',
    'if (flag) /"x"/.test(""); node.textContent = "delete";',
  ];
  for (const source of rejected) {
    assert.ok(offendersIn(source).length > 0, `not rejected: ${source}`);
  }
  const accepted = [
    'const method = "POST";',
    'el("button");',
    'el("button", { type: "button", text: expandText("drivers.delete_row", {}, lang) });',
    'tr.append(el("td", { text: row.target_kernel ?? "UNKNOWN" }));',
    'tr.append(el("td", { text: row.size !== null ? formatMB(row.size) : "UNKNOWN" }));',
    'el("div", { class: "row-mismatch", id: `row-${row.id}` });',
    'check.setAttribute("aria-label", expandText("drivers.select_row", {}, lang));',
    'row.status === "UNKNOWN" ? "UNKNOWN" : row.status;',
    'const key = `drivers.source.${row.kind}`;',
    'el("span", { class: `row-${"row"}` });',
    'logPre.append(document.createTextNode(`${logLineText(app, line)}\n`));',
    'el("span", { text: row.status === "UNKNOWN" ? row.status : "" });',
    'const half = size / 2; document.body.append(renderNotice("x", {}, lang));',
    '// document.body.append("DELETE");',
    '/* document.body.append("\\u5220\\u9664"); */',
    'document.body.append("UNKNOWN");',
    'node.setAttribute("aria-describedby", "update-blocked");',
    'el("input", { type: "checkbox", value: "zh" });',
    'node.textContent = "UNKNOWN";',
    'el("span", {text: expandText("drivers.delete_row", {}, lang)});',
    'el("span", {class: `row-${"row"}`});',
    '/* node.textContent = "delete"; */',
    'const re = /delete/;',
  ];
  for (const source of accepted) {
    assert.deepEqual(offendersIn(source), [], `false positive: ${source}`);
  }
});

test("the scan body lexes comments, regexes, strings and nested templates", () => {
  // A regex that holds quotes and `//` must not hide the literal after it.
  const regex = "const re = /https?:\\/\\/x[\"']/; document.body.append(\"DELETE\");";
  assert.deepEqual(
    offendersIn(regex).map((literal) => literal.text),
    ["DELETE"],
  );
  // `//` inside a string is part of the string, not a comment.
  const string = 'const u = "a // b"; document.body.append("DELETE");';
  assert.deepEqual(
    offendersIn(string).map((literal) => literal.text),
    ["DELETE"],
  );
  // A template inside the interpolation of a template inside the argument.
  const nested = 'document.body.append(`a${`${"DELETE"}`}b`);';
  assert.ok(
    offendersIn(nested).some((literal) => literal.text === "DELETE"),
    `the nested interpolation was not scanned: ${nested}`,
  );
  // A comment is never a position, even when it looks like code.
  assert.deepEqual(offendersIn('foo(/* "aria-label", */ "POST");'), []);
});

test("compile-valid visible-text fixtures are rejected through the full scan body", () => {
  // These compile-valid fixtures render visible text that the scan must find.
  for (const name of [
    "html-content",
    "punctuated-label",
    "computed-call",
    "computed-object-key",
    "template-object-key",
    "inline-string-call",
    "entity-html",
    "adjacent-html",
    "nested-review-probe",
    "onclick-event",
    "control-direct-delete",
    "control-supplementary-cjk",
  ]) {
    assert.ok(
      offendersIn(fixture(name)).length > 0,
      `${name}: the visible literal passed the guard`,
    );
  }
  for (const name of ["control-UNKNOWN", "control-key-rendering"]) {
    assert.deepEqual(offendersIn(fixture(name)), [], `${name}: the technical control was rejected`);
  }
});

test("the source walk follows tsconfig's src/**/*.ts scope", () => {
  // The walk the scan test uses must descend into subdirectories: a probe file that `tsc -p` compiles
  // under `src/views/nested/` cannot escape the guard.
  const root = mkdtempSync(join(tmpdir(), "nvm-guard-scope-"));
  try {
    mkdirSync(join(root, "views", "nested"), { recursive: true });
    writeFileSync(join(root, "views", "top.ts"), "", "utf8");
    writeFileSync(join(root, "views", "nested", "deep.ts"), "", "utf8");
    writeFileSync(join(root, "views", "nested", "probe.ts.txt"), "", "utf8");
    const found = tsSourcesUnder(root)
      .map((path) => path.slice(root.length + 1))
      .sort();
    assert.deepEqual(found, ["views/nested/deep.ts", "views/top.ts"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("src/views has no visible literal outside the message catalog", () => {
  const offenders: string[] = [];
  let scanned = 0;
  let viewSources = 0;
  for (const path of tsSourcesUnder(srcDir)) {
    // The catalog is the one file whose literals are the text.
    if (path === join(srcDir, "messages.ts")) continue;
    const source = readFileSync(path, "utf8");
    scanned += 1;
    if (path.startsWith(`${viewsDir}/`)) viewSources += 1;
    for (const literal of offendersIn(source)) {
      offenders.push(`${path.slice(srcDir.length + 1)}: ${literal.text}`);
    }
  }
  // Reject an empty scan: the compiled tree contains only .js and cannot prove source coverage.
  assert.ok(scanned > 0, `no source scanned under ${srcDir}`);
  assert.ok(viewSources > 0, `no view source scanned under ${viewsDir}`);
  assert.deepEqual(offenders, []);
});

/** Tokenize the Rust declarations we bind to, excluding comments and opaque strings. */
function rustTokens(source: string): string[] {
  const tokens: string[] = [];
  let i = 0;
  while (i < source.length) {
    if (/\s/.test(source[i] ?? "")) { i += 1; continue; }
    if (source.startsWith("//", i)) {
      const end = source.indexOf("\n", i); i = end < 0 ? source.length : end; continue;
    }
    if (source.startsWith("/*", i)) {
      let depth = 1; i += 2;
      while (i < source.length && depth > 0) {
        if (source.startsWith("/*", i)) { depth += 1; i += 2; }
        else if (source.startsWith("*/", i)) { depth -= 1; i += 2; }
        else i += 1;
      }
      assert.equal(depth, 0, "unterminated Rust comment"); continue;
    }
    const raw = /^r(#+)?"/.exec(source.slice(i));
    if (raw) {
      const end = source.indexOf(`"${raw[1] ?? ""}`, i + raw[0].length);
      assert.ok(end >= 0, "unterminated Rust raw string");
      tokens.push(JSON.stringify(source.slice(i + raw[0].length, end)));
      i = end + 1 + (raw[1]?.length ?? 0); continue;
    }
    const character = /^'(?:\\.|[^'\\\n])'/.exec(source.slice(i));
    if (character) { tokens.push(character[0]); i += character[0].length; continue; }
    const quoted = /^"(?:\\[\s\S]|[^"\\])*"/.exec(source.slice(i));
    if (quoted) { tokens.push(quoted[0]); i += quoted[0].length; continue; }
    const word = /^[A-Za-z_][A-Za-z_0-9]*/.exec(source.slice(i));
    if (word) { tokens.push(word[0]); i += word[0].length; continue; }
    tokens.push(source[i] as string); i += 1;
  }
  return tokens;
}

function rustBlock(tokens: string[], start: number): string[] {
  assert.equal(tokens[start], "{");
  let depth = 1;
  let end = start + 1;
  while (end < tokens.length && depth > 0) {
    if (tokens[end] === "{") depth += 1;
    if (tokens[end] === "}") depth -= 1;
    end += 1;
  }
  assert.equal(depth, 0, "unclosed Rust block");
  return tokens.slice(start + 1, end - 1);
}

function rustOwner(tokens: string[], kind: string, name: string): string[] {
  const start = tokens.findIndex((token, i) => token === kind && tokens[i + 1] === name);
  assert.ok(start >= 0, `missing Rust ${kind} ${name}`);
  const brace = tokens.indexOf("{", start);
  assert.ok(brace >= 0, `missing body of ${name}`);
  return rustBlock(tokens, brace);
}

/** Keep a named expression's scope, rather than finding a key elsewhere in its function. */
function rustLet(tokens: string[], name: string): string[] {
  const start = tokens.findIndex((token, i) =>
    token === "let" && (tokens[i + 1] === name || (tokens[i + 1] === "mut" && tokens[i + 2] === name)));
  assert.ok(start >= 0, `missing Rust let ${name}`);
  let depth = 0;
  let end = start + 2;
  for (; end < tokens.length; end += 1) {
    const token = tokens[end];
    if (["{", "(", "["].includes(token ?? "")) depth += 1;
    if (["}", ")", "]"].includes(token ?? "")) depth -= 1;
    if (token === ";" && depth === 0) break;
  }
  return tokens.slice(start, end);
}

function rustFields(tokens: string[]): Set<string> {
  const fields = new Set<string>();
  let rename: string | undefined;
  for (let i = 0; i < tokens.length; i += 1) {
    if (tokens[i] === "rename" && tokens[i + 1] === "=" && tokens[i + 2]?.startsWith('"')) {
      rename = JSON.parse(tokens[i + 2] as string) as string;
    }
    if (/^[a-z_][a-z_0-9]*$/.test(tokens[i] ?? "") && tokens[i + 1] === ":" && tokens[i + 2] !== ":") {
      fields.add(rename ?? tokens[i] as string);
      rename = undefined;
    }
  }
  return fields;
}

/** Only JSON object keys and Map::insert keys count as emitted response fields. */
function rustJsonKeys(tokens: string[]): Set<string> {
  const keys = new Set<string>();
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i] ?? "";
    if (!token.startsWith('"')) continue;
    if (tokens[i + 1] === ":" || (tokens[i - 1] === "(" && tokens[i - 2] === "insert")) {
      keys.add(JSON.parse(token) as string);
    }
  }
  return keys;
}

interface BashToken { value: string; operator: boolean }

/** Read the guide's shell words without treating quoted or escaped hashes as comments. */
function bashTokens(source: string): BashToken[] {
  const tokens: BashToken[] = [];
  let word: string | undefined;
  let quote = "";
  const emitWord = (): void => {
    if (word !== undefined) tokens.push({ value: word, operator: false });
    word = undefined;
  };
  for (let i = 0; i < source.length; i += 1) {
    const char = source[i] as string;
    const next = source[i + 1];
    if (char === "\\" && quote !== "'") {
      assert.ok(next !== undefined, "unfinished Bash escape");
      if (next === "\n") { i += 1; continue; }
      if (!quote || '$`"\\'.includes(next)) {
        word = (word ?? "") + next; i += 1; continue;
      }
    }
    if (quote) {
      if (char === quote) quote = "";
      else word = (word ?? "") + char;
    } else if (char === "'" || char === '"') {
      word ??= ""; quote = char;
    } else if (char === "#" && word === undefined) {
      const end = source.indexOf("\n", i);
      i = end < 0 ? source.length : end - 1;
    } else if (/\s/.test(char)) {
      emitWord();
      if (char === "\n") tokens.push({ value: char, operator: true });
    } else if ("(){};|&<>".includes(char)) {
      emitWord(); tokens.push({ value: char, operator: true });
    } else word = (word ?? "") + char;
  }
  assert.equal(quote, "", "unclosed Bash quote");
  emitWord();
  return tokens;
}

function bashCommands(tokens: BashToken[]): string[][] {
  const commands: string[][] = [];
  let words: string[] = [];
  for (let i = 0; i <= tokens.length; i += 1) {
    const token = tokens[i] ?? { value: "\n", operator: true };
    if (!token.operator) words.push(token.value);
    else if (token.value === ">" || token.value === "<") {
      assert.ok(tokens[i + 1] && !tokens[i + 1]?.operator, "expected one Bash redirection target");
      i += 1;
    }
    else if (words.length) { commands.push(words); words = []; }
  }
  return commands;
}

function withoutHtmlComments(source: string): string {
  return source.replace(/<!--[\s\S]*?(?:-->|$)/g, "");
}

/** Match code tokens, excluding comments and keeping quoted words distinct from operators. */
function bashHasSequence(tokens: BashToken[], source: string): boolean {
  const code = tokens.filter((token) => !token.operator || token.value !== "\n");
  const expected = bashTokens(source).filter((token) => !token.operator || token.value !== "\n");
  return code.some((_, start) => expected.every((token, i) =>
    token.value === code[start + i]?.value && token.operator === code[start + i]?.operator));
}

interface AgentApiContract {
  endpoints: string[];
  fields: Record<string, string[]>;
  buttons: string[];
  statuses: string[];
  reasons: string[];
  confirmation_keys: string[];
}

function rustOwnerSource(root: string, owner: string): string[] {
  const paths = [join(root, "src", `${owner}.rs`)];
  if (owner === "web") {
    const walk = (dir: string): void => {
      for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
        const path = join(dir, entry.name);
        if (entry.isDirectory()) walk(path);
        else if (entry.isFile() && entry.name.endsWith(".rs")) paths.push(path);
      }
    };
    walk(join(root, "src", "web"));
  }
  return paths.flatMap((path) => rustTokens(readFileSync(path, "utf8")));
}

test("the web Rust owner includes newly added and nested modules", () => {
  const root = mkdtempSync(join(tmpdir(), "nvm-rust-owner-"));
  const modules = join(root, "src", "web");
  mkdirSync(join(modules, "nested"), { recursive: true });
  try {
    writeFileSync(join(root, "src", "web.rs"), 'fn router() { Router::new().route("/probe", get(probe)) }');
    writeFileSync(join(modules, "first.rs"), 'struct ProbeRequest { value: String }');
    const initial = rustOwnerSource(root, "web");
    assert.ok(rustOwner(initial, "fn", "router").includes('"/probe"'));
    assert.ok(rustFields(rustOwner(initial, "struct", "ProbeRequest")).has("value"));
    writeFileSync(join(modules, "added.rs"), 'fn probe() { ApiError::new(StatusCode::BAD_REQUEST, "probe.invalid") }');
    writeFileSync(join(modules, "nested", "later.rs"), 'fn nested_probe() {}');
    writeFileSync(join(modules, "ignored.txt"), 'fn not_rust() {}');
    const added = rustOwnerSource(root, "web");
    assert.ok(rustOwner(added, "fn", "probe").includes('"probe.invalid"'));
    assert.deepEqual(rustOwner(added, "fn", "nested_probe"), []);
    assert.ok(!added.includes("not_rust"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the agent guide binds every documented API token to its Rust owner", () => {
  const root = repoRoot();
  const guide = readFileSync(join(root, "docs", "agent.en.md"), "utf8");
  const [rawBody, inventory] = guide.split("<!-- agent-api-contract -->");
  assert.ok(rawBody && inventory, "missing agent API contract");
  const body = withoutHtmlComments(rawBody);
  const json = /```json\s*([\s\S]*?)```/.exec(inventory)?.[1];
  assert.ok(json, "missing contract JSON");
  const contract = JSON.parse(json) as AgentApiContract;
  const source = (file: string): string[] => rustOwnerSource(root, file);
  const web = source("web");
  const plan = source("plan");
  const state = source("task_state");
  const codes = source("codes");
  const gpu = source("gpu_users");
  const auth = source("auth");
  const api = rustTokens(readFileSync(join(root, "web", "src", "api.ts"), "utf8"));
  const fn = (name: string): string[] => rustOwner(web, "fn", name);
  const apiFn = (name: string): string => rustOwner(api, "function", name).join(" ");
  // HTTP field names are case insensitive; bind the actual read and write sites.
  const tokenRead = /headers \. get \( ("[^"]+") \)/.exec(rustOwner(auth, "fn", "check_session_at").join(" "))?.[1];
  assert.ok(tokenRead, "missing session token header read");
  const tokenHeader = (JSON.parse(tokenRead) as string).toLowerCase();
  assert.equal(tokenHeader, "x-nvm-token");
  const tokenWrite = /headers \. set \( ("[^"]+") , token \)/.exec(apiFn("withToken"))?.[1];
  assert.ok(tokenWrite, "missing API token header write");
  assert.equal((JSON.parse(tokenWrite) as string).toLowerCase(), tokenHeader);
  assert.ok(rustLet(fn("origin_guard"), "origin").join(" ").includes("headers . get ( header : : ORIGIN )"));
  assert.ok(rustLet(fn("from_request"), "content_type").join(" ").includes("req . headers ( ) . get ( header : : CONTENT_TYPE )"));
  assert.ok(fn("from_request").join(" ").includes('media_type . eq_ignore_ascii_case ( "application/json" )'));
  const jsonHeader = /headers : \{ ("[^"]+") : "application\/json" \}/.exec(apiFn("jsonInit"))?.[1];
  assert.ok(jsonHeader, "missing JSON request header");
  assert.equal((JSON.parse(jsonHeader) as string).toLowerCase(), "content-type");
  assert.ok(apiFn("getTask").includes("` ? after = $ { after } `"), "missing task log cursor query");
  const precheck = rustOwner(plan, "fn", "precheck");
  const confirmation = rustOwner(plan, "fn", "confirmation_dialog");
  assert.ok(rustLet(precheck, "dialog").join(" ").startsWith("let dialog = confirmation_dialog ("), "precheck must use the confirmation owner");
  const struct = (tokens: string[], name: string): Set<string> => rustFields(rustOwner(tokens, "struct", name));
  const jsonKeys = (name: string): Set<string> => rustJsonKeys(fn(name));
  const union = (...sets: Set<string>[]): Set<string> => new Set(sets.flatMap((set) => [...set]));
  const homeGpus = fn("home_gpus_value");
  const homeGpusBody = rustBlock(homeGpus, homeGpus.indexOf("{", homeGpus.lastIndexOf("json")));
  const homeGpuValue = rustBlock(homeGpusBody, homeGpusBody.indexOf("{", homeGpusBody.indexOf('"value"')));
  const planResponseArm = (variant: string): string[] => {
    const response = fn("plan_response");
    const start = response.findIndex((token, i) => token === "Precheck" && response[i + 3] === variant);
    assert.ok(start >= 0, `missing Precheck::${variant} response`);
    const next = response.findIndex((token, i) => i > start && token === "Precheck");
    return response.slice(start, next < 0 ? undefined : next);
  };
  const owners: Record<string, Set<string>> = {
    SshChallenge: rustJsonKeys(rustLet(fn("ssh_challenge"), "body")),
    SshLoginRequest: struct(web, "SshLoginRequest"),
    SshLoginResponse: rustJsonKeys(rustLet(fn("ssh_login_route"), "body")),
    HomeResponse: rustJsonKeys(rustLet(fn("home_response"), "body")),
    HomeGpus: union(rustJsonKeys(homeGpusBody), rustJsonKeys(rustLet(fn("read_home"), "gpus_json"))),
    HomeGpuValue: rustJsonKeys(homeGpuValue),
    HomeGpuDevice: rustJsonKeys(homeGpus),
    VersionsResponse: jsonKeys("read_versions"),
    GpuReading: rustJsonKeys(rustOwner(source("catalog"), "fn", "gpu_readings_json")),
    DriversResponse: jsonKeys("drivers_response"),
    GroupSummary: struct(source("upload"), "GroupSummary"),
    GroupSummaryRun: struct(source("upload"), "GroupSummaryRun"),
    DriverRow: jsonKeys("driver_row_json"),
    PlanRequest: struct(web, "PlanRequest"),
    PlanSource: union(struct(plan, "OfficialRequest"), struct(plan, "LocalRequest"), struct(plan, "UploadRequest")),
    PlanReady: jsonKeys("create_plan"),
    PlanDialog: struct(plan, "Dialog"),
    PlanDialogSource: rustJsonKeys(rustLet(confirmation, "source_json")),
    OriginalSaved: new Set<string>(),
    Confirmation: struct(plan, "Confirmation"),
    ConfirmationButton: struct(plan, "ConfirmationButton"),
    PlanBlocked: rustJsonKeys(planResponseArm("Blocked")),
    GpuBlock: struct(plan, "GpuState"),
    ObjectEntry: struct(gpu, "GpuUserJson"),
    PlanUncomputable: rustJsonKeys(planResponseArm("Uncomputable")),
    PlanChangedParams: jsonKeys("submit_reject_response"),
    SourceInvalidParams: jsonKeys("source_invalid_response"),
    TaskRequest: struct(web, "TaskRequest"),
    TaskSubmission: jsonKeys("submit_task"),
    TaskDetail: rustJsonKeys(rustLet(fn("task_detail_response"), "body")),
    LogTail: rustJsonKeys(rustLet(fn("task_detail_response"), "log_json")),
    LogLine: struct(state, "LogLine"),
    CheckRecord: struct(state, "CheckRecord"),
    ReplaceRecord: struct(state, "ReplaceRecord"),
    Reading: rustFields(rustOwner(state, "enum", "ReadingJson")),
    ReasonField: struct(state, "ReasonValue"),
    TaskError: struct(state, "ErrorRecord"),
    ApiError: rustJsonKeys(rustOwner(rustOwner(web, "impl", "ApiError"), "fn", "body")),
    Query: new Set(fn("task_detail").flatMap((token, i, tokens) =>
      token === "query_param" && tokens[i + 5]?.startsWith('"') ? [JSON.parse(tokens[i + 5] as string) as string] : [])),
  };
  // The stock metadata expression is a tuple binding inside precheck.
  const original = precheck.findIndex((token, i) => token === "original_json" && precheck[i - 1] === ",");
  assert.ok(original >= 0, "missing original metadata binding");
  owners.OriginalSaved = rustJsonKeys(rustBlock(precheck, precheck.indexOf("{", original)));
  assert.ok(homeGpuValue.join(" ").includes('"devices" : devices'), "home GPU devices must use the device array");
  assert.ok(homeGpus.join(" ").includes("devices : Vec < Value > = gpus . iter ( ) . map"), "home GPU devices must be an array");
  assert.ok(rustOwner(state, "struct", "TaskState").join(" ").includes("checks : Vec < CheckRecord >"), "task checks must be an array of CheckRecord");
  assert.ok(rustOwner(api, "interface", "TaskDetail").join(" ").includes("checks : CheckRecord [ ]"), "API task checks must be an array of CheckRecord");
  for (const [owner, fields] of Object.entries(contract.fields)) {
    assert.ok(owners[owner], `unknown contract owner ${owner}`);
    for (const field of fields) assert.ok(owners[owner]?.has(field), `${owner}.${field} is not emitted/accepted by its Rust owner`);
  }
  const router = rustOwner(web, "fn", "router");
  const routes = new Set<string>();
  for (let i = 0; i < router.length; i += 1) {
    if (router[i] !== "route" || router[i + 1] !== "(") continue;
    const path = JSON.parse(router[i + 2] as string) as string;
    let end = i + 4;
    let depth = 1;
    for (; end < router.length && depth > 0; end += 1) {
      if (router[end] === "(") depth += 1;
      if (router[end] === ")") depth -= 1;
    }
    for (const method of ["get", "post", "put", "delete"]) {
      if (router.slice(i + 4, end).some((token, j, tokens) => token === method && tokens[j + 1] === "(")) routes.add(`${method.toUpperCase()} ${path}`);
    }
  }
  for (const endpoint of contract.endpoints) assert.ok(routes.has(endpoint), `missing live route ${endpoint}`);
  const wireCasing = (tokens: string[], name: string, casing: string): void => {
    const start = tokens.findIndex((token, i) => token === "enum" && tokens[i + 1] === name);
    assert.ok(tokens.slice(start - 12, start).join(" ").includes(`serde ( rename_all = "${casing}" )`), `${name} wire casing changed`);
  };
  wireCasing(plan, "Button", "lowercase");
  wireCasing(state, "Pass", "snake_case");
  wireCasing(state, "Status", "SCREAMING_SNAKE_CASE");
  const checkPasses = rustOwner(state, "enum", "Pass").filter((token) => /^[A-Z][a-z]+$/.test(token)).map((token) => token.toLowerCase());
  assert.deepEqual(checkPasses, ["update", "restore"], "documented check passes differ from the API");
  const buttonVariants = rustOwner(plan, "enum", "Button").filter((token) => /^[A-Z][a-z]+$/.test(token)).map((token) => token.toLowerCase());
  assert.deepEqual([...contract.buttons].sort(), buttonVariants.sort());
  const statusStrings = rustOwner(rustOwner(state, "impl", "Status"), "fn", "as_str");
  for (const value of contract.statuses) {
    assert.ok(value === "UNKNOWN" ? fn("unreconciled_task_detail").includes('"UNKNOWN"')
      : statusStrings.includes(JSON.stringify(value)), `missing task status ${value}`);
  }
  const reasons = new Set<string>();
  // Error strings must be live ApiError constructors, declared reason constants, or the ErrorCode
  // serialization match, never comments or arbitrary literals.
  for (let i = 0; i < web.length; i += 1) {
    if (web[i] === "ApiError" && web.slice(i + 1, i + 4).join("") === "::new") {
      const comma = web.indexOf(",", i + 4);
      const value = web[comma + 1];
      if (value?.startsWith('"')) reasons.add(JSON.parse(value) as string);
    }
  }
  for (let i = 0; i < codes.length; i += 1) {
    if (codes[i] === "const") {
      const eq = codes.indexOf("=", i);
      const value = codes[eq + 1];
      if (value?.startsWith('"')) reasons.add(JSON.parse(value) as string);
    }
  }
  const errorCodes = rustOwner(rustOwner(codes, "impl", "ErrorCode"), "fn", "as_str");
  for (const token of errorCodes) if (token.startsWith('"')) reasons.add(JSON.parse(token) as string);
  for (const token of rustLet(fn("ssh_login_route"), "key")) {
    if (token.startsWith('"')) reasons.add(JSON.parse(token) as string);
  }
  for (const reason of contract.reasons) assert.ok(reasons.has(reason), `missing live reason ${reason}`);
  const confirmationKeys = [...new Set(confirmation
    .filter((token) => token.startsWith('"'))
    .map((token) => JSON.parse(token) as string)
    .filter((key) => key.startsWith("confirm.") || key === "original.skip_warning"))];
  assert.deepEqual([...contract.confirmation_keys].sort(), confirmationKeys.sort(), "confirmation key inventory drift");
  for (const [file, lang] of [["agent.en.md", "en"], ["agent.zh-CN.md", "zh"]] as const) {
    const guideText = readFileSync(join(root, "docs", file), "utf8");
    const guideBody = withoutHtmlComments(guideText.split("<!-- agent-api-contract -->")[0] as string);
    const fieldPaths: [string, string[]][] = [
      [".gpus.value.devices[].address", ["HomeResponse", "HomeGpus", "HomeGpuValue", "HomeGpuDevice"]],
      [".gpus.unknown", ["HomeResponse", "HomeGpus"]],
      [".gpus[].address", ["VersionsResponse", "GpuReading"]],
      [".dialog.original.saved", ["PlanReady", "PlanDialog", "OriginalSaved"]],
      [".dialog.original.version", ["PlanReady", "PlanDialog", "OriginalSaved"]],
      [".dialog.original.saved_at", ["PlanReady", "PlanDialog", "OriginalSaved"]],
      [".dialog.confirmation.original", ["PlanReady", "PlanDialog", "Confirmation"]],
      [".dialog.save_available", ["PlanReady", "PlanDialog"]],
    ];
    for (const [path, pathOwners] of fieldPaths) {
      assert.ok(guideBody.includes(`\`${path}\``), `${file}: missing explicit field path ${path}`);
      const segments = [...path.matchAll(/\.([a-z_][a-z_0-9]*)/g)].map((match) => match[1] as string);
      for (const [i, owner] of pathOwners.entries()) {
        assert.ok(contract.fields[owner]?.includes(segments[i] as string), `${file}: ${path} is missing ${owner}.${segments[i]} from the contract`);
      }
    }
    for (const pass of checkPasses) {
      assert.ok(guideBody.includes(`\`.checks[] | select(.pass == "${pass}")\``), `${file}: missing check array selector for ${pass}`);
    }
    const timeUnits: [RegExp, RegExp] = lang === "en" ? [
      /`saved_at` is Unix epoch milliseconds(?: \(divide by 1000 for seconds\)\.|; divide by 1000 for seconds\.)/,
      /`since_before` and `since_after` values are Unix epoch microseconds \(divide by 1000000 for seconds\);/,
    ] : [
      /`saved_at` (?:是|为) Unix epoch 毫秒，(?:换成|转为)秒时除以 1000。/,
      /`since_before` 和 `since_after` 的值是 Unix epoch 微秒，换成秒时除以 1000000；/,
    ];
    assert.match(guideBody, timeUnits[0], `${file}: saved_at must use epoch milliseconds and divisor 1000`);
    assert.match(guideBody, timeUnits[1], `${file}: since readings must use epoch microseconds and divisor 1000000`);
    const expectedHeaders: Map<string, string> = new Map([
      [tokenHeader, "$TOKEN"], ["origin", "$BASE"], ["content-type", "application/json"],
    ]);
    const blocks = [...guideBody.matchAll(/^```bash[ \t]*\r?\n([\s\S]*?)^```[ \t]*$/gm)]
      .map((match) => bashTokens(match[1] as string));
    assert.ok(blocks.length > 0, `${file}: missing Bash examples`);
    const commands = blocks.flatMap(bashCommands);
    const apiDefinitions: string[][] = [];
    for (const tokens of blocks) {
      for (let i = 0; i < tokens.length; i += 1) {
        if (tokens[i]?.operator || tokens[i]?.value !== "api" ||
            (i > 0 && !tokens[i - 1]?.operator) ||
            !["(", ")", "{"].every((value, j) => tokens[i + j + 1]?.operator && tokens[i + j + 1]?.value === value)) continue;
        let depth = 1;
        let end = i + 4;
        for (; end < tokens.length && depth > 0; end += 1) {
          if (tokens[end]?.operator && tokens[end]?.value === "{") depth += 1;
          if (tokens[end]?.operator && tokens[end]?.value === "}") depth -= 1;
        }
        assert.equal(depth, 0, `${file}: unclosed api() definition`);
        const bodyCommands = bashCommands(tokens.slice(i + 4, end - 1));
        assert.equal(bodyCommands.length, 1, `${file}: api() must directly run one curl command`);
        assert.equal(bodyCommands[0]?.[0], "curl", `${file}: api() must run curl`);
        apiDefinitions.push(bodyCommands[0] as string[]);
      }
    }
    assert.equal(apiDefinitions.length, 1, `${file}: expected one api() definition`);
    const headersOf = (words: string[]): string[] => words.flatMap((word, i) => word === "-H" ? [words[i + 1] ?? ""] : []);
    const parseHeader = (header: string): [string, string] => {
      const match = /^([^:]+): (.*)$/.exec(header);
      assert.ok(match, `${file}: malformed curl header`);
      return [(match[1] as string).toLowerCase(), match[2] as string];
    };
    const apiHeaders = headersOf(apiDefinitions[0] as string[]).map(parseHeader);
    assert.deepEqual([...new Set(apiHeaders.map(([name]) => name))].sort(),
      [...expectedHeaders.keys()].sort(), `${file}: api() request header names differ from the API`);
    for (const [name, value] of commands.filter((words) => ["curl", "api"].includes(words[0] ?? "")).flatMap(headersOf).map(parseHeader)) {
      assert.equal(value, expectedHeaders.get(name), `${file}: incorrect curl header name or value`);
    }
    const polls: { command: string[]; tokens: BashToken[] }[] = [];
    for (const tokens of blocks) {
      let inLoop = false;
      for (const words of bashCommands(tokens)) {
        if (words[0] === "while") { assert.equal(inLoop, false, `${file}: unsupported nested Bash loop`); inLoop = true; }
        else if (words[0] === "done") inLoop = false;
        else if (inLoop && words[0] === "api" && words.some((word) => word.startsWith("$BASE/api/tasks/$TASK_ID"))) polls.push({ command: words, tokens });
      }
      assert.equal(inLoop, false, `${file}: unclosed Bash loop`);
    }
    assert.equal(polls.length, 1, `${file}: expected one task polling command`);
    const poll = polls[0];
    assert.ok(poll, `${file}: missing task polling block`);
    assert.deepEqual(poll.command.slice(0, 3), ["api", "--max-time", "$REMAINING"], `${file}: task polling must bound each request to the remaining budget`);
    assert.ok(bashHasSequence(poll.tokens, "REMAINING=$((DEADLINE - SECONDS))\nif (( REMAINING <= 0 )); then break; fi\napi --max-time"), `${file}: task polling must reject an exhausted request budget`);
    const cursorQuery = /^\$BASE\/api\/tasks\/\$TASK_ID\?([a-z_][a-z_0-9]*)=\$AFTER$/.exec(poll.command.find((word) => word.startsWith("$BASE/api/")) ?? "");
    assert.ok(cursorQuery, `${file}: task polling command must send the log cursor`);
    assert.deepEqual([cursorQuery[1]], contract.fields.Query, `${file}: task log cursor query differs from the API`);
    const table = new Map<string, string>();
    for (const row of guideBody.matchAll(/^\| `((?:confirm|original)\.[^`]+)` \| (.*?) \|$/gm)) {
      const key = row[1] as string;
      assert.ok(contract.confirmation_keys.includes(key), `${file}: unlisted confirmation key ${key}`);
      assert.equal(table.has(key), false, `${file}: duplicate confirmation key ${key}`);
      table.set(key, row[2] as string);
      assert.equal(row[2], expandText(key, {}, lang), `${file}: confirmation text differs from the UI for ${key}`);
    }
    assert.deepEqual([...table.keys()].sort(), [...contract.confirmation_keys].sort(), `${file}: incomplete confirmation table`);
    const fields = new Set(Object.values(contract.fields).flat());
    for (const inline of guideBody.matchAll(/`([^`\n]+)`/g)) {
      if (!inline[1]?.startsWith(".")) continue;
      for (const match of (inline[1] ?? "").matchAll(/\.([a-z_][a-z_0-9]*)/g)) {
        assert.ok(fields.has(match[1] as string), `${file}: undocumented field contract ${match[0]}`);
      }
    }
    for (const bash of guideBody.matchAll(/```bash\s*([\s\S]*?)```/g)) {
      for (const quoted of (bash[1] ?? "").matchAll(/'([^'\n]*)'/g)) {
        const expression = quoted[1] ?? "";
        if (!expression.includes("{") && !expression.startsWith(".")) continue;
        for (const key of expression.matchAll(/[{,]\s*([a-z_][a-z_0-9]*)\s*(?=[:,}])|\.([a-z_][a-z_0-9]*)/g)) {
          const field = (key[1] ?? key[2]) as string;
          assert.ok(fields.has(field), `${file}: example field missing from contract: ${field}`);
        }
      }
    }
    const paths = contract.endpoints.map((endpoint) => endpoint.split(" ")[1] as string);
    for (const match of guideBody.matchAll(/\/api\/[a-z/-]+(?:\$TASK_ID|\{id\})?(?:\/[a-z-]+)?/g)) {
      const path = match[0].replace("$TASK_ID", "{id}").replace(/\/$/, "");
      assert.ok(paths.includes(path), `${file}: guide path missing from contract: ${path}`);
    }
  }
  for (const key of contract.confirmation_keys) {
    assert.ok(confirmation.includes(JSON.stringify(key)), `missing confirmation key ${key}`);
    assert.ok(MESSAGES[key], `missing confirmation translation ${key}`);
  }
  const buttonParagraph = /The buttons are ([^\n]+)/.exec(body)?.[1];
  assert.ok(buttonParagraph, "missing button guidance");
  for (const value of buttonParagraph.matchAll(/`([a-z_]+)`/g)) {
    assert.ok(contract.buttons.includes(value[1] as string), `guide button missing from contract: ${value[1]}`);
  }
  const outcomeParagraph = /Report task id,[^\n]+/.exec(body)?.[0];
  assert.ok(outcomeParagraph, "missing outcome guidance");
  for (const value of outcomeParagraph.matchAll(/`([a-z_A-Z]+)`/g)) {
    assert.ok(contract.statuses.includes(value[1] as string), `guide outcome missing from contract: ${value[1]}`);
  }
  const reasonSet = new Set(contract.reasons);
  for (const match of body.matchAll(/`([A-Z]+_[A-Z_]+|(?:ssh|login|auth|plan|task|reason|branch)\.[a-z_.]+)`/g)) {
    const key = match[1] as string;
    if (!contract.statuses.includes(key)) assert.ok(reasonSet.has(key), `guide reason missing from contract: ${key}`);
  }
});

test("HTML comments cannot supply guide text or code", () => {
  assert.equal(withoutHtmlComments("visible <!-- inline --> text\n<!--\n```bash\nignored\n```\n-->\nend"), "visible  text\n\nend");
  assert.equal(withoutHtmlComments("visible<!-- unfinished\nhidden"), "visible");
});

test("Bash sequence matching rejects commented and quoted guard copies", () => {
  const guard = "if (( REMAINING <= 0 )); then break; fi";
  assert.equal(bashHasSequence(bashTokens(guard), guard), true);
  assert.equal(bashHasSequence(bashTokens("# " + guard), guard), false);
  assert.equal(bashHasSequence(bashTokens(`printf '%s' '${guard}'`), guard), false);
  assert.equal(bashHasSequence(bashTokens(guard.replace("<=", "<")), guard), false);
  assert.equal(bashHasSequence(bashTokens("# note\n" + guard + " # note"), guard), true);
});

test("Bash command extraction excludes comments and preserves literal hashes", () => {
  const commands = bashCommands(bashTokens(String.raw`
    # curl -H "X-Wrong-Token: $TOKEN"
    printf '%s' 'single # hash' "double # hash" escaped\#hash bare#hash # trailing comment
    curl \
      -H "X-NVM-Token: $TOKEN" # -H "Wrong-Origin: $BASE"
    printf '%s' > api "$BASE/api/tasks/$TASK_ID?after=$AFTER" > "$WORK/task-next.json"
  `));
  assert.deepEqual(commands, [
    ["printf", "%s", "single # hash", "double # hash", "escaped#hash", "bare#hash"],
    ["curl", "-H", "X-NVM-Token: $TOKEN"],
    ["printf", "%s", "$BASE/api/tasks/$TASK_ID?after=$AFTER"],
  ]);
});

test("Rust contract extraction keeps comments, literals and adjacent owners out", () => {
  const tokens = rustTokens(`
    // struct Bound { pub stale: String }
    /* nested /* struct Bound { stale: bool } */ comment */
    struct Other { stale: String }
    struct Bound { #[serde(rename = "renamed")] field: String, next: bool }
    fn response() { let body = json!({"renamed": "stale", "next": true}); }
    fn unrelated() { let body = json!({"stale": true}); }
    const TEXT: &str = r#"fn response() { stale }"#;
    const QUOTE: char = '"';
  `);
  assert.deepEqual([...rustFields(rustOwner(tokens, "struct", "Bound"))], ["renamed", "next"]);
  const response = rustOwner(tokens, "fn", "response");
  assert.deepEqual([...rustJsonKeys(rustLet(response, "body"))], ["renamed", "next"]);
  assert.equal(response.includes("stale"), false);
});

test("setup log guidance finds the newest JSON message containing the code marker", () => {
  for (const key of ["setup.step.copy"]) {
    for (const lang of ["zh", "en"] as const) {
      const text = MESSAGES[key]![lang];
      assert.match(text, /SETUP CODE:/, `${key}.${lang} names the exact marker`);
      assert.match(text, lang === "zh" ? /最新一条包含/ : /newest log message containing/, `${key}.${lang} finds a message by content`);
      assert.match(text, /msg/, `${key}.${lang} identifies the JSON field`);
      assert.equal(/开头|starts with|SETUP CODE line/i.test(text), false);
    }
  }
  for (const lang of ["zh", "en"] as const) {
    assert.match(MESSAGES["setup.code_invalid"]![lang], /SETUP CODE:/);
    assert.match(MESSAGES["setup.code_invalid"]![lang], lang === "zh" ? /最新一条/ : /newest/);
    assert.equal(MESSAGES["setup.note"]![lang].includes("SETUP CODE:"), false);
    assert.match(MESSAGES["setup.help.body"]![lang], lang === "zh" ? /不写入磁盘/ : /never saved to disk/);
  }

});


test("Chinese TrueNAS interface guidance uses the translated menu names", () => {
  for (const key of messageKeys()) {
    assert.equal(/\b(?:Shell|Apps|Workloads|View Logs|Discover Apps|More Options|Install via YAML|System Settings)\b/.test(MESSAGES[key]!.zh), false, `${key}.zh uses translated menus`);
  }
  assert.match(MESSAGES["login.help.forgot_password"]!.zh, /TrueNAS 命令行/);
  assert.match(MESSAGES["help.reset_account"]!.zh, /TrueNAS 命令行/);
  assert.match(MESSAGES["extension.missing"]!.zh, /TrueNAS 的应用设置/);
});

const REJECTION_REASONS = {
  "branch.extra_cflags_removed": {
    zh: "内核的构建脚本已不再支持此版本驱动源码使用的 `EXTRA_CFLAGS`",
    en: "the kernel's build scripts no longer support the `EXTRA_CFLAGS` that this version's driver source uses",
  },
  "branch.fb_create_format_info": {
    zh: "此版本的 `nvidia-drm` 源码与当前内核头文件不匹配（例如 `fb_create` 多了一个 `drm_format_info` 参数）",
    en: "this version's `nvidia-drm` source does not match the current kernel headers (for example, `fb_create` takes an extra `drm_format_info` argument)",
  },
} as const;

const REJECTION_TEMPLATE = {
  zh: "error|版本 {version} 无法在本机构建：{unbuildable_reason}。",
  en: "error|Version {version} cannot be built on this host: {unbuildable_reason}.",
} as const;

test("version-specific rejection copy matches the proposed Chinese and English text", () => {
  assert.deepEqual(MESSAGES["plan.source_invalid.branch_unbuildable"], REJECTION_TEMPLATE);
  for (const [key, text] of Object.entries(REJECTION_REASONS)) assert.deepEqual(MESSAGES[key], text);
  assert.deepEqual(MESSAGES["branch.pci_resize_resource_args"], {
    zh: "内核的 `pci_resize_resource()` 参数个数与此版本驱动源码中的调用不一致",
    en: "the number of arguments of the kernel's `pci_resize_resource()` does not match the call in this version's driver source",
  });
});

for (const reason of Object.keys(REJECTION_REASONS) as (keyof typeof REJECTION_REASONS)[]) {
  for (const lang of ["zh", "en"] as const) {
    test(`a rejected plan renders ${reason} in ${lang}`, () => {
      installDom();
      const error = new ApiError(400, "plan.source_invalid.branch_unbuildable", {
        version: "580.119.02", from: null, reasons: [{ key: reason, params: {} }],
      }, undefined, null);
      const box = new FakeElement("div");
      renderPlanError({ lang } as never, box as never, error, false);
      const expected = REJECTION_TEMPLATE[lang].slice("error|".length)
        .replace("{version}", "580.119.02")
        .replace("{unbuildable_reason}", REJECTION_REASONS[reason][lang].replaceAll("`", ""));
      assert.equal(box.textContent, (lang === "zh" ? "错误" : "Error") + expected);
      assert.equal((box.children[0] as FakeElement).getAttribute("role"), "alert");
    });
  }
}
