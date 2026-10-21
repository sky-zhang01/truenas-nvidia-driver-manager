import http from "node:http";
import { test, expect, type APIRequestContext } from "./local-test.js";
import { BASE, setScenario, signIn } from "./helpers.js";

async function currentScenario(request: APIRequestContext): Promise<string> {
  const res = await request.get(`${BASE}/__fake/state`);
  expect(res.ok()).toBeTruthy();
  const state = (await res.json()) as { scenario?: string };
  return state.scenario ?? "";
}

test("an unauthenticated page returns to sign-in without losing its route", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/#/drivers");
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page).toHaveURL(/#\/drivers$/);
});

test("the empty form reports before a request, a wrong password answers on the form", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await page.click("button[type=submit]");
  await expect(page.locator(".form-error[role=alert]")).toHaveCount(1);

  await expect(page.locator(".login-help-hint")).toBeVisible();
  await expect(page.locator("details.login-help summary")).toBeVisible();

  await page.fill("#login-user", "admin");
  await page.fill("#login-pass", "definitely-wrong-password");
  await page.click("button[type=submit]");
  await expect(page.locator(".form-error")).toContainText(/incorrect|不正确/);
  await expect(page.locator(".login-help-hint")).toBeVisible();
  await expect(page.locator("details.login-help summary")).toBeVisible();
});

test("signing out ends the session on both sides", async ({ page, request }) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await signIn(page);
  await expect(page.locator("#login-pass")).toHaveCount(0);

  const logout = page.getByRole("button", { name: /Sign out|退出登录/ });
  await logout.click();
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page.locator(".app-main")).toContainText(/signed out|已退出登录/);
});

test("the scenario endpoint reports the active scenario", async ({ request }) => {
  await setScenario(request, "fresh");
  expect(await currentScenario(request)).toBe("fresh");
});

test("a real cross-site click navigation reaches login while browser API reads and framing are blocked", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");

  // Create an external site server on localhost (a different origin and site than 127.0.0.1:18109)
  const externalServer = http.createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
    res.end(`<!doctype html>
<html>
<head><title>External Site</title></head>
<body>
  <a id="cross-site-link" href="${BASE}/">Open App</a>
  <iframe id="cross-site-iframe" src="${BASE}/"></iframe>
</body>
</html>`);
  });

  await new Promise<void>((resolve) => externalServer.listen(0, "localhost", () => resolve()));
  const extAddr = externalServer.address();
  const extPort = typeof extAddr === "object" && extAddr ? extAddr.port : 0;
  const externalOrigin = `http://localhost:${extPort}`;
  // This exact origin belongs to the loopback listener created above; all other hosts remain blocked.
  await page.route(`${externalOrigin}/**`, (route) => route.continue());

  try {
    // 1. Listen for the iframe load and visit the external site. The page is    served (200); the CSP
    // is what refuses to display it in the frame.
    const iframeResPromise = page.waitForResponse((r) => r.url() === `${BASE}/`);
    await page.goto(`${externalOrigin}/`);
    const iframeRes = await iframeResPromise;
    expect(iframeRes.status()).toBe(200);
    expect(iframeRes.headers()["content-security-policy"]).toContain("frame-ancestors 'none'");

    // 2. Negative control: the external page cannot read an API response (no    CORS headers; the
    // Strict cookie is not attached cross-site).
    const apiStatus = await page.evaluate(async (target) => {
      try {
        await fetch(`${target}/api/home`);
        return "allowed";
      } catch {
        return "blocked";
      }
    }, BASE);
    expect(apiStatus).toBe("blocked");

    const iframeElement = page.frameLocator("#cross-site-iframe");
    await expect(iframeElement.locator("#login-user")).toHaveCount(0);

    const [navRes] = await Promise.all([
      page.waitForResponse((r) => r.url() === `${BASE}/`),
      page.click("#cross-site-link"),
    ]);
    expect(navRes.status()).toBe(200);
    await expect(page.locator("#login-user")).toBeVisible();
    await expect(page.locator("#login-pass")).toBeVisible();
    await expect(page).toHaveURL(`${BASE}/`);
  } finally {
    externalServer.close();
  }
});

test("a same-site, other-port page carries the Strict cookie and the Origin check is what refuses its write", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await signIn(page);

  // The attacker page sits on the same host and another port: the cookies of 127.0.0.1 are not
  // port-scoped, so SameSite=Strict still attaches them (the site is the same). Only the Origin
  // authority check separates it from the app. Real navigations are used because Chromium reports a
  // CORS-blocked fetch as a failed request, which hides the exchange.
  const attacker = http.createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
    res.end(`<!doctype html><html><body>
      <a id="read-home" href="${BASE}/api/home">read</a>
      <a id="back" href="/">back</a>
      <form id="write-logout" method="POST" action="${BASE}/api/logout">
        <button id="write-go" type="submit">write</button>
      </form>
    </body></html>`);
  });
  await new Promise<void>((resolve) => attacker.listen(0, "127.0.0.1", () => resolve()));
  const addr = attacker.address();
  const port = typeof addr === "object" && addr ? addr.port : 0;
  const attackerOrigin = `http://127.0.0.1:${port}`;

  try {
    await page.goto(`${attackerOrigin}/`);
    // A same-site navigation to the API carries the Strict cookie and does not carry the request
    // token, so the answer is the signed-out 401.
    await page.click("#read-home");
    const home = await page.locator("body").innerText();
    expect(home).toContain("auth.required");
    expect(home).not.toContain("\"kernel\"");

    // The credentialed write from the same host but another port is refused by the Origin authority
    // check (the Fetch Metadata label is gone, and the cookie alone is not the gate). The request
    // headers and the status of this very POST are asserted; no cookie value is printed.
    await page.goto(`${attackerOrigin}/`);
    const postWait = page.waitForRequest(
      (r) => r.url() === `${BASE}/api/logout` && r.method() === "POST",
    );
    const responseWait = page.waitForResponse((r) => r.url() === `${BASE}/api/logout`);
    await page.click("#write-go");
    const post = await postWait;
    const headers = await post.allHeaders();
    expect(headers["origin"]).toBe(attackerOrigin);
    expect(headers["cookie"]?.includes("nvm_session=")).toBe(true);
    const refusedRes = await responseWait;
    expect(refusedRes.status()).toBe(403);
    expect(await refusedRes.text()).toContain("auth.origin_mismatch");

    // The session survived: the refused write never reached its handler.
    await page.goto("/");
    await expect(page.locator("#login-user")).toHaveCount(0);
  } finally {
    attacker.close();
  }
});
