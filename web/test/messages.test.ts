import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { expandText } from "../src/dom.js";
import { normalizeReason } from "../src/rules.js";
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
  const buildReasons = [...codes.matchAll(/pub const BUILD_[A-Z0-9_]+: &str = "([^"]*)";/g)]
    .map((match) => match[1] as string);
  assert.ok(buildReasons.length > 0, "no build reasons scanned");
  const plan = readFileSync(join(repoRoot(), "src", "plan.rs"), "utf8");
  const confirmationKeys = [...plan.matchAll(/"(confirm\.[a-z_.]+|original\.skip_warning)"/g)]
    .map((match) => match[1] as string);
  assert.ok(confirmationKeys.length > 0, "no confirmation keys scanned");
  const catalog = new Set(messageKeys());
  const missing = [
    ...interfaceKeys,
    ...buildReasons.map((reason) => catalog.has(`build.${reason}`) ? `build.${reason}` : normalizeReason(reason).key),
    ...confirmationKeys,
  ].filter((key) => !catalog.has(key));
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

// Everything the user reads comes from the catalog (`messages.ts`). The guard strips comments from
// each other `src/**/*.ts` file and rejects a string or template literal that holds CJK text or a
// capitalised Latin sentence ("Delete the drivers"). Keys, URLs, enum values and class lists have no
// capitalised first word or carry hyphens and digits, and an `Error` message is not interface text.
const LITERAL_OR_COMMENT =
  /\/\/[^\n]*|\/\*[\s\S]*?\*\/|"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|`(?:\\[\s\S]|[^`\\])*`/g;
const CJK = /[\u3400-\u9fff\uf900-\ufaff\u{20000}-\u{2fa1f}]/u;
const LATIN_SENTENCE = /^[A-Z][a-z']*(?:\s+[A-Za-z']+[.,:;!?]*)+$/;

function visibleLiterals(source: string): string[] {
  const found: string[] = [];
  for (const { 0: token, index } of source.matchAll(LITERAL_OR_COMMENT)) {
    if (token.startsWith("//") || token.startsWith("/*")) continue;
    if (source.slice(index - 10, index) === "new Error(") continue;
    // Text between `${` and `}` is code, not words the template shows.
    const text = token.slice(1, -1).replace(/\$\{[^}]*\}/g, "");
    if (CJK.test(text) || LATIN_SENTENCE.test(text)) found.push(token);
  }
  return found;
}

test("the guard rejects hard-coded Chinese and English sentences and ignores comments", () => {
  assert.deepEqual(visibleLiterals('// 删除 now\nel("p", { text: "删除" });'), ['"删除"']);
  assert.deepEqual(visibleLiterals("append(`Delete the drivers`)"), ["`Delete the drivers`"]);
  assert.deepEqual(visibleLiterals('/* Delete all */ el("div", { class: `row-${a} b` }); new Error("Bad input");'), []);
});

test("src has no visible literal outside the message catalog", () => {
  const offenders: string[] = [];
  let scanned = 0;
  for (const path of tsSourcesUnder(srcDir)) {
    if (path === join(srcDir, "messages.ts")) continue;
    scanned += 1;
    for (const literal of visibleLiterals(readFileSync(path, "utf8"))) {
      offenders.push(`${path.slice(srcDir.length + 1)}: ${literal}`);
    }
  }
  // The compiled tree holds only .js and cannot prove source coverage.
  assert.ok(scanned > 0, `no source scanned under ${srcDir}`);
  assert.deepEqual(offenders, []);
});


test("agent guide API paths exist in the router", () => {
  const root = repoRoot();
  const routes = new Set([...readFileSync(join(root, "src", "web.rs"), "utf8")
    .matchAll(/\.route\(\s*"([^"\n]+)"/g)].map((match) => match[1]));
  for (const file of ["agent.en.md", "agent.zh-CN.md"]) {
    const guide = readFileSync(join(root, "docs", file), "utf8");
    const paths = [...guide.matchAll(/\/api\/[A-Za-z0-9_/${}-]+/g)];
    assert.ok(paths.length > 0, `${file}: no API paths scanned`);
    for (const match of paths) {
      const path = match[0].replace("$TASK_ID", "{id}").replace(/\/$/, "");
      assert.ok(routes.has(path), `${file}: missing route ${path}`);
    }
  }
});

test("agent guide error codes and reason keys exist in source", () => {
  const root = repoRoot();
  const codes = new Set([...readFileSync(join(root, "src", "codes.rs"), "utf8")
    .matchAll(/"([^"\n]+)"/g)].map((match) => match[1]));
  const reasons = new Set([...codes, ...Object.keys(MESSAGES)]);
  const webSources = [join(root, "src", "web.rs")];
  const dirs = [join(root, "src", "web")];
  for (const dir of dirs) {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) dirs.push(path);
      else if (entry.name.endsWith(".rs")) webSources.push(path);
    }
  }
  for (const path of webSources) {
    for (const match of readFileSync(path, "utf8").matchAll(/"([^"\n]+)"/g)) reasons.add(match[1]);
  }
  for (const file of ["agent.en.md", "agent.zh-CN.md"]) {
    const guide = readFileSync(join(root, "docs", file), "utf8");
    const keys = [...guide.matchAll(/`([A-Z]+_[A-Z_]+|(?:ssh|login|auth|plan|task|reason|branch|notice)\.[a-z_.]+)`/g)]
      .map((match) => match[1] as string).filter((key) => key !== "STOCK_RESTORED");
    // Bare build reasons occur in the first cell of the failure table.
    for (const row of guide.matchAll(/^\| ([^|]+) \|/gm)) {
      for (const match of (row[1] as string).matchAll(/`([a-z]+_[a-z_]+)`/g)) keys.push(match[1] as string);
    }
    assert.ok(keys.length > 0, `${file}: no error codes or reason keys scanned`);
    for (const key of keys) {
      assert.ok((/^[A-Z]/.test(key) ? codes : reasons).has(key), `${file}: unknown error code or reason ${key}`);
    }
  }
});

test("agent guide confirmation tables match interface text", () => {
  const root = repoRoot();
  for (const [file, lang] of [["agent.en.md", "en"], ["agent.zh-CN.md", "zh"]] as const) {
    const guide = readFileSync(join(root, "docs", file), "utf8");
    const rows = [...guide.matchAll(/^\| `((?:confirm|original)\.[^`]+)` \| (.*?) \|$/gm)];
    assert.ok(rows.length > 0, `${file}: no confirmation rows scanned`);
    for (const row of rows) {
      assert.equal(row[2], expandText(row[1] as string, {}, lang), `${file}: confirmation text differs for ${row[1]}`);
    }
  }
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
  assert.equal(
    MESSAGES["extension.missing"]!.zh,
    "error|未检测到 TrueNAS 的 NVIDIA 驱动扩展。TrueNAS 25.10：打开应用 → 已安装 → 配置 → 设置，勾选安装 NVIDIA 驱动程序，点保存。TrueNAS 27：打开系统 → 高级设置，在 NVIDIA Drivers 卡片点配置，勾选 Enable NVIDIA GPU Support，点保存。",
  );
  assert.equal(
    MESSAGES["extension.missing"]!.en,
    "error|TrueNAS NVIDIA driver extension not found. TrueNAS 25.10: Apps → Installed → Configuration → Settings, tick Install NVIDIA Drivers, click Save. TrueNAS 27: System → Advanced Settings, click Configure on the NVIDIA Drivers card, tick Enable NVIDIA GPU Support, click Save.",
  );
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
