import { launch, open, shot, blur, sleep, CHECK } from "./lib.mjs";
const b = await launch();
// Hold the plan response so the waiting panel stays up; control /api/gpu-users answers.
async function setup(page, { delayFirst = 0 } = {}) {
  const st = { mode: "real", users: null, calls: 0, release: null };
  const held = new Promise((r) => { st.release = r; });
  await page.route("**/api/plans", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    try { const resp = await route.fetch(); await held; await route.fulfill({ response: resp }); } catch {}
  });
  await page.route("**/api/gpu-users", async (route) => {
    st.calls++;
    if (st.calls === 1 && delayFirst) await sleep(delayFirst);
    if (st.mode === "real") return route.continue();
    await route.fulfill({ json: { checked_at: 1792209600000 + st.calls * 2000, errors: [], ...st.mode } });
  });
  return st;
}
{ // update-waiting-panel
  const { page, ctx } = await open(b, "gpu_wait", "/update");
  await page.locator("#version-select").waitFor();
  const st = await setup(page, { delayFirst: 1500 });
  await page.selectOption("#version-select", "580.178.04"); await blur(page);
  await page.getByRole("button", { name: CHECK }).click();
  await page.locator(".update-check-status").waitFor();
  await sleep(150);
  await shot(page, "update-waiting-panel-1-loading", { fullPage: true });
  await page.locator(".update-check-status .gpu-user").first().waitFor({ timeout: 6000 });
  await sleep(300);
  await shot(page, "update-waiting-panel-2-list", { fullPage: true });
  await shot(page, "update-waiting-panel", { fullPage: true });
  // gpu-list-states
  const U = (id, name, type, usage) => ({ id, name, type, usage });
  const seq = [
    ["gpu-list-states-a-unknown-incus", { state: "busy", users: [U("docker:u", "gpu-worker", "docker", "unknown"), U("container:1", "compute-instance", "incus", "in_use")] }, 6500],
    ["gpu-list-states-b-stopped", { state: "busy", users: [U("container:1", "compute-instance", "incus", "in_use")] }, 2400],
    ["gpu-list-states-c-idle", { state: "idle", users: [] }, 6500],
    ["gpu-list-states-d-unreadable", { state: "unreadable", users: [], read_reason: { key: "reason.permission_denied", params: {} } }, 2400],
    ["gpu-list-states-e-busy-empty", { state: "busy", users: [] }, 2400],
  ];
  for (const [id, mode, wait] of seq) { st.mode = mode; await sleep(wait); await shot(page, id, { fullPage: true }); }
  st.release(); await ctx.close();
}
await b.close();
