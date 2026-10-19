import { test, expect, type APIRequestContext, type Page } from "@playwright/test";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

import { BASE, setScenario, signIn } from "./helpers";

const run = promisify(execFile);
const ORIGIN = { Origin: BASE };
const KEYGEN_TIMEOUT_MS = 20_000;
let signCounter = 0;

type KeyType = "ed25519" | "ecdsa-p256" | "ecdsa-p384";

/** The public-line algorithm prefix of one key type. */
const KEY_PREFIX: Record<KeyType, string> = {
  ed25519: "ssh-ed25519",
  "ecdsa-p256": "ecdsa-sha2-nistp256",
  "ecdsa-p384": "ecdsa-sha2-nistp384",
};

interface FakeState {
  scenario: string;
  auth_epoch: number;
  limiter: { tokens: number | null; consecutive_failures: number };
  logged_out: number;
  ssh_challenges: number;
}

async function fakeState(request: APIRequestContext): Promise<FakeState> {
  const res = await request.get(`${BASE}/__fake/state`);
  expect(res.ok()).toBeTruthy();
  return (await res.json()) as FakeState;
}

/** Select a scenario and prove the isolation really happened: the auth epoch moved and the previously-used limiter, logged-out table and challenge table came back empty. */
async function resetScenario(
  request: APIRequestContext,
  name: string,
): Promise<FakeState> {
  const before = await fakeState(request);
  await setScenario(request, name);
  const after = await fakeState(request);
  expect(typeof after.auth_epoch).toBe("number");
  expect(typeof after.logged_out).toBe("number");
  expect(typeof after.ssh_challenges).toBe("number");
  expect(after.auth_epoch, "the AuthState was rebuilt").toBeGreaterThan(
    before.auth_epoch,
  );
  expect(after.logged_out, "no logged-out cookie survives the rebuild").toBe(0);
  expect(after.ssh_challenges, "no live challenge survives the rebuild").toBe(0);
  expect(
    after.limiter.consecutive_failures,
    "the fresh limiter has no failures",
  ).toBe(0);
  return after;
}

/** A throwaway key of one of the three accepted algorithms. */
async function newKey(label: string, type: KeyType): Promise<{
  dir: string;
  file: string;
  pub: string;
}> {
  const dir = await mkdtemp(path.join(tmpdir(), `nvm-e2e-ssh-${label}-`));
  const file = path.join(dir, "key");
  const args =
    type === "ed25519"
      ? ["-t", "ed25519", "-N", "", "-f", file, "-q"]
      : ["-t", "ecdsa", "-b", type === "ecdsa-p256" ? "256" : "384", "-N", "", "-f", file, "-q"];
  await run("ssh-keygen", args, { timeout: KEYGEN_TIMEOUT_MS });
  const pub = (await readFile(`${file}.pub`, "utf8")).trim();
  expect(pub.startsWith(`${KEY_PREFIX[type]} `), `a real ${type} public line`).toBe(true);
  return { dir, file, pub };
}

/** Sign the exact challenge bytes with the real ssh-keygen (SSHSIG, namespace nvm-login) and return the armored PEM. */
async function signChallenge(file: string, challenge: string): Promise<string> {
  const dir = path.dirname(file);
  // A unique message file per signature: ssh-keygen refuses to overwrite an existing.sig, and one key
  // signs several challenges in a test.
  const message = path.join(dir, `challenge-${Date.now()}-${signCounter++}.txt`);
  // No trailing newline: the signed bytes are the 64 ASCII characters.
  await writeFile(message, challenge, "utf8");
  try {
    await run("ssh-keygen", ["-Y", "sign", "-f", file, "-n", "nvm-login", message], {
      timeout: KEYGEN_TIMEOUT_MS,
    });
  } catch (err) {
    const withOutput = err as Error & { stdout?: string; stderr?: string };
    console.error("SIGNFAIL stdout:", withOutput.stdout);
    console.error("SIGNFAIL stderr:", withOutput.stderr);
    throw err;
  }
  return readFile(`${message}.sig`, "utf8");
}

async function signInChanged(page: Page): Promise<void> {
  await page.goto("/");
  await signIn(page, { password: "longpass-99" });
}

