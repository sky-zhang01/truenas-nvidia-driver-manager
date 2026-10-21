import { launch, open, shot, sleep, openUpload, CHECK } from "./lib.mjs";
const b = await launch();
const NOW = 1792209600000;
const U = [
  { id: "docker:a", name: "gpu-worker", type: "docker", usage: "in_use" },
  { id: "docker:b", name: "media-idle", type: "docker", usage: "requested" },
  { id: "container:1", name: "compute-instance", type: "incus", usage: "unknown" },
];
// 1. upload dialog, check running longer than 1 s
{
  const { page, ctx } = await open(b, "upload_stage", "/update");
  await page.locator("#version-select").waitFor();
  const d = await openUpload(page);
  await page.route("**/api/gpu-users", (r) => r.fulfill({ json: { state: "busy", checked_at: NOW, errors: [], users: U } }));
  let rel; const gate = new Promise((r) => (rel = r));
  await page.route("**/api/plans", async (route) => { if (route.request().method() !== "POST") return route.continue(); await gate; try { await route.fulfill({ json: { result: "blocked", failures: [], gpu: { state: "busy", checked_at: NOW, errors: [], users: U } } }); } catch {} });
  await d.getByRole("button", { name: CHECK }).click(); await sleep(2500);
  await shot(page, "upload-dialog-check-slow");
  await page.setViewportSize({ width: 390, height: 900 });
  await d.locator(".update-check-status").scrollIntoViewIfNeeded();
  await shot(page, "upload-check-waiting-mobile-390");
  console.log("SLOW", (await d.innerText()).replace(/\n+/g, " | ").slice(-700));
  console.log("DISABLED", await d.locator(".upload-footer button").evaluateAll((ns) => ns.map((n) => [n.textContent, n.disabled, n.getAttribute("aria-disabled")])), await d.locator(".upload-dismiss").evaluate((n) => n.disabled));
  rel(); await ctx.close();
}
// 2. unreadable popup title
{
  const { page, ctx } = await open(b, "readme_update", "/update");
  await page.locator("#version-select").waitFor();
  await page.route("**/api/plans", (r) => r.request().method() === "POST" ? r.fulfill({ json: { result: "blocked", failures: [], gpu: { state: "unreadable", checked_at: NOW, errors: [], users: [], read_reason: { key: "reason.io_error", params: {} } } } }) : r.continue());
  await page.selectOption("#version-select", "580.178.04");
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator("dialog[open]").waitFor(); await sleep(600);
  await shot(page, "gpu-blocked-dialog-unreadable");
  console.log("UNREAD", (await page.locator("dialog[open]").innerText()).replace(/\n+/g, " | "));
  await ctx.close();
}
// 3. version changed notice
{
  const { page, ctx } = await open(b, "fresh", "/");
  const detail = {
    task_id: "fake-task", status: "SUCCEEDED", phase: "read_loaded_version_end", created_at: NOW - 60000, finished_at: NOW, interrupt_discovered_at: null,
    recovery_material_kept: false, state_read_error: null, state_note: null, unit_query: null,
    plan: { version: "580.178.04", source: { kind: "official" }, original: "skip", kernel: "6.12.105-production+truenas" },
    loaded_version_start: { value: "580.173.02" }, loaded_version_end: { value: "580.178.04" }, download: null, replace: [], checks: [], restore: null,
    error: null, gpu_users: { checked_at: NOW, users: U, errors: [] }, version_change: true, notice: null, log: { lines: [], next: 0, reset: false },
  };
  await page.route("**/api/tasks/fake-task*", (r) => r.fulfill({ json: detail }));
  await page.goto("/#/tasks/fake-task");
  await page.locator(".task-state, h2").first().waitFor(); await sleep(1500);
  await shot(page, "task-version-changed", { fullPage: true });
  console.log("VC", (await page.locator("main").innerText()).replace(/\n+/g, " | ").slice(0, 900));
  await ctx.close();
}
await b.close();
