import { test } from "node:test";
import assert from "node:assert/strict";

import { installDom, FakeElement, FakeText } from "./dom-shim.js";
import { MESSAGES } from "../src/messages.js";
import { expand, expandText, formatTaskTime } from "../src/dom.js";
import { refreshObserved } from "../src/rules.js";
import { armUpdateCheckStatus, gpuBlockNode } from "../src/views/plan.js";
import { logParams, renderTask, waitingGpuMessageKey } from "../src/views/tasks.js";

installDom();
// `poll.ts` needs `window`; the rendering fixtures never fire the interval.
(globalThis as unknown as { window: unknown }).window = {
  setInterval: (): number => 1,
  clearInterval: (): void => {},
};

function seg(exp: ReturnType<typeof expand>, kind: string): string {
  return exp.segments
    .filter((s) => s.kind === kind)
    .map((s) => s.nodes.map((n) => n.text).join(""))
    .join("\n");
}

function placeholders(text: string): string[] {
  const out: string[] = [];
  const re = /\{([A-Za-z_][A-Za-z0-9_]*)\}/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) out.push(m[1] as string);
  return out.sort();
}

test("the Chinese single-row delete texts keep the count", () => {
  assert.equal(
    seg(expand("delete.confirm.one", { row_count: 1 }, "zh"), "body"),
    "将删除下面这个本地驱动在数据目录中的文件。正式路径上的驱动文件保留。",
  );
  assert.equal(
    seg(expand("delete.confirm.one", { row_count: 1 }, "en"), "body"),
    "This deletes the data-directory file of the local driver below. The driver file at the permanent path stays.",
  );
  assert.equal(expandText("delete.done.one", { row_count: 1 }, "zh"), "已删除 1 个本地驱动。");
  assert.equal(expandText("delete.done.one", { row_count: 1 }, "en"), "Deleted the local driver.");

  assert.equal(
    seg(expand("delete.confirm", { row_count: 3 }, "zh"), "body"),
    "将删除以下 3 个本地驱动在数据目录中的文件。正式路径上的驱动文件保留。",
  );
  assert.equal(expandText("delete.done", { row_count: 3 }, "zh"), "已删除 3 个本地驱动。");

  for (const key of ["delete.confirm.one", "delete.confirm", "delete.done.one", "delete.done"]) {
    const msg = MESSAGES[key];
    assert.ok(msg, key);
    assert.deepEqual(placeholders(msg.zh), placeholders(msg.en), key);
  }
});

