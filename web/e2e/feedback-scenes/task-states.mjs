import { launch, open, shot, sleep } from "./lib.mjs";
const b = await launch();
const NOW = 1792209600000;
const U = [
  { id: "docker:a", name: "gpu-worker", type: "docker", usage: "in_use" },
  { id: "docker:b", name: "media-idle", type: "docker", usage: "requested" },
  { id: "container:1", name: "compute-instance", type: "incus", usage: "unknown" },
];
const base = () => ({
  task_id: "fake-task", status: "RUNNING", phase: "unload", created_at: NOW - 60000, finished_at: null, interrupt_discovered_at: null,
  recovery_material_kept: false, state_read_error: null, state_note: null, unit_query: null,
  plan: { version: "580.178.04", source: { kind: "official" }, original: "skip", kernel: "6.12.105-production+truenas" },
  loaded_version_start: { value: "580.173.02" }, loaded_version_end: null, download: null, replace: [], checks: [], restore: null,
  error: null, gpu_users: null, version_change: false, notice: null, log: { lines: [], next: 0, reset: false },
});
async function task(name, detail, { users = null, file } = {}) {
  const { page, ctx } = await open(b, "fresh", "/");
  await page.route("**/api/tasks/fake-task*", (r) => r.fulfill({ json: detail }));
  if (users) await page.route("**/api/gpu-users", (r) => r.fulfill({ json: { state: "busy", checked_at: NOW, errors: [], users } }));
  await page.goto("/#/tasks/fake-task");
  await page.locator(".task-state, h2").first().waitFor(); await sleep(1500);
  await shot(page, file ?? name, { fullPage: true });
  console.log(name, "::", (await page.locator("main").innerText()).replace(/\n+/g, " | ").slice(0, 700));
  await ctx.close();
}
await task("task-waiting-gpu", { ...base(), waiting_gpu: true }, { users: U.slice(0, 2) });
await task("task-waiting-gpu-align", { ...base(), phase: "align_modules", waiting_gpu: true }, { users: U.slice(0, 2) });
await task("task-waiting-gpu-restore", { ...base(), phase: "restore_unload", waiting_gpu: true, restore: { decision: "restore", reason: "check_failed", read_reason: null, original: { row_id: "x", version: "580.173.02", saved_at: NOW - 86400000 } } }, { users: U.slice(0, 2) });
const failed = { ...base(), status: "FAILED", finished_at: NOW, error: { code: "GPU_IN_USE", params: { occupancy: "busy" }, command: null, exit: null, raw: null },
  gpu_users: { checked_at: NOW, users: U, errors: [] }, notice: { key: "notice.gpu_in_use", variant: "task", params: {} } };
await task("task-stopped-notice", failed);
await task("task-stopped-notice-restore", { ...base(), status: "FAILED", phase: "restore_unload", finished_at: NOW,
  loaded_version_end: { value: "580.178.04" },
  checks: [{ pass: "update", since_before: { value: 1 }, since_after: { value: 2 }, refresh_ok: true, nvidia_smi: { value: "580.173.02" }, proc_version: { value: "580.173.02" }, expected: "580.178.04", version_ok: false, passed: false }],
  restore: { decision: "restore", reason: "check_failed", read_reason: null, original: { row_id: "x", version: "580.173.02", saved_at: NOW - 86400000 } },
  error: { code: "GPU_IN_USE", params: { occupancy: "busy" }, command: null, exit: null, raw: null },
  gpu_users: { checked_at: NOW, users: U, errors: [] }, notice: { key: "notice.restore_stopped", variant: "busy", params: {} } });
await task("task-download-progress", { ...base(), phase: "download", download: { done_bytes: 350_000_000, total_bytes: 400_000_000 } }, { file: "task-download-progress" });
await task("task-download-progress2", { ...base(), phase: "download", download: { done_bytes: 980_000, total_bytes: 1_200_000 } }, { file: "task-download-progress-2" });
await b.close();