async function registerKey(page: Page, pub: string, comment: string): Promise<void> {
  await page.goto("/#/password");
  await page.fill("#ssh-add-key", pub);
  await page.fill("#ssh-add-comment", comment);
  await page.click(".ssh-keys-add button[type=submit]");
}

/** Open the SSH block and wait for the auto-fetched challenge. */
async function openSshBlock(page: Page): Promise<string> {
  await page.goto("/");
  const block = page.locator("details.login-ssh");
  await expect(block).toBeVisible();
  await block.locator("summary").click();
  const challenge = page.locator("#ssh-challenge");
  await expect
    .poll(async () => (await challenge.textContent())?.trim() ?? "")
    .toMatch(/^[0-9a-f]{64}$/);
  return (await challenge.textContent())?.trim() ?? "";
}

async function sshSignIn(page: Page, key: { file: string }): Promise<void> {
  const challenge = await openSshBlock(page);
  const signature = await signChallenge(key.file, challenge);
  await page.fill("#ssh-signature", signature);
  await page.click(".ssh-login-form button[type=submit]");
  await expect(page).toHaveURL(/#\/home$/);
  await expect(page.locator("#login-user")).toHaveCount(0);
}

/** The delete confirm keeps the session facts. Only the last key is told that the sign-in page will stop offering SSH. */
async function expectDeleteCopy(page: Page, last: boolean): Promise<void> {
  const dialog = page.locator("dialog.nvm-dialog");
  await expect(dialog).toBeVisible();
  const text = ((await dialog.innerText()) ?? "").replace(/\s+/g, " ");
  expect(text).toMatch(/这个浏览器保持登录|This browser stays signed in/);
  expect(text).toMatch(/其他浏览器会退出登录|Other browsers sign out/);
  await expect(dialog.locator("code")).not.toHaveText("");
  const warning = /登录页不再提供 SSH 登录|no longer offers SSH sign-in/;
  if (last) {
    expect(text).toMatch(/最后一把已登记的 SSH 公钥|last registered SSH public key/);
    expect(text).toMatch(warning);
    expect(text.search(warning)).toBeLessThan(
      text.search(/这个浏览器保持登录|This browser stays signed in/),
    );
  } else {
    expect(text).not.toMatch(/最后一把|last registered SSH public key/);
    expect(text).not.toMatch(warning);
  }
}

async function closeDeleteDialog(page: Page): Promise<void> {
  await page.locator("dialog.nvm-dialog button:not(.btn-primary)").click();
  await expect(page.locator("dialog.nvm-dialog")).toHaveCount(0);
}

async function signOut(page: Page, request?: APIRequestContext): Promise<void> {
  await page.getByRole("button", { name: /Sign out|退出登录/ }).click();
  await expect(page.locator("#login-user")).toBeVisible();
  // The fake wall clock is frozen inside one scenario. A later sign-in at the same millisecond is
  // already a different cookie because of the nonce. Advance the clock anyway so the issued times move
  // the way real time does.
  if (request) {
    const res = await request.post(`${BASE}/__fake/clock`, { data: { advance_ms: 1000 } });
    expect(res.ok()).toBeTruthy();
  }
}

test("ssh_disabled: no SSH block until a key is registered", async ({ page, request }) => {
  await resetScenario(request, "fresh");
  await page.goto("/");
  await expect(page.locator("details.login-ssh")).toHaveCount(0);

  await expect(page.locator(".login-form")).toBeVisible();
  await expect(page.locator("details.login-help summary")).toBeVisible();
  await expect(page.locator(".login-help-hint")).toBeVisible();
});

test("ssh_ok: register three algorithms, sign in, revoke down to hidden", async ({
  page,
  request,
}) => {
  // About 24 s on a single busy CPU with the release server (max of five loaded runs: 24.1 s). Three
  // real SSHSIG sign-ins, the revoke flow and its cookie checks share this one scenario, so the
  // default 30 s budget is too tight for a busier moment.
  test.slow();
  await resetScenario(request, "fresh");
  const ed25519 = await newKey("ok-ed", "ed25519");
  const p256 = await newKey("ok-p256", "ecdsa-p256");
  const p384 = await newKey("ok-p384", "ecdsa-p384");
  try {
    await signInChanged(page);
    for (const [index, key] of [ed25519, p256, p384].entries()) {
      await registerKey(page, key.pub, `e2e ${["ed25519", "p256", "p384"][index]}`);
      await expect(page.locator(".ssh-key-row")).toHaveCount(index + 1);
    }
    // The per-key command embeds the canonical key and the namespace, and never the comment. Rows
    // order by registration time; find the ed25519 row by its own text.
    const command = page
      .locator(".ssh-key-row", { hasText: "ssh-ed25519" })
      .locator(".ssh-command-code");
    await expect(command).toContainText("-n nvm-login");
    await expect(command).toContainText("ssh-ed25519");
    await expect(command).not.toContainText("e2e ed25519");

    await signOut(page, request);
    await sshSignIn(page, ed25519);

    // Revoke that key from the account page: the confirm dialog warns, the response cookie keeps this
    // browser signed in, the old ssh cookie dies through the real router.
    const cookies = await page.context().cookies(BASE);
    const oldCookie = cookies.find((c) => c.name === "nvm_session");
    expect(oldCookie, "the browser held a session cookie before the revoke").toBeDefined();
    await page.goto("/#/password");
    const row = page.locator(".ssh-key-row", { hasText: "ssh-ed25519" });
    const previousLang = await page.locator("#lang-control").inputValue();
    for (const lang of ["zh", "en"]) {
      await page.locator("#lang-control").selectOption(lang);
      await row.locator(".ssh-key-delete").click();
      await expectDeleteCopy(page, false);
      await closeDeleteDialog(page);
      await expect(page.locator(".ssh-key-row")).toHaveCount(3);
    }
    await page.locator("#lang-control").selectOption(previousLang);
    await row.locator(".ssh-key-delete").click();
    await expectDeleteCopy(page, false);
    await page.locator("dialog.nvm-dialog button.btn-primary").click();
    await expect(page.locator(".ssh-key-row")).toHaveCount(2);

    await page.goto("/#/home");
    await expect(page.locator("#login-user")).toHaveCount(0);
    const res = await request.get(`${BASE}/api/home`, {
      headers: { Cookie: `nvm_session=${oldCookie?.value ?? ""}` },
    });
    expect(res.status(), "the pre-revoke cookie is dead").toBe(401);

    await signOut(page, request);
    let challenge = await openSshBlock(page);
    let signature = await signChallenge(ed25519.file, challenge);
    await page.fill("#ssh-signature", signature);
    await page.click(".ssh-login-form button[type=submit]");
    await expect(page.locator(".ssh-login-form .form-error")).toContainText(
      /未在本程序登记|not registered/,
    );
    await sshSignIn(page, p256);

    await signOut(page, request);
    await sshSignIn(page, p384);

    // Removing the last key closes the SSH entrance; the password works. Two keys still do not get the
    // last-key warning. The last one does, in both languages, and cancelling leaves the key in place.
    await page.goto("/#/password");
    await page.locator(".ssh-key-row").nth(0).locator(".ssh-key-delete").click();
    await expectDeleteCopy(page, false);
    await page.locator("dialog.nvm-dialog button.btn-primary").click();
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    for (const lang of ["zh", "en"]) {
      await page.locator("#lang-control").selectOption(lang);
      await page.locator(".ssh-key-row").nth(0).locator(".ssh-key-delete").click();
      await expectDeleteCopy(page, true);
      await closeDeleteDialog(page);
      await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    }
    await page.locator("#lang-control").selectOption(previousLang);
    await page.locator(".ssh-key-row").nth(0).locator(".ssh-key-delete").click();
    await expectDeleteCopy(page, true);
    await page.locator("dialog.nvm-dialog button.btn-primary").click();
    await expect(page.locator(".ssh-key-row")).toHaveCount(0);
    await signOut(page, request);
    await page.goto("/");
    await expect(
      page.locator("details.login-ssh"),
      "no registered key means no SSH entrance",
    ).toHaveCount(0);
    await page.fill("#login-user", "admin");
    await page.fill("#login-pass", "longpass-99");
    await page.locator("form.login-form").first().locator("button[type=submit]").click();
    await expect(page).toHaveURL(/#\/home$/);
  } finally {
    await rm(ed25519.dir, { recursive: true, force: true });
    await rm(p256.dir, { recursive: true, force: true });
    await rm(p384.dir, { recursive: true, force: true });
  }
});

test("the sign-in entrance follows the key list when the sign-in read fails", async ({
  page,
  request,
}) => {
  await resetScenario(request, "fresh");
  const key = await newKey("list-flag", "ed25519");
  const failLoginRead = async (): Promise<void> => {
    await page.route("**/api/login", (route) => {
      if (route.request().method() === "GET") return route.abort();
      return route.continue();
    });
  };
  try {
    await signInChanged(page);
    await signOut(page, request);
    await expect(page.locator("details.login-ssh")).toHaveCount(0);
    await page.fill("#login-user", "admin");
    await page.fill("#login-pass", "longpass-99");
    await page.locator("form.login-form").first().locator("button[type=submit]").click();
    await expect(page).toHaveURL(/#\/home$/);
    await registerKey(page, key.pub, "e2e");
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    await failLoginRead();
    await signOut(page, request);
    await expect(page.locator("main")).toContainText(/请求处理失败|request handling failed/);
    await expect(
      page.locator("details.login-ssh"),
      "the first registered key keeps the SSH entrance when the sign-in read fails",
    ).toBeVisible();
    await page.unroute("**/api/login");
    await page.fill("#login-user", "admin");
    await page.fill("#login-pass", "longpass-99");
    await page.locator("form.login-form").first().locator("button[type=submit]").click();
    await expect(page).toHaveURL(/#\/home$/);
    await page.goto("/#/password");
    await page.locator(".ssh-key-delete").click();
    await expectDeleteCopy(page, true);
    await page.locator("dialog.nvm-dialog button.btn-primary").click();
    await expect(page.locator(".ssh-key-row")).toHaveCount(0);
    await failLoginRead();
    await signOut(page, request);
    await expect(page.locator("main")).toContainText(/请求处理失败|request handling failed/);
    await expect(
      page.locator("details.login-ssh"),
      "deleting the last key closes the SSH entrance even when the sign-in read fails",
    ).toHaveCount(0);
    await expect(page.locator(".login-form")).toBeVisible();
  } finally {
    await page.unroute("**/api/login").catch(() => {});
    await rm(key.dir, { recursive: true, force: true });
  }
});

test("ssh_fail: a wrong signature reports and the refresh keeps working", async ({
  page,
  request,
}) => {
  await resetScenario(request, "fresh");
  const key = await newKey("fail", "ed25519");
  try {
    await signInChanged(page);
    await registerKey(page, key.pub, "e2e");
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    await signOut(page, request);
    const challenge = await openSshBlock(page);
    // A real SSHSIG over other bytes.
    const signature = await signChallenge(key.file, `${challenge}0`);
    await page.fill("#ssh-signature", signature);
    await page.click(".ssh-login-form button[type=submit]");
    await expect(page.locator(".ssh-login-form .form-error")).toContainText(
      /签名校验未通过|did not verify/,
    );

    await page.click("#ssh-refresh");
    await expect
      .poll(async () => (await page.locator("#ssh-challenge").textContent())?.trim() ?? "")
      .toMatch(/^[0-9a-f]{64}$/);
  } finally {
    await rm(key.dir, { recursive: true, force: true });
  }
});

test("ssh_expired: a challenge older than the TTL is refused", async ({ page, request }) => {
  await resetScenario(request, "fresh");
  const key = await newKey("expired", "ecdsa-p256");
  try {
    await signInChanged(page);
    await registerKey(page, key.pub, "e2e");
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    await signOut(page, request);
    const challenge = await openSshBlock(page);
    const signature = await signChallenge(key.file, challenge);
    const res = await request.post(`${BASE}/__fake/clock`, { data: { advance_ms: 121_000 } });
    expect(res.ok()).toBeTruthy();
    await page.fill("#ssh-signature", signature);
    await page.click(".ssh-login-form button[type=submit]");
    await expect(page.locator(".ssh-login-form .form-error")).toContainText(
      /已失效|expired/,
    );
    // The invalid challenge points at the existing refresh control: it is highlighted and focused, and
    // the spent challenge is no longer copyable (the display is back to the placeholder).
    const refresh = page.locator("#ssh-refresh");
    await expect(refresh).toBeFocused();
    await expect(refresh).toHaveClass(/ssh-refresh-attention/);
    await expect(page.locator("#ssh-challenge-copy")).toBeDisabled();
    // The refresh fetches a real new challenge: the emphasis clears, the copy button returns, and the
    // fresh challenge signs in.
    await refresh.click();
    await expect
      .poll(async () => (await page.locator("#ssh-challenge").textContent())?.trim() ?? "")
      .toMatch(/^[0-9a-f]{64}$/);
    await expect(refresh).not.toHaveClass(/ssh-refresh-attention/);
    await expect(page.locator("#ssh-challenge-copy")).toBeEnabled();
    const fresh = (await page.locator("#ssh-challenge").textContent())?.trim() ?? "";
    const freshSignature = await signChallenge(key.file, fresh);
    await page.fill("#ssh-signature", freshSignature);
    await page.click(".ssh-login-form button[type=submit]");
    await expect(page).toHaveURL(/#\/home$/);
  } finally {
    await rm(key.dir, { recursive: true, force: true });
  }
});

test("ssh_not_registered: another valid key is refused by its own message", async ({
  page,
  request,
}) => {
  await resetScenario(request, "fresh");
  const registered = await newKey("reg-a", "ed25519");
  const stranger = await newKey("reg-b", "ecdsa-p384");
  try {
    await signInChanged(page);
    await registerKey(page, registered.pub, "the registered one");
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    await signOut(page, request);
    const challenge = await openSshBlock(page);
    const signature = await signChallenge(stranger.file, challenge);
    await page.fill("#ssh-signature", signature);
    await page.click(".ssh-login-form button[type=submit]");
    await expect(page.locator(".ssh-login-form .form-error")).toContainText(
      /未在本程序登记|not registered/,
    );
    await expect(page.locator("#login-user")).toBeVisible();
  } finally {
    await rm(registered.dir, { recursive: true, force: true });
    await rm(stranger.dir, { recursive: true, force: true });
  }
});

test("ssh private key pastes never leave the browser", async ({ page, request }) => {
  await resetScenario(request, "fresh");
  const key = await newKey("private", "ed25519");
  const second = await newKey("private-2", "ed25519");
  try {
    // A key must be registered, or the entrance would not be rendered.
    await signInChanged(page);
    await registerKey(page, key.pub, "e2e");
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    await signOut(page, request);
    await openSshBlock(page);
    const privateKey = await readFile(key.file, "utf8");
    let loginSubmitted = false;
    page.on("request", (req) => {
      if (req.url().includes("/api/ssh/login")) loginSubmitted = true;
    });
    await page.fill("#ssh-signature", privateKey);
    await page.click(".ssh-login-form button[type=submit]");
    await expect(page.locator(".ssh-login-form .form-error")).toContainText(
      /私钥|Private key/,
    );

    await expect(page.locator("#ssh-signature")).toHaveValue("");
    await page.waitForTimeout(300);
    expect(loginSubmitted).toBe(false);

    // Still unauthenticated: a real password sign-in is the only way to the account page.
    await expect(page.locator("#login-user")).toBeVisible();
    await page.fill("#login-user", "admin");
    await page.fill("#login-pass", "longpass-99");
    await page.locator("form.login-form").first().locator("button[type=submit]").click();
    await expect(page).toHaveURL(/#\/home$/);
    await page.goto("/#/password");

    // The registration boundary: a valid public key in place, so the private-key material in the
    // comment is the only possible refusal cause — an independent zero-POST proof of the comment path.
    let addSubmitted = false;
    page.on("request", (req) => {
      if (req.url().includes("/api/ssh/keys") && req.method() === "POST") addSubmitted = true;
    });
    await page.fill("#ssh-add-key", key.pub);
    await page.fill("#ssh-add-comment", privateKey);
    await page.click(".ssh-keys-add button[type=submit]");
    await expect(page.locator(".ssh-keys-add .form-error")).toContainText(/私钥|Private key/);
    await expect(page.locator("#ssh-add-comment")).toHaveValue("");

    await page.fill("#ssh-add-key", privateKey);
    await page.click(".ssh-keys-add button[type=submit]");
    await expect(page.locator(".ssh-keys-add .form-error")).toContainText(/私钥|Private key/);
    await expect(page.locator("#ssh-add-key")).toHaveValue("");
    await page.waitForTimeout(300);
    expect(addSubmitted).toBe(false);
    // A different valid key still registers afterwards: the form is alive, and using a second key
    // proves it independently of the first registration (the same fingerprint would be a 409
    // conflict).
    await page.fill("#ssh-add-key", second.pub);
    await page.click(".ssh-keys-add button[type=submit]");
    await expect(page.locator(".ssh-key-row")).toHaveCount(2);
  } finally {
    await rm(key.dir, { recursive: true, force: true });
    await rm(second.dir, { recursive: true, force: true });
  }
});

test("copy buttons reflect the real copy outcome", async ({ page, request }) => {
  await resetScenario(request, "fresh");
  const key = await newKey("copy", "ed25519");
  try {
    await signInChanged(page);
    await registerKey(page, key.pub, "e2e");
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    await signOut(page, request);
    const challenge = await openSshBlock(page);
    expect(challenge).toMatch(/^[0-9a-f]{64}$/);
    const copyButton = page.locator(".ssh-command .ssh-copy").first();
    const copiedText = /已复制|Copied/;

    // A dead clipboard and a dead fallback report failure: the label stays and the command text
    // remains selectable for a manual copy.
    await page.addInitScript(() => {
      Object.defineProperty(navigator, "clipboard", {
        value: {
          writeText: () => Promise.reject(new Error("denied")),
        },
        configurable: true,
      });
      document.execCommand = () => false;
    });
    await page.goto("/");
    const block = page.locator("details.login-ssh");
    await block.locator("summary").click();
    await expect
      .poll(async () => (await page.locator("#ssh-challenge").textContent())?.trim() ?? "")
      .toMatch(/^[0-9a-f]{64}$/);
    // The reload fetched a fresh challenge: the copy assertions below must compare against what this
    // page really displays.
    const displayed = (await page.locator("#ssh-challenge").textContent())?.trim() ?? "";
    await page.locator(".ssh-command .ssh-copy").first().click();
    await page.waitForTimeout(400);
    await expect(
      page.locator(".ssh-command .ssh-copy").first(),
      "a failed copy does not claim success",
    ).not.toContainText(copiedText);

    const challengeCopy = page.locator("#ssh-challenge-copy");
    await challengeCopy.click();
    await page.waitForTimeout(400);
    await expect(
      challengeCopy,
      "a failed challenge copy does not claim success",
    ).not.toContainText(copiedText);

    // A granted clipboard reports success with the transient feedback, and the challenge button copies
    // exactly the bytes the page displays.
    await page.evaluate(() => {
      const w = window as unknown as { __copied?: string[] };
      w.__copied = [];
      Object.defineProperty(navigator, "clipboard", {
        value: {
          writeText: (text: string) => {
            w.__copied?.push(text);
            return Promise.resolve();
          },
        },
        configurable: true,
      });
    });
    await challengeCopy.click();
    await expect(challengeCopy).toContainText(copiedText);
    await expect
      .poll(async () =>
        page.evaluate(() => (window as unknown as { __copied?: string[] }).__copied ?? []),
      )
      .toEqual([displayed]);
    await page.locator(".ssh-command .ssh-copy").first().click();
    await expect(page.locator(".ssh-command .ssh-copy").first()).toContainText(copiedText);

    await expect(page.locator(".ssh-command .ssh-copy").first()).not.toContainText(copiedText, {
      timeout: 5000,
    });
  } finally {
    await rm(key.dir, { recursive: true, force: true });
  }
});

test("a late challenge response never overwrites the newest one", async ({
  page,
  request,
}) => {
  await resetScenario(request, "fresh");
  const key = await newKey("late", "ed25519");
  try {
    await signInChanged(page);
    await registerKey(page, key.pub, "e2e");
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    await signOut(page, request);
    await page.goto("/");
    // The first (auto-fetch) response is held back; the second arrives at once. The page must show the
    // second challenge, never the stale one.
    let firstResolve: ((value: string) => void) | null = null;
    const firstBody = new Promise<string>((resolve) => {
      firstResolve = resolve;
    });
    let seen = 0;
    await page.route("**/api/ssh/challenge", async (route) => {
      seen += 1;
      const response = await route.fetch();
      const body = (await response.json()) as { challenge: string };
      if (seen === 1) {
        firstResolve?.(body.challenge);
        await page.waitForTimeout(700);
      }
      await route.fulfill({ response });
    });
    const block = page.locator("details.login-ssh");
    await block.locator("summary").click();

    await expect
      .poll(async () => seen)
      .toBe(1);
    // Only the placeholder is on screen while the first response is still in flight: there is no real
    // challenge to copy yet.
    await expect(page.locator("#ssh-challenge-copy")).toBeDisabled();
    await page.click("#ssh-refresh");
    await expect
      .poll(async () => (await page.locator("#ssh-challenge").textContent())?.trim() ?? "")
      .toMatch(/^[0-9a-f]{64}$/);
    // Let the stale response land and give the page a chance to misapply it; the display must still
    // hold the newest challenge.
    const stale = await firstBody;
    await page.waitForTimeout(900);
    const shown = (await page.locator("#ssh-challenge").textContent())?.trim() ?? "";
    expect(shown).toMatch(/^[0-9a-f]{64}$/);
    expect(shown, "the stale response must not overwrite the fresh challenge").not.toBe(stale);

    const signature = await signChallenge(key.file, shown);
    await page.fill("#ssh-signature", signature);
    await page.click(".ssh-login-form button[type=submit]");
    await expect(page).toHaveURL(/#\/home$/);
  } finally {
    await rm(key.dir, { recursive: true, force: true });
  }
});

test("the SSH block keeps its layout at four themes, both languages and short screens", async ({
  page,
  request,
}) => {
  await resetScenario(request, "fresh");
  const key = await newKey("themes", "ed25519");
  try {
    await signInChanged(page);
    await registerKey(page, key.pub, "e2e");
    await expect(page.locator(".ssh-key-row")).toHaveCount(1);
    await signOut(page, request);
    await page.goto("/");
    const block = page.locator("details.login-ssh");
    await expect(block).toBeVisible();
    await block.locator("summary").click();
    await expect
      .poll(async () => (await page.locator("#ssh-challenge").textContent())?.trim() ?? "")
      .toMatch(/^[0-9a-f]{64}$/);
    // A native details keeps its open state across the theme switch (no redraw) at every one of the
    // four closed-set themes.
    for (const theme of ["focused-light", "focused-dark", "plate-light", "plate-dark"]) {
      await page.selectOption("#theme-control", theme);
      await expect(block).toHaveAttribute("open");
      await expect(page.locator("#ssh-signature")).toBeVisible();
      await expect(page.locator("#ssh-challenge")).toBeVisible();
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
        `no horizontal overflow at ${theme}`,
      ).toBeLessThanOrEqual(page.viewportSize()?.width ?? 1280);
    }
    // The language switch redraws the page; the block stays rendered and open, and the wording follows
    // the language.
    await block.locator("summary").click();
    await expect(block).not.toHaveAttribute("open");
    await page.selectOption("#lang-control", "zh");
    await expect(page.locator("details.login-ssh summary")).toContainText("使用 SSH 密钥登录");
    await page.locator("details.login-ssh summary").click();
    await expect(page.locator("#ssh-signature")).toBeVisible();
    await page.selectOption("#lang-control", "en");
    await expect(page.locator("details.login-ssh summary")).toContainText("Sign in with an SSH key");
    // The language redraw replaced the whole group with a closed block: open it again so the
    // short-screen pass measures a live SSH section.
    await page.locator("details.login-ssh summary").click();
    await expect(page.locator("details.login-ssh")).toHaveAttribute("open");
    await expect(page.locator("#ssh-signature")).toBeVisible();
    // Short screens: every SSH control is reachable by real document scrolling and nothing overflows horizontally.
    for (const [width, height] of [
      [390, 480],
      [320, 568],
    ] as const) {
      await page.setViewportSize({ width, height });
      await page.evaluate(() => window.scrollTo(0, 0));
      for (const selector of ["#ssh-challenge", ".ssh-login-form button[type=submit]"]) {
        await page.evaluate(() => window.scrollTo(0, 0));
        await page.locator(selector).scrollIntoViewIfNeeded();
        // Center the target explicitly: `scrollIntoViewIfNeeded` alone only guarantees partial
        // visibility when some part is already on screen, which would make the full-visibility check
        // below flaky.
        await page.locator(selector).evaluate((node) =>
          node.scrollIntoView({ block: "center", inline: "nearest" }),
        );
        const box = (await page.locator(selector).boundingBox()) as { y: number; height: number };
        const header = (await page.locator("header.app-header").boundingBox()) as {
          y: number;
          height: number;
        };
        expect(box.y, `${selector} under the sticky header at ${width}`).toBeGreaterThanOrEqual(
          header.y + header.height - 1,
        );
        expect(box.y + box.height, `${selector} below the fold at ${width}`).toBeLessThanOrEqual(
          height + 1,
        );
      }
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
        `document overflows at ${width}`,
      ).toBeLessThanOrEqual(width);
    }
  } finally {
    await rm(key.dir, { recursive: true, force: true });
  }
});

test("scenario reset clears sessions, SSH challenges and login failures", async ({ request }) => {
  // Dirty the current AuthState first: the scenario switch must clear it, and the reset itself is
  // asserted in resetScenario.
  const warmed = await resetScenario(request, "fresh");
  expect(warmed.auth_epoch).toBeGreaterThan(0);
  // One password sign-in and sign-out fills the logged-out table, one challenge is left outstanding,
  // and one failed SSH login raises the failure counter — everything the next switch must wipe.
  const login = await request.post(`${BASE}/api/login`, {
    headers: { ...ORIGIN, "Content-Type": "application/json" },
    data: { username: "admin", password: "changed-password-1" },
  });
  expect(login.status()).toBe(200);
  const loginBody = (await login.json()) as { token?: string };
  const cookie = (await login.headersArray()).find((h) => h.name === "set-cookie")?.value ?? "";
  expect(cookie).toContain("nvm_session=");
  const logout = await request.post(`${BASE}/api/logout`, {
    headers: {
      ...ORIGIN,
      Cookie: cookie.split(";")[0],
      "X-NVM-Token": loginBody.token ?? "",
    },
  });
  expect(logout.status()).toBe(200);
  const challengeRes = await request.post(`${BASE}/api/ssh/challenge`, {
    headers: { ...ORIGIN, "Content-Type": "application/json" },
    data: {},
  });
  expect(challengeRes.status()).toBe(200);
  const challenge = (await challengeRes.json()) as { challenge_id: string };
  const badLogin = await request.post(`${BASE}/api/ssh/login`, {
    headers: { ...ORIGIN, "Content-Type": "application/json" },
    data: {
      challenge_id: challenge.challenge_id,
      signature: "-----BEGIN SSH SIGNATURE-----\ngarbage\n-----END SSH SIGNATURE-----\n",
    },
  });
  expect(badLogin.status()).toBe(401);
  // A second, unconsumed challenge stays outstanding until the rebuild.
  const outstanding = await request.post(`${BASE}/api/ssh/challenge`, {
    headers: { ...ORIGIN, "Content-Type": "application/json" },
    data: {},
  });
  expect(outstanding.status()).toBe(200);
  const dirty = await fakeState(request);
  expect(dirty.logged_out, "a logged-out cookie exists before the switch").toBeGreaterThan(0);
  expect(dirty.ssh_challenges, "an outstanding challenge exists before the switch").toBeGreaterThan(0);
  expect(dirty.limiter.consecutive_failures).toBeGreaterThan(0);

  await resetScenario(request, "fresh");
});
