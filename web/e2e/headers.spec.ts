import { test, expect } from "./local-test.js";
import { setScenario, signIn } from "./helpers.js";

test("the page shell has no inline script and no inline style", async ({ request }) => {
  await setScenario(request, "fresh");
  const res = await request.get("/");
  expect(res.status()).toBe(200);
  const html = await res.text();
  expect(html).toContain("<script type=\"module\" src=\"/main.js\">");
  expect(html).toContain("<link rel=\"stylesheet\" href=\"/styles.css\">");
  expect(html).not.toMatch(/<script(?![^>]*src=)/);
  expect(html).not.toMatch(/\sstyle=/);
  for (const path of ["/main.js", "/styles.css", "/api/login", "/api/home"]) {
    const response = await request.get(path);
    const csp = response.headers()["content-security-policy"];
    expect(csp, path).toBeTruthy();
    expect(csp, path).toContain("frame-ancestors 'none'");
    expect(csp, path).toContain("script-src 'self'");
    expect(response.headers()["referrer-policy"], path).toBeUndefined();
  }
});

test("the module graph loads without inline code", async ({ page, request }) => {
  await setScenario(request, "fresh");
  const failures: string[] = [];
  page.on("pageerror", (e) => failures.push(String(e)));
  await page.goto("/");
  await expect(page.locator("h1.app-title")).toHaveText("TrueNAS NVIDIA Driver Manager");
  expect(failures).toEqual([]);
});

test("another site cannot frame the page", async ({ page }) => {
  // The CSP carries frame-ancestors 'none', so Chromium refuses to display the page in a cross-site iframe.
  const messages: string[] = [];
  page.on("console", (message) => messages.push(message.text()));
  await page.setContent(
    `<iframe id="probe" src="http://127.0.0.1:18109/"></iframe>`,
    { waitUntil: "load" },
  );
  await expect
    .poll(() => messages.join("\n"), { timeout: 5000 })
    .toContain("frame-ancestors");

  const frame = page.frames().find((f) => f !== page.mainFrame());
  if (frame) {
    expect(await frame.locator("#login-pass").count()).toBe(0);
  }
});

test("the header GPU reading survives route and language redraws without another home read", async ({
  page,
  request,
}) => {
  await setScenario(request, "pascal");
  await page.goto("/");
  await signIn(page);
  const slot = page.locator("#header-gpu");
  await expect(slot).toContainText("GTX 1080");
  await page.waitForLoadState("networkidle");
  const reading = async (): Promise<string> =>
    ((await slot.innerText()) ?? "").replace(/\s+/g, " ").trim();
  const before = await reading();
  expect(before).toContain("GTX 1080");
  let homeReads = 0;
  page.on("request", (req) => {
    if (new URL(req.url()).pathname === "/api/home") homeReads += 1;
  });
  for (const href of ["#/tasks", "#/drivers", "#/update", "#/password"]) {
    await page.locator(`a[href='${href}']`).click();
    expect(await reading(), href).toBe(before);
  }
  await page.evaluate(() => {
    window.location.hash = "#/tasks/not-a-real-task";
  });
  await expect(page.locator(".app-main")).toBeVisible();
  expect(await reading()).toBe(before);
  await page.locator("#lang-control").selectOption("zh");
  expect(await reading()).toBe(before);
  await page.locator("#lang-control").selectOption("en");
  expect(await reading()).toBe(before);
  expect(homeReads).toBe(0);
});

test("the header subtitle is never occluded by the navigation rail across desktop and mobile viewports", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await signIn(page);
  await expect(page.locator(".app-nav")).toHaveCount(1);
  const purpose = page.locator("p.app-purpose");
  await expect(purpose).toBeVisible();

  const themes = ["focused-light", "focused-dark", "plate-light", "plate-dark"] as const;
  const desktopWidths = [980, 1024, 1280, 1440];
  const mobileWidths = [320, 390, 979];

  for (const width of desktopWidths) {
    await page.setViewportSize({ width, height: 800 });
    for (const lang of ["en", "zh"] as const) {
      await page.locator("#lang-control").selectOption(lang);
      const purposeBox = (await purpose.boundingBox()) as {
        x: number;
        y: number;
        width: number;
        height: number;
      };
      const navBox = (await page.locator("nav.app-nav").boundingBox()) as {
        x: number;
        y: number;
        width: number;
        height: number;
      };

      expect(
        navBox.y,
        `nav rail top (${navBox.y}) overlaps subtitle bottom (${purposeBox.y + purposeBox.height}) at ${width}px (${lang})`,
      ).toBeGreaterThanOrEqual(purposeBox.y + purposeBox.height - 1);

      for (const ratio of [0.25, 0.5, 0.75]) {
        const pt = {
          x: purposeBox.x + purposeBox.width * ratio,
          y: purposeBox.y + purposeBox.height / 2,
        };
        const hitTag = await page.evaluate(({ x, y }) => {
          const el = document.elementFromPoint(x, y);
          return el ? el.tagName.toLowerCase() : null;
        }, pt);
        expect(
          hitTag,
          `subtitle hit-test at (${pt.x}, ${pt.y}) intercepted by <${hitTag}> at ${width}px (${lang})`,
        ).toBe("p");
      }
    }
  }

  for (const width of mobileWidths) {
    await page.setViewportSize({ width, height: 800 });
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
    expect(overflow, `horizontal overflow at ${width}px`).toBe(true);
    const purposeBox = (await purpose.boundingBox()) as {
      x: number;
      y: number;
      width: number;
      height: number;
    };
    const hitTag = await page.evaluate(
      ({ x, y }) => {
        const el = document.elementFromPoint(x, y);
        return el ? el.tagName.toLowerCase() : null;
      },
      { x: purposeBox.x + purposeBox.width / 2, y: purposeBox.y + purposeBox.height / 2 },
    );
    expect(hitTag, `subtitle intercepted at mobile width ${width}px`).toBe("p");
  }

  for (const theme of themes) {
    await page.locator("#theme-control").selectOption(theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    const purposeBox = (await purpose.boundingBox()) as {
      x: number;
      y: number;
      width: number;
      height: number;
    };
    const navBox = (await page.locator("nav.app-nav").boundingBox()) as {
      x: number;
      y: number;
      width: number;
      height: number;
    };
    expect(navBox.y, `nav rail top overlaps subtitle under ${theme}`).toBeGreaterThanOrEqual(
      purposeBox.y + purposeBox.height - 1,
    );
  }
});
