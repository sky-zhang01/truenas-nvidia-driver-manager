// Build the release fake_host_server before running Playwright: password hashing and upload hashing
// in debug builds can exhaust the browser timeout. Use NVM_E2E_DATA_DIR to place the process temp
// directory; CI must start a fresh server for the current code.
import { defineConfig, devices } from "@playwright/test";

const PORT = 18109;
const DATA_DIR = process.env["NVM_E2E_DATA_DIR"] ?? "/tmp/nvm-e2e-data";
const SERVER_LOG = process.env["NVM_E2E_SERVER_LOG"] ?? "/tmp/nvm-e2e-server.log";
const IN_CI = process.env["CI"] !== undefined || process.env["GITEA_ACTIONS"] !== undefined;

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  expect: { timeout: 7_000 },
  // The fake host owns one mutable scenario; spec files cannot run against it concurrently.
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    headless: true,
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${PORT}` },
    },
  ],
  webServer: {
    command:
      `cargo run --locked --release --example fake_host_server -- --port ${PORT} --data-dir ${DATA_DIR}` +
      (SERVER_LOG ? ` 2>&1 | tee "${SERVER_LOG}"` : ""),
    cwd: "..",
    url: `http://127.0.0.1:${PORT}/api/login`,
    reuseExistingServer: !IN_CI,
    timeout: 120_000,
    // TMPDIR controls all fake-host storage. Preserve the remaining process environment.
    env: { ...process.env, TMPDIR: DATA_DIR },
  },
});