test("the first task read is not duplicated by the timer", async () => {
  // Capture the interval callback so the test can fire it while the first read is still in flight.
  let fire: (() => void) | null = null;
  const previousWindow = (globalThis as unknown as { window: unknown }).window;
  (globalThis as unknown as { window: unknown }).window = {
    setInterval: (fn: () => void): number => {
      fire = fn;
      return 1;
    },
    clearInterval: (): void => {},
  };
  let calls = 0;
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  (globalThis as unknown as { fetch: unknown }).fetch = async (): Promise<unknown> => {
    calls += 1;
    await gate;
    return { ok: true, status: 200, json: async (): Promise<unknown> => taskDetail([], "task") };
  };
  const container = new FakeElement("div");
  renderTask(
    { ...fakeApp, lang: "zh" } as never,
    container as unknown as HTMLElement,
    "audit-task",
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(calls, 1, "the first read did not start");
  (fire as unknown as () => void)();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(calls, 1, "the timer started a second read while the first was in flight");
  release();
  await new Promise((resolve) => setTimeout(resolve, 0));
  (globalThis as unknown as { window: unknown }).window = previousWindow;
});

test("the pre-delivery failure reason expands from its real shape", () => {
  const sha = logParams("log.artifact_check_failed", { build_reason: "artifact.sha_mismatch" });
  assert.equal(
    expandText("log.artifact_check_failed", sha, "zh"),
    "交付前核对未通过：构建出的驱动校验值与构建记录不符。",
  );
  assert.equal(
    expandText("log.artifact_check_failed", sha, "en"),
    "Pre-delivery check failed: the checksum of the built driver does not match the build record.",
  );
  const kernel = logParams("log.artifact_check_failed", {
    build_reason: {
      key: "artifact.kernel_mismatch",
      params: { built_kernel: "6.12.105", current_kernel: "6.12.96" },
    },
  });
  assert.equal(
    expandText("log.artifact_check_failed", kernel, "zh"),
    "交付前核对未通过：构建出的驱动目标内核为 6.12.105，当前内核为 6.12.96。",
  );
  const version = logParams("log.artifact_check_failed", {
    build_reason: {
      key: "artifact.version_mismatch",
      params: { built_version: "580.178.04", target: "590.48.01" },
    },
  });
  assert.equal(
    expandText("log.artifact_check_failed", version, "zh"),
    "交付前核对未通过：构建出的驱动版本为 580.178.04，目标版本为 590.48.01。",
  );
  const tool = logParams("log.artifact_check_failed", {
    build_reason: { key: "artifact.tool_missing", params: { program: "nvidia-ctk" } },
  });
  assert.equal(
    expandText("log.artifact_check_failed", tool, "en"),
    "Pre-delivery check failed: the built driver has no executable nvidia-ctk.",
  );
  // Every artifact reason of the closed set expands to its phrase and never leaks the internal code.
  for (const code of [
    "artifact.sha_mismatch",
    "artifact.kernel_mismatch",
    "artifact.kernel_ambiguous",
    "artifact.version_mismatch",
    "artifact.version_unreadable",
    "artifact.tool_missing",
    "artifact.unreadable",
    "artifact.deliver_failed",
  ]) {
    for (const lang of ["zh", "en"] as const) {
      const text = expandText(
        "log.artifact_check_failed",
        logParams("log.artifact_check_failed", { build_reason: code }),
        lang,
      );
      assert.equal(text.includes(code), false, `${code} ${lang}: ${text}`);
      assert.equal(text.includes("{"), false, `${code} ${lang}: ${text}`);
    }
  }
  // A shape that carries no reason at all is shown as UNKNOWN, never guessed.
  assert.match(
    expandText("log.artifact_check_failed", logParams("log.artifact_check_failed", {}), "zh"),
    /交付前核对未通过：UNKNOWN。/,
  );
});

test("the six cleanup steps expand to their names", () => {
  const steps: Record<string, [string, string]> = {
    delete_temp_file: ["删除临时文件", "delete the temporary file"],
    unmount_writable_mount: ["卸载可写挂载", "unmount the writable mount"],
    restore_readonly: ["设回只读属性", "restore the readonly property"],
    remove_build_container: ["删除构建容器", "remove the build container"],
    delete_state_temps: ["删除状态文件的临时文件", "delete the state file's temporary files"],
    delete_task_dir: ["删除任务目录", "delete the task directory"],
  };
  for (const [code, [zh, en]] of Object.entries(steps)) {
    const p = logParams("log.cleanup_failed", {
      cleanup_step: code,
      reason: "RAW_FROM_COMMAND",
    });
    assert.equal(
      expandText("log.cleanup_failed", p, "zh"),
      `清理步骤「${zh}」失败：RAW_FROM_COMMAND。`,
      code,
    );
    assert.equal(
      expandText("log.cleanup_failed", p, "en"),
      `Cleanup step "${en}" failed: RAW_FROM_COMMAND.`,
      code,
    );
  }
});

test("a signaled cleanup command expands its structured reason in both languages", () => {
  const p = logParams("log.cleanup_failed", {
    cleanup_step: "remove_build_container",
    reason: { key: "reason.signal", params: { signal: 15 }, raw: "docker: killed" },
  });
  assert.equal(expandText("log.cleanup_failed", p, "zh"),
    "清理步骤「删除构建容器」失败：命令被信号 15 终止。");
  assert.equal(expandText("log.cleanup_failed", p, "en"),
    "Cleanup step \"remove the build container\" failed: the command was terminated by signal 15.");
  assert.equal(logParams("log.cleanup_failed", {
    cleanup_step: "remove_build_container",
    reason: { key: "status.SUCCEEDED", params: {} },
  })["reason"], "UNKNOWN");
});

test("a cache failure expands its I/O reason and says the build continues", () => {
  const p = logParams("log.build_kit_cache", {
    reason: { key: "reason.permission_denied", params: {}, raw: "listing refused" },
  });
  assert.equal(
    expandText("log.build_kit_cache", p, "zh"),
    "编译包缓存未使用：没有读取权限，继续构建。",
  );
  assert.equal(
    expandText("log.build_kit_cache", p, "en"),
    "The build-package cache was not used: permission was denied; the build continues.",
  );
});

test("row_created maps the real source and the published fingerprint", () => {
  const upload = logParams("log.row_created", {
    version: "580.178.04",
    source: "upload",
    fingerprint: "a1b2c3",
  });
  assert.deepEqual(upload["source"], {
    key: "drivers.source.upload",
    params: { fingerprint: "a1b2c3" },
  });
  assert.equal(
    expandText("log.row_created", upload, "zh"),
    "本地驱动表新增一行：580.178.04（手动上传文件 · 指纹 a1b2c3）。",
  );
  assert.equal(
    expandText("log.row_created", upload, "en"),
    "Added a row to Local drivers: 580.178.04 (Files uploaded manually · fingerprint a1b2c3).",
  );
  const official = logParams("log.row_created", { version: "580.178.04", source: "official" });
  assert.deepEqual(official["source"], {
    key: "drivers.source.official",
    params: { fingerprint: "" },
  });
  assert.equal(
    expandText("log.row_created", official, "en"),
    "Added a row to Local drivers: 580.178.04 (Official build).",
  );
  // A missing source is reported as UNKNOWN, never guessed as the official one.
  const missing = logParams("log.row_created", { version: "580.178.04" });
  assert.equal(missing["source"], "UNKNOWN");
  assert.equal(
    expandText("log.row_created", missing, "zh"),
    "本地驱动表新增一行：580.178.04（UNKNOWN）。",
  );
  // Source labels use source_kind, independently of the kind field.
  const kindOnly = logParams("log.row_created", { version: "1", kind: "official" });
  assert.equal(kindOnly["source"], "UNKNOWN");
});

test("upload guidance names the optional installer and checksum files", () => {
  assert.equal(MESSAGES["upload.run.skip"]!.zh, "下载页上的其他文件");
  for (const lang of ["zh", "en"] as const) {
    const help = expandText("upload.run.skip.body", {}, lang);
    assert.ok(help.includes("no-compat32"));
    assert.ok(help.includes(".md5sum"));
    assert.ok(help.includes(".sha256sum"));
    assert.match(help, lang === "zh" ? /不用上传/ : /Do not upload/);
  }
});

interface GpuBlockFixture {
  state: "busy" | "unreadable";
  users: { name: string; type: string }[];
  errors: { type: string; reason: { key: string; params: Record<string, unknown> } }[];
  read_reason: { key: string; params: Record<string, unknown> };
}

const fakeApp = {
  lang: "zh" as const,
  addCleanup: (): void => {},
  sessionEnded: (): void => {},
  refreshLogin: async (): Promise<void> => {},
  reportTaskState(): void {},
};

type AnyNode = FakeElement | FakeText;

function isElement(node: AnyNode): node is FakeElement {
  return node instanceof FakeElement;
}

function allElements(node: AnyNode): FakeElement[] {
  if (!isElement(node)) return [];
  return [node, ...node.children.flatMap((child) => allElements(child))];
}

/** Which of action / list / note a rendered notice tree carries. */
function noticeParts(node: AnyNode): { action: boolean; list: boolean; note: boolean } {
  const found = allElements(node);
  const has = (name: string): boolean =>
    found.some((n) => n.className.split(/\s+/).includes(name));
  return { action: has("notice-action"), list: has("notice-list"), note: has("notice-note") };
}

test("the plan unreadable GPU block keeps Action and List, and never the Note", () => {
  const app = fakeApp as unknown as Parameters<typeof gpuBlockNode>[0];
  const unreadable: GpuBlockFixture = {
    state: "unreadable",
    users: [{ name: "media-service", type: "docker" }],
    errors: [],
    read_reason: { key: "reason.permission_denied", params: {} },
  };
  const nonEmpty = gpuBlockNode(app, unreadable as never);
  assert.deepEqual(noticeParts(nonEmpty as unknown as AnyNode), {
    action: true,
    list: true,
    note: false,
  });
  assert.match((nonEmpty as unknown as FakeElement).textContent, /media-service/);
  assert.match((nonEmpty as unknown as FakeElement).textContent, /请停止标为「正在使用显卡」的应用/);

  const empty = gpuBlockNode(app, { ...unreadable, users: [] } as never);
  assert.deepEqual(noticeParts(empty as unknown as AnyNode), {
    action: false,
    list: false,
    note: false,
  });

  const busy = gpuBlockNode(app, { ...unreadable, state: "busy" } as never);
  assert.deepEqual(noticeParts(busy as unknown as AnyNode), {
    action: true,
    list: true,
    note: true,
  });
  const busyEmpty = gpuBlockNode(app, { ...unreadable, state: "busy", users: [] } as never);
  assert.deepEqual(noticeParts(busyEmpty as unknown as AnyNode), {
    action: false,
    list: false,
    note: true,
  });
});

function taskDetail(users: { name: string; type: string }[], variant: "task" | "task_unreadable") {
  return {
    task_id: "audit-task",
    status: "FAILED",
    phase: "unload",
    plan: null,
    loaded_version_start: null,
    loaded_version_end: null,
    download: null,
    replace: [],
    checks: [],
    restore: null,
    state_read_error: null,
    state_note: null,
    unit_query: null,
    error: {
      code: "GPU_IN_USE",
      params: {
        occupancy: variant === "task" ? "busy" : "unreadable",
        ...(variant === "task_unreadable"
          ? { read_reason: { key: "reason.permission_denied", params: {} } }
          : {}),
      },
      command: null,
      exit: null,
      raw: null,
    },
    gpu_users: { users, errors: [] },
    version_change: false,
    notice: { key: "notice.gpu_in_use", variant, params: {} },
    log: { lines: [], next: 0, reset: false },
  };
}

async function renderDetailContainer(detail: unknown, lang: "zh" | "en" = "zh"): Promise<FakeElement> {
  (globalThis as unknown as { fetch: unknown }).fetch = async (): Promise<unknown> => ({
    ok: true,
    status: 200,
    json: async (): Promise<unknown> => detail,
  });
  const container = new FakeElement("div");
  renderTask(
    { ...fakeApp, lang } as never,
    container as unknown as HTMLElement,
    "audit-task",
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
  return container;
}

async function renderDetail(detail: unknown): Promise<FakeElement> {
  const container = await renderDetailContainer(detail);
  const area = allElements(container).find((n) =>
    n.className.split(/\s+/).includes("task-notice"),
  );
  assert.ok(area, "the task notice area was rendered");
  return area;
}

test("task status keeps each label paired with its value in both languages", async () => {
  for (const lang of ["en", "zh"] as const) {
    const container = await renderDetailContainer(taskDetail([], "task"), lang);
    const status = allElements(container).find((node) => node.className === "task-state");
    assert.ok(status);
    assert.equal(status.getAttribute("role"), "status");
    const readings = status.children.filter((node) => node instanceof FakeElement) as FakeElement[];
    const labeled = readings.filter((node) => node.className === "reading");
    assert.equal(labeled.length, 2);
    const wait = readings.find((node) => node.className === "gpu-users");
    assert.ok(wait);
    assert.equal(wait.textContent, "");
    for (const [index, key] of ["task.status", "task.phase"].entries()) {
      const reading = labeled[index]!;
      assert.equal(reading.className, "reading");
      const [label, value] = reading.children as FakeElement[];
      assert.equal(label!.className, "reading-label");
      assert.equal(label!.textContent, expandText(key, {}, lang));
      assert.equal(value!.className, "reading-value");
      assert.ok(value!.textContent.length > 0);
    }
  }
});

test("task times distinguish running tasks from missing or invalid recorded times", async () => {
  for (const lang of ["en", "zh"] as const) {
    for (const status of ["RUNNING", "SUCCEEDED", "FAILED", "STOCK_RESTORED", "INTERRUPTED"]) {
      for (const time of [null, undefined, NaN, Infinity, 9e20, 1_760_000_060_000]) {
        const container = await renderDetailContainer({
          ...taskDetail([], "task"), status, phase: null,
          created_at: time, finished_at: time, interrupt_discovered_at: time,
        }, lang);
        const times = allElements(container).find((node) => node.className === "task-times")!;
        const values = allElements(times).filter((node) => node.className === "reading-value");
        const valid = time === 1_760_000_060_000;
        const missing = time === null || time === undefined;
        assert.equal(values[0]!.textContent, valid
          ? formatTaskTime(time)
          : lang === "en" ? "Not recorded" : "未记录");
        assert.equal(values[1]!.textContent, valid
          ? formatTaskTime(time)
          : status === "RUNNING" && missing
            ? lang === "en" ? "Not finished yet" : "尚未结束"
            : lang === "en" ? "Not recorded" : "未记录");
        assert.equal(values[2]!.textContent.includes("UNKNOWN"), false);
        const state = allElements(container).find((node) => node.className === "task-state")!;
        assert.equal(state.textContent.includes("UNKNOWN"), false);
      }
    }
  }
});

test("task details only appear when there is additional content", async () => {
  for (const lang of ["en", "zh"] as const) {
    const detail = { ...taskDetail([], "task"), error: null, notice: null };
    const empty = await renderDetailContainer(detail, lang);
    assert.equal(allElements(empty).some((node) => node.className === "task-details"), false);
    assert.equal(empty.textContent.includes(expandText("task.details", {}, lang)), false);
    const filled = await renderDetailContainer({
      ...detail, state_note: { kept_as: "state-unreadable.json" },
    }, lang);
    const section = allElements(filled).find((node) => node.className === "task-details");
    assert.ok(section);
    assert.equal(section.children[0]!.textContent, expandText("task.details", {}, lang));
    assert.match(section.textContent, /state-unreadable.json/);
  }
});

test("the task-detail unreadable notice keeps Action and List and never the Note", async () => {
  const nonEmpty = await renderDetail(taskDetail([{ name: "media-service", type: "docker" }], "task_unreadable"));
  assert.deepEqual(noticeParts(nonEmpty), { action: true, list: true, note: false });
  assert.match(nonEmpty.textContent, /media-service/);

  const empty = await renderDetail(taskDetail([], "task_unreadable"));
  assert.deepEqual(noticeParts(empty), { action: false, list: false, note: false });

  const busyEmpty = await renderDetail(taskDetail([], "task"));
  assert.deepEqual(noticeParts(busyEmpty), { action: false, list: false, note: true });
  assert.match(busyEmpty.textContent, /没有找到申请显卡的应用/);
});

test("unreadable refresh readings keep their own phrase", () => {
  const unknown = { key: "reason.exit_status", params: { code: 1 } };
  assert.deepEqual(refreshObserved({ unknown }, { value: 5 }, false), {
    key: "check.observed.status_unreadable",
    params: {},
  });
  assert.deepEqual(refreshObserved({ value: 5 }, { unknown }, false), {
    key: "check.observed.status_unreadable",
    params: {},
  });
  assert.deepEqual(refreshObserved({ unknown }, { unknown }, false), {
    key: "check.observed.status_unreadable",
    params: {},
  });

  assert.deepEqual(refreshObserved({ not_loaded: true }, { value: 5 }, false), {
    key: "check.observed.refresh_before_absent",
    params: {},
  });
  assert.deepEqual(refreshObserved({ value: 5 }, { not_loaded: true }, false), {
    key: "check.observed.refresh_after_absent",
    params: {},
  });

  assert.equal(refreshObserved({ value: 1 }, { value: 2 }, true).key, "check.observed.refresh_both");

  const log = logParams("log.check_item", {
    check_item: "refresh",
    passed: false,
    since_before: { unknown },
    since_after: { value: 5 },
  });
  const text = expandText("log.check_item", log, "zh");
  assert.match(text, /systemd-sysext status 无法读取/);
  assert.equal(text.includes("无 `/usr` 上的 nvidia 扩展"), false);
  const absent = logParams("log.check_item", {
    check_item: "refresh",
    passed: false,
    since_before: { not_loaded: true },
    since_after: { value: 5 },
  });
  assert.match(expandText("log.check_item", absent, "zh"), /刷新前无 \/usr 上的 nvidia 扩展/);
});

test("null, absent and malformed log readings stay safe and never 'absent'", () => {
  // `since_before: null` must not throw, and a reading that could not be read is not an absent item.
  const nullBefore = logParams("log.check_item", {
    check_item: "refresh",
    passed: false,
    since_before: null,
    since_after: { value: 5 },
  });
  const nullText = expandText("log.check_item", nullBefore, "zh");
  assert.match(nullText, /systemd-sysext status 无法读取/);
  assert.equal(nullText.includes("无 `/usr` 上的 nvidia 扩展"), false, nullText);

  // An absent field and malformed shapes are unreadable too, never absent.
  for (const p of [
    { check_item: "refresh", passed: false, since_after: { value: 5 } },
    {
      check_item: "refresh",
      passed: false,
      since_before: { value: "5" },
      since_after: { not_loaded: false },
    },
  ]) {
    const text = expandText(
      "log.check_item",
      logParams("log.check_item", p as Record<string, unknown>),
      "zh",
    );
    assert.match(text, /systemd-sysext status 无法读取/);
    assert.equal(text.includes("无 `/usr` 上的 nvidia 扩展"), false, text);
  }

  const versionText = expandText(
    "log.check_item",
    logParams("log.check_item", {
      check_item: "version",
      passed: false,
      nvidia_smi: null,
      proc_version: undefined,
      expected: "580.178.04",
    }),
    "zh",
  );
  assert.match(versionText, /无法读取/);

  // The genuine shapes keep their own writings (controls): a real unknown stays unreadable and a real
  // `{not_loaded}` stays absent.
  const unknown = logParams("log.check_item", {
    check_item: "refresh",
    passed: false,
    since_before: { unknown: { key: "reason.exit_status", params: { code: 1 } } },
    since_after: { value: 5 },
  });
  assert.match(expandText("log.check_item", unknown, "zh"), /systemd-sysext status 无法读取/);
  const absent = logParams("log.check_item", {
    check_item: "refresh",
    passed: false,
    since_before: { not_loaded: true },
    since_after: { value: 5 },
  });
  assert.match(expandText("log.check_item", absent, "zh"), /刷新前无 \/usr 上的 nvidia 扩展/);
});

test("different valid merge times explain unconfirmed refreshes only for a real failure", () => {
  assert.equal(refreshObserved({ value: 1 }, { value: 2 }, false).key, "check.observed.refresh_unconfirmed");
  assert.equal(refreshObserved({ value: 1 }, { value: 1 }, false).key, "check.observed.refresh_both");
  assert.equal(refreshObserved({ value: 1 }, { value: 2 }, undefined).key, "check.observed.refresh_both");
  for (const value of [NaN, Infinity, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
    assert.equal(refreshObserved({ value }, { value: 2 }, false).key, "check.observed.status_unreadable");
  }
  for (const passed of [undefined, null, "false", 0]) {
    const p = logParams("log.check_item", {
      check_item: "refresh", passed, since_before: { value: 1 }, since_after: { value: 2 },
    });
    assert.deepEqual(p["observed"], refreshObserved({ value: 1 }, { value: 2 }, undefined));
    assert.equal(p["check_result"], "UNKNOWN");
  }
});

test("task checks, failed restore notices and logs explain an unconfirmed refresh in both languages", async () => {
  const before = 1_791_000_000_000_000;
  const after = before + 120_000_000;
  const check = {
    pass: "update", since_before: { value: before }, since_after: { value: after },
    refresh_ok: false, nvidia_smi: { value: "580.178.04" }, proc_version: { value: "580.178.04" },
    expected: "580.178.04", version_ok: true, passed: false,
  };
  for (const lang of ["zh", "en"] as const) {
    for (const restored of [false, true]) {
      const detail = {
        ...taskDetail([], "task"),
        plan: { version: "580.178.04", source: { kind: "official" }, original: "skip" },
        created_at: 1_760_000_000_000,
        finished_at: 1_760_000_060_000,
        interrupt_discovered_at: null,
        checks: restored ? [check, { ...check, pass: "restore" }] : [check],
        restore: restored
          ? { decision: "restore", reason: null, read_reason: null, original: { row_id: "stock", version: "580.173.02", saved_at: 1791000000000 } }
          : { decision: "none", reason: "restore.no_original.not_saved", read_reason: null, original: null },
        error: { code: restored ? "RESTORE_CHECK_FAILED" : "NO_USABLE_ORIGINAL", params: {}, command: null, exit: null, raw: null },
        notice: { key: restored ? "notice.restore_failed" : "notice.no_restore", variant: restored ? "written" : undefined, params: {} },
        log: { lines: [{ t: 1, key: "log.check_item", p: { check_item: "refresh", passed: false, since_before: check.since_before, since_after: check.since_after } }], next: 1, reset: false },
      };
      const container = await renderDetailContainer(detail, lang);
      const phrase = lang === "zh" ? "未确认扩展已卸下并重新合并" : "unmerge and subsequent merge not confirmed";
      const expected = lang === "zh" ? "已确认扩展卸下并重新合并，且合并时间与之前不同" : "a confirmed unmerge and subsequent merge, with a merge time different from before";
      for (const className of ["task-notice", "log-tail"]) {
        const area = allElements(container).find((n) => n.className.split(/\s+/).includes(className));
        assert.ok(area, className);
        assert.ok(area.textContent.includes(phrase), area.textContent);
        assert.ok(area.textContent.includes(expected), area.textContent);
      }
      assert.ok((container.textContent.match(new RegExp(phrase, "g")) ?? []).length >= 3);
      assert.equal(container.textContent.includes("UNKNOWN"), false, container.textContent);
    }
  }
});

test("a start without a merged nvidia extension is explained as that, not as unreadable", async () => {
  // The start reading is a readable status that carries no nvidia merge time: `systemd-sysext status`
  // answered, the extension was simply not merged.
  const check = {
    pass: "update",
    since_before: { unknown: { key: "reason.format_unrecognized", params: {} } },
    since_after: { value: 1_791_000_120_000_000 },
    refresh_ok: false,
    nvidia_smi: { value: "580.178.04" },
    proc_version: { value: "580.178.04" },
    expected: "580.178.04",
    version_ok: true,
    passed: false,
  };
  const base = refreshObserved(check.since_before, check.since_after, check.refresh_ok);
  assert.deepEqual(base, { key: "check.observed.refresh_before_unmerged", params: {} });

  for (const lang of ["zh", "en"] as const) {
    const detail = {
      ...taskDetail([], "task"),
      plan: { version: "580.178.04", source: { kind: "official" }, original: "skip" },
      created_at: 1_760_000_000_000,
      finished_at: 1_760_000_060_000,
      interrupt_discovered_at: null,
      checks: [check],
      restore: { decision: "none", reason: "restore.no_original.not_saved", read_reason: null, original: null },
      error: { code: "NO_USABLE_ORIGINAL", params: {}, command: null, exit: null, raw: null },
      notice: { key: "notice.no_restore", variant: undefined, params: {} },
      log: { lines: [], next: 0, reset: false },
    };
    const container = await renderDetailContainer(detail, lang);
    const phrase = lang === "zh" ? "更新开始时扩展未合并" : "not merged when the update started";
    const retry = lang === "zh" ? "重试即可" : "retry";
    const area = allElements(container).find((n) =>
      n.className.split(/\s+/).includes("task-notice"),
    );
    assert.ok(area, lang);
    assert.ok(area.textContent.includes(phrase), area.textContent);
    assert.ok(area.textContent.includes(retry), area.textContent);
    assert.equal(area.textContent.includes("status` 无法读取"), false, area.textContent);
    assert.equal(area.textContent.includes("unreadable"), false, area.textContent);
  }
});

test("a successful update with a module that did not come back warns and says what to do", async () => {
  for (const lang of ["zh", "en"] as const) {
    const detail = {
      ...taskDetail([], "task"),
      status: "SUCCEEDED",
      phase: "check",
      plan: { version: "580.178.04", source: { kind: "official" }, original: "skip" },
      error: null,
      gpu_users: null,
      notice: {
        key: "notice.update_complete",
        variant: "reload_warning",
        params: { version: "580.178.04", modules: "nvidia_uvm" },
      },
      replace: [
        {
          pass: "update",
          unloaded: ["nvidia_uvm"],
          reloaded: [
            { module: "nvidia_uvm", ok: false, reason: { key: "reason.exit_status", params: { code: 1 } } },
          ],
          readback_ok: true,
        },
      ],
      checks: [],
    };
    const container = await renderDetailContainer(detail, lang);
    const area = allElements(container).find((n) =>
      n.className.split(/\s+/).includes("task-notice"),
    );
    assert.ok(area, lang);
    const title =
      lang === "zh" ? "部分驱动模块未重新加载" : "some driver modules were not reloaded";
    const action = lang === "zh" ? "重启主机" : "Restart the host";
    assert.ok(area.textContent.includes(title), area.textContent);
    assert.ok(area.textContent.includes("nvidia_uvm"), area.textContent);
    assert.ok(area.textContent.includes(action), area.textContent);

    assert.equal(area.textContent.split(lang === "zh" ? "成功检查通过。" : "the post-update check passed.").length - 1, 1, area.textContent);
  }
});

test("sysext stdout and stderr remain literal log text despite malformed payloads", async () => {
  const rawOut = '<script>alert("stdout")</script>\n{"hierarchy":"/usr"}';
  const rawErr = "<img src=x onerror=alert(1)> stderr {target} **raw**";
  const lines = [
    ...["status", "unmerge", "merge"].map((command, i) => ({ t: i + 1, key: "log.cmd", p: {
      argv: ["systemd-sysext", command], exit: { code: 0 }, ms: 1, stdout: rawOut, stderr: rawErr,
    } })),
    { t: 4, key: "log.cmd", p: null },
    { t: 5, key: "log.cmd", p: { argv: ["systemd-sysext", "merge"], stdout: { html: "BAD_OBJECT" }, stderr: ["BAD_ARRAY"] } },
    { t: 6, key: "log.cmd", p: { argv: ["echo", "ready"], exit: { code: 0 }, ms: 1 } },
  ];
  for (const lang of ["zh", "en"] as const) {
    const container = await renderDetailContainer({ ...taskDetail([], "task"), log: { lines, next: 6, reset: false } }, lang);
    const logTail = allElements(container).find((n) => n.className === "log-tail");
    assert.ok(logTail);
    assert.ok(logTail.textContent.includes(rawOut));
    assert.ok(logTail.textContent.includes(rawErr));
    assert.equal(logTail.textContent.includes("BAD_OBJECT"), false);
    assert.equal(logTail.textContent.includes("BAD_ARRAY"), false);
    assert.match(logTail.textContent, /echo ready/);
    assert.equal(logTail.children.every((n) => n instanceof FakeText), true);
    for (const area of allElements(container).filter((n) => ["alert", "status"].includes(n.getAttribute("role") ?? ""))) {
      assert.equal(area.textContent.includes(rawOut), false);
      assert.equal(area.textContent.includes(rawErr), false);
    }
  }
});

test("a null log reading cannot stop the task render", async () => {
  const detail = taskDetail([], "task") as unknown as {
    log: { lines: unknown[]; next: number; reset: boolean };
  };
  detail.log = {
    lines: [
      {
        t: 1,
        key: "log.check_item",
        p: {
          check_item: "refresh",
          passed: false,
          since_before: null,
          since_after: { value: 5 },
        },
      },
    ],
    next: 1,
    reset: false,
  };
  const container = await renderDetailContainer(detail);
  const logTail = allElements(container).find((n) =>
    n.className.split(/\s+/).includes("log-tail"),
  );
  assert.ok(logTail, "the log tail was rendered");
  assert.match(logTail.textContent, /systemd-sysext status 无法读取/);
});

test("a non-object log payload renders UNKNOWN and does not stop later lines", async () => {
  for (const payload of [null, [], ["unexpected"], 0, false, "unexpected"]) {
    const params = logParams("log.check_item", payload);
    assert.deepEqual(params, {});
    assert.match(expandText("log.check_item", params, "zh"), /UNKNOWN/);
  }

  const detail = taskDetail([], "task") as unknown as {
    log: { lines: unknown[]; next: number; reset: boolean };
  };
  detail.log = {
    lines: [
      { t: 1, key: "log.check_item", p: null },
      {
        t: 2,
        key: "log.check_item",
        p: {
          check_item: "refresh",
          passed: true,
          since_before: { value: 1 },
          since_after: { value: 2 },
        },
      },
    ],
    next: 2,
    reset: false,
  };
  const container = await renderDetailContainer(detail);
  const logTail = allElements(container).find((n) =>
    n.className.split(/\s+/).includes("log-tail"),
  );
  assert.ok(logTail, "the log tail was rendered");
  assert.match(logTail.textContent, /UNKNOWN/);
  assert.match(logTail.textContent, /驱动扩展刷新：通过/);
});

test("malformed fields inside object log payloads do not stop later lines", async () => {
  const malformed = [
    { key: "log.cmd", p: { argv: "unexpected", exit: { code: 0 }, ms: 1 } },
    { key: "log.gpu_users", p: { applications: [null], errors: [null] } },
    { key: "log.restore_none", p: { reason: { key: "restore.no_original.unreadable", params: null } } },
    { key: "log.artifact_check_failed", p: { build_reason: { key: "build.compiler_missing", params: null } } },
  ];
  for (const line of malformed) {
    assert.match(expandText(line.key, logParams(line.key, line.p), "zh"), /UNKNOWN/);
  }

  const detail = taskDetail([], "task") as unknown as {
    log: { lines: unknown[]; next: number; reset: boolean };
  };
  detail.log = {
    lines: [
      ...malformed.map((line, i) => ({ t: i + 1, ...line })),
      { t: 5, key: "log.occupancy", p: { usage_result: "busy", module_readings: [{ key: "reason.timed_out", params: null }] } },
      { t: 6, key: "log.cmd", p: { argv: ["echo", "ready"], exit: { code: 0 }, ms: 1 } },
    ],
    next: 6,
    reset: false,
  };
  const container = await renderDetailContainer(detail);
  const logTail = allElements(container).find((n) =>
    n.className.split(/\s+/).includes("log-tail"),
  );
  assert.ok(logTail, "the log tail was rendered");
  assert.match(logTail.textContent, /UNKNOWN/);
  assert.match(logTail.textContent, /echo ready/);
});

test("invalid log fields remain UNKNOWN instead of inventing an exit or result", () => {
  for (const exit of [undefined, null, {}, { timed_out: "yes" }, { code: "0" }, { code: 0, signal: 15 }]) {
    const text = expandText(
      "log.cmd",
      logParams("log.cmd", { argv: ["echo", "ready"], exit, ms: 1 }),
      "en",
    );
    assert.match(text, /UNKNOWN/);
    assert.equal(text.includes("could not be started"), false);
    assert.equal(text.includes("timed out"), false);
  }
  assert.match(expandText("log.cmd", logParams("log.cmd", { argv: [], exit: { spawn_failed: true } }), "en"), /could not be started/);
  assert.match(expandText("log.cmd", logParams("log.cmd", { argv: [], exit: { signal: 15 } }), "en"), /killed by signal 15/);

  for (const p of [{}, { check_item: "bogus", passed: "false" }, { check_item: "refresh" }]) {
    const text = expandText("log.check_item", logParams("log.check_item", p), "zh");
    assert.match(text, /UNKNOWN/);
    assert.equal(text.includes("失败"), false, text);
    assert.equal(text.includes("未通过"), false, text);
  }
  const valid = expandText(
    "log.check_item",
    logParams("log.check_item", { check_item: "refresh", passed: false, since_before: { value: 1 }, since_after: { value: 2 } }),
    "zh",
  );
  assert.match(valid, /驱动扩展刷新：未通过/);
});

test("the readback log line keeps match, mismatch and an unknown hash apart", () => {
  const readback = (p: unknown): string =>
    expandText("log.readback", logParams("log.readback", p), "zh");
  assert.match(readback({ readback_result: true }), /与写入的一致/);
  assert.match(readback({ readback_result: false }), /与写入的不一致/);
  // The runner stores a failed hash as null: it must not read as a mismatch, and a raw boolean must
  // never reach the line.
  for (const p of [{ readback_result: null }, {}, { readback_result: "false" }]) {
    const text = readback(p);
    assert.match(text, /UNKNOWN/);
    assert.equal(text.includes("不一致"), false, text);
  }
  assert.equal(readback({ readback_result: true }).includes("true"), false);
  assert.equal(readback({ readback_result: false }).includes("false"), false);

  assert.match(
    expandText("log.readback", logParams("log.readback", { readback_result: false }), "en"),
    /does not match what was written/,
  );
  assert.match(
    expandText("log.readback", logParams("log.readback", { readback_result: null }), "en"),
    /UNKNOWN/,
  );
});

test("GPU application kinds use only the Docker and Incus keys", () => {
  const text = expandText(
    "log.gpu_users",
    logParams("log.gpu_users", {
      applications: [{ name: "one", kind: "bogus" }, { name: "two", kind: "docker" }],
      errors: [{ kind: "bogus" }, { kind: "incus" }],
    }),
    "zh",
  );
  assert.match(text, /one · UNKNOWN/);
  assert.match(text, /two · Docker 应用/);
  assert.match(text, /Incus 实例 查询失败/);
  assert.equal(text.includes("query.object.bogus"), false);
  assert.match(expandText("log.gpu_users", logParams("log.gpu_users", {}), "zh"), /UNKNOWN/);
});

test("unrecognized log domains do not select another message", () => {
  const fields = [
    ["log.phase_enter", { phase: "bogus" }],
    ["log.finished", { status: "bogus" }],
    ["log.build_step", { build_step: "bogus" }],
    ["log.build_install_mode", { mode: "bogus" }],
    ["log.occupancy", { usage_result: "bogus" }],
    ["log.loaded_version_unknown", { read_reason: "status.SUCCEEDED" }],
    ["log.restore_none", { reason: "status.SUCCEEDED" }],
    ["log.artifact_check_failed", { build_reason: "status.SUCCEEDED" }],
    ["log.cleanup_failed", { cleanup_step: "delete_temp_file", reason: { key: "status.SUCCEEDED", params: {} } }],
    ["log.build_kit_cache", { reason: { key: "status.SUCCEEDED", params: {} } }],
  ] as const;
  for (const [key, payload] of fields) {
    const text = expandText(key, logParams(key, payload), "en");
    assert.match(text, /UNKNOWN/, `${key}: ${text}`);
    assert.equal(text.includes("Succeeded"), false, `${key}: ${text}`);
  }
  const upload = logParams("log.row_created", { version: "580.178.04", source: "upload" });
  assert.match(expandText("log.row_created", upload, "en"), /fingerprint UNKNOWN/);
});

test("unknown top-level task log keys show UNKNOWN and leave later lines visible", async () => {
  const detail = taskDetail([], "task") as unknown as {
    log: { lines: unknown[]; next: number; reset: boolean };
  };
  detail.log = {
    lines: [
      { t: 1, key: "log.internal_unrecognized", p: {} },
      { t: 2, key: "status.SUCCEEDED", p: {} },
      { t: 3, key: "log.cmd", p: { argv: ["echo", "ready"], exit: { code: 0 }, ms: 1 } },
    ],
    next: 3,
    reset: false,
  };
  const container = await renderDetailContainer(detail);
  const logTail = allElements(container).find((n) =>
    n.className.split(/\s+/).includes("log-tail"),
  );
  assert.ok(logTail, "the log tail was rendered");
  assert.equal((logTail.textContent.match(/UNKNOWN/g) ?? []).length, 2);
  assert.equal(logTail.textContent.includes("log.internal_unrecognized"), false);
  assert.equal(logTail.textContent.includes("成功"), false);
  assert.match(logTail.textContent, /echo ready/);
});

test("the build script's two-pass install mode uses the log phrase", async () => {
  const mode = logParams("log.build_install_mode", { mode: "two-pass" });
  assert.deepEqual(mode["install_mode"], { key: "log.install_mode.two_pass", params: {} });
  assert.equal(
    expandText("log.build_install_mode", mode, "zh"),
    "安装方式：两遍（先用户态，后内核模块）。",
  );
  assert.equal(
    expandText("log.build_install_mode", mode, "en"),
    "Installation: two passes (user space first, then kernel modules).",
  );
  assert.deepEqual(logParams("log.build_install_mode", { mode: "single" })["install_mode"], {
    key: "log.install_mode.single",
    params: {},
  });
  assert.equal(logParams("log.build_install_mode", { mode: "bogus" })["install_mode"], "UNKNOWN");

  const detail = taskDetail([], "task") as unknown as {
    log: { lines: unknown[]; next: number; reset: boolean };
  };
  detail.log = {
    lines: [{ t: 1, key: "log.build_install_mode", p: { mode: "two-pass" } }],
    next: 1,
    reset: false,
  };
  const container = await renderDetailContainer(detail);
  const logTail = allElements(container).find((n) =>
    n.className.split(/\s+/).includes("log-tail"),
  );
  assert.ok(logTail);
  assert.match(logTail.textContent, /安装方式：两遍（先用户态，后内核模块）/);
  assert.equal(logTail.textContent.includes("UNKNOWN"), false);
});


test("recovery release is available only for terminal tasks with retained material", async () => {
  for (const lang of ["zh", "en"] as const) {
    for (const status of ["SUCCEEDED", "STOCK_RESTORED", "FAILED", "INTERRUPTED", "RUNNING", "UNKNOWN"]) {
      for (const kept of [true, false, undefined]) {
        const container = await renderDetailContainer({
          ...taskDetail([], "task"), status, recovery_material_kept: kept,
        }, lang);
        const buttons = allElements(container).filter((node) => node.tagName === "BUTTON"
          && node.textContent === expandText("task.release.button", {}, lang));
        assert.equal(buttons.length, kept === true && !["RUNNING", "UNKNOWN"].includes(status) ? 1 : 0);
      }
    }
  }
});

test("released recovery history uses the releasing task and Unix-millisecond timestamp", async () => {
  for (const lang of ["zh", "en"] as const) {
    const container = await renderDetailContainer({
      ...taskDetail([], "task"), recovery_material_kept: false,
      released_by: "later-task", released_at: 1792209600000,
    }, lang);
    assert.match(container.textContent, /later-task/);
    assert.match(container.textContent, /2026-10-17/);
    const area = allElements(container).find((node) => node.className === "task-recovery");
    assert.ok(area);
    assert.equal(area.textContent.includes("UNKNOWN"), false);
    assert.equal(allElements(container).some((node) => node.tagName === "BUTTON"), false);
  }
});

test("a recorded release with retained material offers a cleanup retry", async () => {
  const container = await renderDetailContainer({
    ...taskDetail([], "task"), recovery_material_kept: true,
    released_by: "later-task", released_at: 1792209600000,
  }, "en");
  assert.match(container.textContent, /directory cleanup is still pending/);
  assert.ok(allElements(container).some((node) => node.tagName === "BUTTON"));
});

test("occupancy wait copy is a Chinese and English pair", () => {
  assert.equal(
    expandText("gpu.wait.headline", {}, "zh"),
    "显卡正被使用。停止标为「正在使用显卡」的应用后，更新会自动继续。",
  );
  assert.equal(
    expandText("gpu.wait.headline", {}, "en"),
    "The GPU is in use. Once you stop the apps marked \"Using the GPU now\", the update continues automatically.",
  );
  for (const [phase, key] of [
    ["unload", "task.waiting_gpu"],
    ["align_modules", "task.waiting_gpu.align"],
    ["restore_unload", "task.waiting_gpu.restore"],
    ["restore_align_modules", "task.waiting_gpu.restore"],
  ] as const) {
    assert.equal(waitingGpuMessageKey({ status: "RUNNING", phase, waiting_gpu: true }), key);
    assert.equal(expandText(key, {}, "zh"), MESSAGES[key]?.zh);
    assert.equal(expandText(key, {}, "en"), MESSAGES[key]?.en);
  }
  assert.equal(waitingGpuMessageKey({ status: "RUNNING", phase: "unload" }), null);
  assert.equal(
    waitingGpuMessageKey({ status: "FAILED", phase: "unload", waiting_gpu: true }),
    null,
  );

  const released = logParams("log.occupancy_wait", { waited_ms: 1000, result: "released" });
  assert.equal(
    expandText("log.occupancy_wait", released, "zh"),
    "显卡正被使用，等待 1000 毫秒后，结果：已空闲。",
  );
  assert.equal(
    expandText("log.occupancy_wait", released, "en"),
    "The GPU was in use. After waiting 1000 ms, result: released.",
  );
  const held = logParams("log.occupancy_wait", { waited_ms: 30000, result: "still_in_use" });
  assert.match(expandText("log.occupancy_wait", held, "zh"), /仍被使用/);
  assert.match(expandText("log.occupancy_wait", held, "en"), /still in use/);

  const immediate = expand(
    "notice.unload_refused",
    { module: "nvidia", unload_reason: "in use", old: "580.173.02" },
    "en",
  );
  assert.equal(/waited about 30 seconds/.test(seg(immediate, "body")), false);
  const waited = expand(
    "notice.unload_refused",
    { module: "nvidia", unload_reason: "in use", old: "580.173.02", waited_in_use: true },
    "zh",
  );
  assert.match(seg(waited, "body"), /等待约 30 秒后仍无法卸载驱动模块 nvidia/);

  const busy = expand("notice.restore_stopped", { current: "580.173.02" }, "en", "busy");
  assert.match(seg(busy, "body"), /the GPU stayed in use/);
  assert.match(seg(busy, "body"), /permanent path/);
  const busyEmpty = expand(
    "notice.restore_stopped",
    { current: "580.173.02", empty_list: true },
    "zh",
    "busy",
  );
  assert.match(seg(busyEmpty, "action"), /如有应用在反复重启，请先停止它/);
  assert.match(seg(expand("notice.gpu_in_use", {}, "zh"), "body"), /已等待约 30 秒，驱动文件未改动/);
  assert.match(seg(expand("notice.gpu_in_use.empty_note", {}, "en"), "note"), /No app requesting the GPU was found/);
});

test("a running wait shows one status line and does not rewrite it while it stays", async () => {
  let fire: (() => void) | null = null;
  let fireGpu: (() => void) | null = null;
  let gpuStopped = false;
  const previousWindow = (globalThis as unknown as { window: unknown }).window;
  (globalThis as unknown as { window: unknown }).window = {
    setInterval: (fn: () => void, ms: number): number => {
      if (ms === 2000) { fireGpu = fn; return 2; }
      fire = fn;
      return 1;
    },
    clearInterval: (id: number): void => { if (id === 2) gpuStopped = true; },
  };
  const detail = {
    ...taskDetail([], "task"),
    status: "RUNNING",
    phase: "align_modules",
    waiting_gpu: true,
  };
  (globalThis as unknown as { fetch: unknown }).fetch = async (url: string): Promise<unknown> => ({
    ok: true,
    status: 200,
    json: async (): Promise<unknown> => url === "/api/gpu-users"
      ? { state: "busy", users: [{name: "worker", type: "docker", usage: "in_use"}], errors: [] } : detail,
  });
  const container = new FakeElement("div");
  renderTask(
    { ...fakeApp, lang: "en" } as never,
    container as unknown as HTMLElement,
    "audit-task",
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
  const status = allElements(container).find((node) => node.getAttribute("role") === "status");
  assert.ok(status);
  const wait = allElements(status).find((node) => node.className.split(/\s+/).includes("gpu-list-headline"));
  assert.ok(wait);
  assert.equal(wait.getAttribute("hidden"), null);
  assert.equal(wait.textContent, MESSAGES["task.waiting_gpu.align"]?.en);
  const users = allElements(status).find((node) => node.className === "gpu-users");
  assert.ok(users);
  assert.match(users.textContent, /Using the GPU now.*worker.*Docker app/);
  const children = [...users.children];
  const descriptor = Object.getOwnPropertyDescriptor(FakeElement.prototype, "textContent");
  assert.ok(descriptor?.set && descriptor.get);
  let writes = 0;
  Object.defineProperty(wait, "textContent", {
    configurable: true,
    get: () => descriptor.get?.call(wait),
    set: (value: string) => {
      writes += 1;
      descriptor.set?.call(wait, value);
    },
  });
  const tick = fire as (() => void) | null;
  if (tick === null) throw new Error("interval was not armed");
  tick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(writes, 0);
  const gpuTick = fireGpu as (() => void) | null;
  gpuTick?.();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(users.children, children);
  detail.waiting_gpu = false;
  tick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(users.children.length, 0);
  assert.equal(gpuStopped, true);
  (globalThis as unknown as { window: unknown }).window = previousWindow;
});

test("the update check status appears after about 1 second and leaves when the request ends", () => {
  const timers = new Map<number, () => void>();
  let next = 1;
  const previousWindow = (globalThis as unknown as { window: Window }).window;
  (globalThis as unknown as { window: unknown }).window = {
    setTimeout: (fn: () => void, ms?: number): number => {
      assert.equal(ms, 1000);
      const id = next;
      next += 1;
      timers.set(id, fn);
      return id;
    },
    clearTimeout: (id?: number): void => {
      if (id !== undefined) timers.delete(id);
    },
    setInterval: (): number => 1,
    clearInterval: (): void => {},
  };
  const parent = new FakeElement("div");
  const button = new FakeElement("button");
  const after = new FakeElement("span");
  parent.append(button, after);
  const stop = armUpdateCheckStatus(button as unknown as HTMLElement, "zh");
  assert.equal(parent.children.length, 2);
  timers.get(1)?.();
  assert.equal(parent.children.length, 3);
  const status = parent.children[1] as FakeElement;
  assert.equal(status.getAttribute("role"), "status");
  assert.equal(status.textContent, MESSAGES["gpu.list.loading"]?.zh);
  assert.equal(parent.children[2], after);
  stop();
  assert.equal(parent.children.length, 2);

  const parentEarly = new FakeElement("div");
  const earlyButton = new FakeElement("button");
  parentEarly.append(earlyButton);
  const stopEarly = armUpdateCheckStatus(earlyButton as unknown as HTMLElement, "en");
  stopEarly();
  assert.equal(timers.has(2), false);
  assert.equal(parentEarly.children.length, 1);
  (globalThis as unknown as { window: unknown }).window = previousWindow;
});

test("a terminal snapshot renders each category error once and keeps raw output outside alerts", async () => {
  const detail = taskDetail([{name:"worker",type:"docker"}], "task");
  const container = await renderDetailContainer({ ...detail, gpu_users: {checked_at:1792209600000, users:[{id:"docker:a",name:"worker",type:"docker",usage:"in_use"}], errors:[{type:"incus",reason:{key:"reason.permission_denied"},raw:"RAW_SNAPSHOT_COMMAND"}] } }, "en");
  const notice = allElements(container).find((node) => node.className === "task-notice"); assert.ok(notice);
  assert.equal(notice.textContent.split("permission").length - 1, 1);
  assert.ok(notice.textContent.includes("List taken at"));
  assert.ok(notice.textContent.includes("RAW_SNAPSHOT_COMMAND"));
  assert.equal(allElements(notice).filter((node) => ["alert","status"].includes(node.getAttribute("role") ?? "")).some((node) => node.textContent.includes("RAW_SNAPSHOT_COMMAND")), false);
});
