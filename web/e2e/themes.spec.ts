import { test, expect, type Locator, type Page } from "./local-test.js";
import { mkdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { setScenario, signIn } from "./helpers";

const THEME_VALUES = ["focused-light", "focused-dark", "plate-light", "plate-dark"] as const;
const SHOT_DIR = join(
  process.cwd().endsWith("web") ? process.cwd() : join(process.cwd(), "web"),
  "test-results",
  "theme-shots",
);

const APPEARANCES = THEME_VALUES.flatMap((theme) =>
  (["zh", "en"] as const).map((lang) => ({ theme, lang })),
);

async function bounds(locator: Locator) {
  const box = await locator.boundingBox();
  expect(box, `${locator} has a layout box`).not.toBeNull();
  return box!;
}

async function selectAppearance(page: Page, theme: string, lang: "zh" | "en"): Promise<void> {
  await page.locator("#theme-control").selectOption(theme);
  await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
  await page.locator("#lang-control").selectOption(lang);
  await expect(page.locator("html")).toHaveAttribute("lang", lang === "zh" ? "zh-CN" : "en");
  await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
}

async function expectReachable(page: Page, selector: string, height: number): Promise<void> {
  const target = page.locator(selector);
  await target.scrollIntoViewIfNeeded();
  const box = await bounds(target);
  const header = await bounds(page.locator("header.app-header"));
  expect(box.y, `${selector} under the sticky header`).toBeGreaterThanOrEqual(header.y + header.height - 1);
  expect(box.y + box.height, `${selector} below the fold`).toBeLessThanOrEqual(height + 1);
}

async function expectContentFits(locator: Locator, context: string): Promise<void> {
  for (const node of await locator.all()) {
    expect(await node.evaluate((n) => n.scrollWidth <= n.clientWidth + 1), context).toBe(true);
  }
}

async function switchWithoutRequests(page: Page, theme: string): Promise<void> {
  const requests: string[] = [];
  const record = (request: { url(): string }) => requests.push(request.url());
  page.on("request", record);
  try {
    await changeThemeProgrammatically(page, theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.waitForTimeout(700);
    expect(requests, "a theme switch must not send a request").toEqual([]);
  } finally {
    page.off("request", record);
  }
}

async function storeTheme(page: Page, theme: string): Promise<void> {
  await page.addInitScript((value) => {
    try {
      window.localStorage.setItem("nvm.theme", value);
    } catch {
      /* the page must still boot */
    }
  }, theme);
}

// Change the theme the programmatic way (no user focus move): the select's change handler must apply
// the theme without redrawing, so the view's own focus, selection and nodes stay exactly where they
// are.
async function changeThemeProgrammatically(page: Page, theme: string): Promise<void> {
  await page.locator("#theme-control").evaluate((node, value) => {
    const select = node as HTMLSelectElement;
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  }, theme);
}

test("first use defaults to focused-light, offers exactly the four closed-set themes, and shows the independent sign-in page", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-light");
  // The header — and with it the control — exists only after the boot login status read has rendered the shell.
  await expect(page.locator("#theme-control")).toHaveCount(1);
  const values = await page
    .locator("#theme-control option")
    .evaluateAll((options) => options.map((o) => (o as HTMLOptionElement).value));
  expect(values).toEqual([...THEME_VALUES]);

  expect(values[0]).toBe("focused-light");
  // The signed-out page is an independent sign-in page: no navigation rail and no business links, but
  // the title, both controls and the form are there. (The task-state dot exists only on the signed-in
  // tasks entry.)
  await expect(page.locator(".app-nav")).toHaveCount(0);
  await expect(page.locator("h1.app-title")).toBeVisible();
  await expect(page.locator("#theme-control")).toBeVisible();
  await expect(page.locator("#lang-control")).toBeVisible();
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page.locator("#login-pass")).toBeVisible();
});

test("the choice applies at once, persists across reload, sign-out, 401 and the zh↔en switch", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await page.locator("#theme-control").selectOption("focused-dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-dark");
  expect(await page.evaluate(() => window.localStorage.getItem("nvm.theme"))).toBe("focused-dark");

  await signIn(page);

  expect(
    await page
      .locator("#theme-control option")
      .evaluateAll((options) => options.map((o) => (o as HTMLOptionElement).value)),
  ).toEqual([...THEME_VALUES]);
  // zh → en → zh inside the same SPA: every switch re-renders the shell, the theme and the control
  // value must survive each one.
  await page.locator("#lang-control").selectOption("zh");
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-CN");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-dark");
  await expect(page.locator("#theme-control")).toHaveValue("focused-dark");
  await page.locator("#lang-control").selectOption("en");
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-dark");
  await expect(page.locator("#theme-control")).toHaveValue("focused-dark");

  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-dark");

  // A real session end inside the SPA: the browser's own session cookie is dropped through the page's
  // same-origin logout, the next same-document route change reads 401 `auth.required`, and the app
  // renders the sign-in page without a reload — theme untouched.
  await page.goto("/#/drivers");
  await expect(page.locator("table.drivers")).toBeVisible();
  await page.evaluate(() => {
    const token = sessionStorage.getItem("nvm.token");
    return fetch("/api/logout", {
      method: "POST",
      credentials: "same-origin",
      headers: token ? { "X-NVM-Token": token } : {},
    });
  });
  await page.goto("/#/home");
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page).toHaveURL(/#\/home$/);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-dark");
});

test("an unknown stored value falls back to the default theme", async ({ page, request }) => {
  await setScenario(request, "fresh");
  await storeTheme(page, "system-auto");
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-light");
});

test("a throwing localStorage still boots and still allows the in-page switch", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.addInitScript(() => {
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      get() {
        throw new Error("storage blocked");
      },
    });
  });
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-light");
  await page.locator("#theme-control").selectOption("plate-dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "plate-dark");
});

test("the signed-out page is an independent centered sign-in layout, at every theme, language and width", async ({
  page,
  request,
}) => {
  // About 20 s on a single busy CPU with the release server (max of five loaded runs: 20.0 s). The
  // matrix is four themes x two languages x four widths and the default 30 s budget leaves too little
  // room for a busier moment.
  test.slow();
  await setScenario(request, "fresh");

  await page.goto("/#/drivers");
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page).toHaveURL(/#\/drivers$/);
  await expect(page.locator(".app-nav")).toHaveCount(0);

  await page.fill("#login-user", "admin");
  await page.locator("#lang-control").selectOption("zh");
  await expect(page.locator("#login-user")).toHaveValue("admin");
  await expect(page.locator(".app-nav")).toHaveCount(0);

  // All four themes × both languages × three widths, every round through the page's own controls, then
  // the real geometry is measured.
  for (const { theme, lang } of APPEARANCES) {
    await selectAppearance(page, theme, lang);
    for (const width of [1280, 390, 320]) {
      await page.setViewportSize({ width, height: 800 });
      await page.evaluate(() => window.scrollTo(0, 0));
      await expect(page.locator("#login-user")).toBeVisible();
      await expect(page.locator(".app-nav")).toHaveCount(0);
      await expect(page.locator("h1.app-title")).toBeVisible();
      await expect(page.locator("#theme-control")).toBeVisible();
      await expect(page.locator("#lang-control")).toBeVisible();
      // Real geometry: no horizontal overflow anywhere, the form column is centered, and the sign-in
      // group starts below the header at scrollTop 0 — vertical centering may never clip the top.
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
        `document overflows at ${width} (${theme}, ${lang})`,
      ).toBeLessThanOrEqual(width);
      const form = await bounds(page.locator("form.login-form"));
      expect(
        Math.abs(form.x + form.width / 2 - width / 2),
        `form off-center at ${width} (${theme}, ${lang})`,
      ).toBeLessThanOrEqual(8);
      const header = await bounds(page.locator("header.app-header"));
      const headerBottom = header.y + header.height;
      expect(
        form.y,
        `form overlaps/clips at the header at ${width} (${theme}, ${lang})`,
      ).toBeGreaterThanOrEqual(headerBottom - 1);
      const stack = await bounds(page.locator(".login-stack"));
      expect(
        stack.y,
        `sign-in group clipped at the top at ${width} (${theme}, ${lang})`,
      ).toBeGreaterThanOrEqual(headerBottom - 1);
    }
  }

  // The theme control works on the signed-out page and only touches the root attribute: the typed
  // username stays, the open guidance stays open, the focus stays put and no request of any kind is
  // sent.
  await page.fill("#login-user", "admin");
  await page.locator("details.login-help summary").click();
  await expect(page.locator("details.login-help")).toHaveAttribute("open");
  await page.locator("#login-user").focus();
  await switchWithoutRequests(page, "focused-dark");
  expect(await page.evaluate(() => document.activeElement?.id)).toBe("login-user");
  await expect(page.locator("#login-user")).toHaveValue("admin");
  await expect(page.locator("details.login-help")).toHaveAttribute("open");
  await page.locator("#theme-control").selectOption("focused-light");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-light");

  // Signing in restores the full shell with its navigation; signing out returns to the independent sign-in page.
  await signIn(page);
  await expect(page.locator(".readings .reading").first()).toBeVisible();
  await expect(page.locator(".app-nav")).toHaveCount(1);
  await page.getByRole("button", { name: /Sign out|退出登录/ }).click();
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page.locator(".app-nav")).toHaveCount(0);
});

test("the sign-in group sits in the middle band with real top whitespace", async ({ page, request }) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await expect(page.locator("#login-user")).toBeVisible();
  const titleGap = async (): Promise<number> => {
    const t = await bounds(page.locator(".login-stack h2"));
    const h = await bounds(page.locator("header.app-header"));
    // The free band between the header's bottom edge and the title's top edge — the title's own height
    // never counts into the whitespace.
    return t.y - (h.y + h.height);
  };
  // The accepted numbers, measured for every theme in both languages: a real breathing band between
  // the header and the sign-in group on a normal desktop and a normal phone height.
  for (const { theme, lang } of APPEARANCES) {
    await selectAppearance(page, theme, lang);
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(() => window.scrollTo(0, 0));
    expect(await titleGap(), `desktop 1440x900 title top gap (${theme}, ${lang})`).toBeGreaterThanOrEqual(80);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.evaluate(() => window.scrollTo(0, 0));
    expect(await titleGap(), `phone 390x844 title top gap (${theme}, ${lang})`).toBeGreaterThanOrEqual(48);
  }
});

test("on short screens the sign-in page scrolls normally from the top without clipping", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await expect(page.locator("#login-user")).toBeVisible();
  for (const [width, height] of [
    [390, 480],
    [320, 568],
  ] as const) {
    await page.setViewportSize({ width, height });
    await page.evaluate(() => window.scrollTo(0, 0));
    const headerBox = await bounds(page.locator("header.app-header"));
    const headerBottom = headerBox.y + headerBox.height;
    const title = await bounds(page.locator(".login-stack h2"));
    // Nothing clips the top: the title starts below the header in the unscrolled page — no
    // fixed-height, transform or centering crop.
    expect(title.y, `title clipped at ${width}x${height}`).toBeGreaterThanOrEqual(headerBottom - 1);
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth),
      `document overflows at ${width}x${height}`,
    ).toBeLessThanOrEqual(width);
    // Every target is brought into view by its own scroll and must then sit fully inside the free
    // viewport: below the sticky header's bottom edge and above the viewport's bottom edge (never only
    // "bottom visible").
    await expectReachable(page, "form.login-form button[type=submit]", height);
    // No nested vertical scroll container:.app-main grows with its content.
    const mainOverflow = await page.evaluate(() => {
      const main = document.querySelector(".app-main");
      return main ? main.scrollHeight - main.clientHeight : -1;
    });
    expect(mainOverflow, `nested vertical scroll at ${width}x${height}`).toBeLessThanOrEqual(1);
    // With the guidance expanded the group is taller still: submit and the last help item stay
    // reachable, the top stays unclipped.
    await page.locator("details.login-help summary").click();
    await expect(page.locator("details.login-help")).toHaveAttribute("open");
    await page.evaluate(() => window.scrollTo(0, 0));
    const openTitle = await bounds(page.locator(".login-stack h2"));
    expect(openTitle.y, `expanded title clipped at ${width}x${height}`).toBeGreaterThanOrEqual(
      headerBottom - 1,
    );
    for (const selector of ["form.login-form button[type=submit]", ".login-help-list li:last-child"]) {
      await expectReachable(page, selector, height);
    }
    // The expanded group always exceeds a short screen, and its end was brought into view: the
    // document itself did the scrolling.
    const scrolled = await page.evaluate(() => ({
      doc: document.scrollingElement?.scrollTop ?? -1,
      main: (() => {
        const main = document.querySelector(".app-main");
        return main ? main.scrollHeight - main.clientHeight : -1;
      })(),
    }));
    expect(scrolled.doc, `document did not scroll at ${width}x${height}`).toBeGreaterThan(0);
    expect(scrolled.main, `nested vertical scroll at ${width}x${height}`).toBeLessThanOrEqual(1);

    await page.locator("details.login-help summary").click();
    await expect(page.locator("details.login-help")).not.toHaveAttribute("open");
  }
});

test("the sign-in page explains setup and password reset without exposing secrets", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await expect(page.locator("#login-user")).toBeVisible();
  const details = page.locator("details.login-help");
  await expect(details).toHaveCount(1);
  await expect(details.locator("summary")).toBeVisible();
  // Collapsed by default, and placed after the form — never a tutorial wall before it. The short next
  // step is readable without expanding anything.
  expect(await details.getAttribute("open")).toBeNull();
  const hint = page.locator(".login-help-hint");
  await expect(hint).toBeVisible();
  await expect(hint).toContainText(/whoever deployed the app|部署者/);
  const formBox = await bounds(page.locator("form.login-form"));
  const hintBox = await bounds(hint);
  expect(hintBox.y, "the hint must sit below the form").toBeGreaterThanOrEqual(
    formBox.y + formBox.height - 1,
  );
  const helpBox = await bounds(details);
  expect(helpBox.y, "the guidance must sit below the form").toBeGreaterThanOrEqual(
    formBox.y + formBox.height - 1,
  );

  await details.locator("summary").click();
  await expect(details).toHaveAttribute("open");
  await expect(details).toContainText(/setup page|设置页/);
  await expect(details).not.toContainText("NVM_INITIAL_ADMIN_PASSWORD");
  await expect(details).toContainText("app logs");

  await page.locator("#lang-control").selectOption("zh");
  await expect(page.locator("details.login-help summary")).toContainText("没有账号");
  await expect(page.locator("details.login-help")).toContainText("设置页");
  await expect(page.locator(".login-help-hint")).toContainText("联系部署者");

  // The interface never shows the real configured secret, a host path, or the conditioned
  // account-reset help file name — only variable names and generic wording.
  const bodyText = await page.locator("body").textContent();
  expect(bodyText).not.toContain("initial-password-1");
  expect(bodyText).not.toContain("/var/lib");
  expect(bodyText).toContain("web-credentials.json");
});

test("a 401 session end lands on the independent sign-in page and keeps the route", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await page.goto("/");
  await signIn(page);
  await expect(page.locator(".app-nav")).toHaveCount(1);
  await page.goto("/#/drivers");
  await expect(page.locator("table.drivers")).toBeVisible();
  // Drop the cookie behind the app's back; the next route change reads 401 `auth.required` and the app
  // returns to the sign-in page, route kept.
  await page.evaluate(() => {
    const token = sessionStorage.getItem("nvm.token");
    return fetch("/api/logout", {
      method: "POST",
      credentials: "same-origin",
      headers: token ? { "X-NVM-Token": token } : {},
    });
  });
  await page.goto("/#/home");
  await expect(page.locator("#login-user")).toBeVisible();
  await expect(page).toHaveURL(/#\/home$/);
  await expect(page.locator(".app-main")).toContainText(/session has ended|登录已失效/);
  await expect(page.locator(".app-nav")).toHaveCount(0);
});

// The fake host's `setup_problem` scenario carries a single `PidHost` item, which is not tall enough
// to exercise long diagnostics. This fabricated login status keeps the closed key set (every item key
// is an startup-check notice phrase, no `raw` anywhere) while genuinely exceeding a viewport, so the
// signed-out page is under test with real, tall diagnostics.
function longStartupProblems(): Record<string, unknown> {
  const items = [
    "startup.mapping.no_hostname_line",
    "startup.mapping.nsenter_failed",
    "startup.mapping.path_characters",
    "startup.mapping.check_write_failed",
    "startup.mapping.check_read_failed",
    "startup.mapping.check_mismatch",
  ].flatMap((key) => [{ key }, { key }, { key }]);
  return { problems: [{ key: "startup.failed", params: { items } }] };
}

test("long startup diagnostics on the signed-out page wrap and stay inside the viewport", async ({
  page,
  request,
}) => {
  await setScenario(request, "setup_problem");
  // Patch login status with the tall diagnostics; everything else in the response stays.
  await page.route("**/api/login", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    body["setup"] = longStartupProblems();
    await route.fulfill({ response, json: body });
  });
  await page.goto("/");
  await expect(page.locator("#login-user")).toBeVisible();
  const noticeBox = await bounds(page.locator(".top-notices .notice").first());
  // The fixture is genuinely long: the first notice is taller than a phone viewport, otherwise this
  // test proves nothing.
  expect(noticeBox.height, "the diagnostics fixture must exceed a phone viewport").toBeGreaterThan(500);
  await expect(page.locator(".app-nav")).toHaveCount(0);
  for (const [width, height] of [
    [1280, 800],
    [320, 800],
    [320, 568],
  ] as const) {
    await page.setViewportSize({ width, height });
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth),
      `document overflows at ${width}x${height}`,
    ).toBeLessThanOrEqual(width);
    await expectContentFits(page.locator(".top-notices .notice"), `a sign-in notice clips its content at ${width}x${height}`);
    // With the diagnostics present the group still starts below the header at scrollTop 0, and the
    // submit stays reachable on a short screen — measured on both edges, never only "bottom visible".
    await page.evaluate(() => window.scrollTo(0, 0));
    const header = await bounds(page.locator("header.app-header"));
    const title = await bounds(page.locator(".login-stack h2"));
    expect(title.y, `title clipped at ${width}x${height}`).toBeGreaterThanOrEqual(
      header.y + header.height - 1,
    );
    await expectReachable(page, "form.login-form button[type=submit]", height);
    // Long diagnostics + expanded help: the group is now genuinely taller than the viewport, and the
    // help end must still be reachable on both edges, with the scrolling done by the document itself.
    await page.locator("details.login-help summary").click();
    await expect(page.locator("details.login-help")).toHaveAttribute("open");
    await expectReachable(page, ".login-help-list li:last-child", height);
    const scrolledDoc = await page.evaluate(() => document.scrollingElement?.scrollTop ?? -1);
    expect(scrolledDoc, `document did not scroll at ${width}x${height}`).toBeGreaterThan(0);
    await page.locator("details.login-help summary").click();
    await expect(page.locator("details.login-help")).not.toHaveAttribute("open");
    // AC: the notice boxes and the sign-in text share the same edges.
    const titleBox = await bounds(page.locator(".login-stack h2"));
    const noticeBox = await bounds(page.locator(".top-notices .notice").first());
    expect(
      Math.abs(noticeBox.x - titleBox.x),
      `notice column edge off the text edge at ${width}x${height}`,
    ).toBeLessThanOrEqual(1);
  }

  await page.setViewportSize({ width: 1280, height: 800 });
  const titleX = (await bounds(page.locator(".login-stack h2"))).x;
  const noticeX = (await bounds(page.locator(".top-notices .notice").first())).x;
  expect(Math.abs(noticeX - titleX), "notice column edge off the text edge at 1280").toBeLessThanOrEqual(1);
});

test("switching the theme redraws nothing and keeps selection, sort and focus on the drivers page", async ({
  page,
  request,
}) => {
  await setScenario(request, "drivers_rows");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/drivers");
  await expect(page.locator("table.drivers")).toBeVisible();

  await page.locator("#select-all").check();
  // The table's initial order is version ascending, so the first click turns it descending and the
  // second back to ascending.
  await page.locator("#sort-version").click();
  await page.locator("#sort-version").click();
  await expect(page.locator("th[aria-sort=ascending]")).toHaveCount(1);
  await page.locator("#sort-version").focus();
  await page.evaluate(() => {
    (document.getElementById("select-all") as HTMLElement).dataset["probe"] = "alive";
  });

  // Every request of any kind counts, not only API calls.
  await switchWithoutRequests(page, "focused-light");

  await expect(page.locator('#select-all[data-probe="alive"]')).toHaveCount(1);
  expect(await page.locator("#select-all").isChecked()).toBe(true);
  await expect(page.locator("th[aria-sort=ascending]")).toHaveCount(1);

  expect(await page.evaluate(() => document.activeElement?.id)).toBe("sort-version");

  // A real user path: focus the control, then change it — focus stays on the control, the theme
  // applies, and the page keeps its own focus target.
  await page.locator("#theme-control").focus();
  await page.locator("#theme-control").selectOption("focused-dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "focused-dark");
  expect(await page.evaluate(() => document.activeElement?.id)).toBe("theme-control");
});

test("switching the theme keeps the update page's GPUs, version, staged file and open dialog", async ({
  page,
  request,
}) => {
  test.setTimeout(60_000);
  await setScenario(request, "mixed_1b80_2b85");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/update");
  await expect(page.locator("#version-select")).toBeVisible();

  const boxes = page.locator("fieldset.gpu-select input[type=checkbox]");
  await expect(boxes).toHaveCount(2);
  await boxes.nth(1).check();
  await page
    .locator("#version-select")
    .selectOption({ index: (await page.locator("#version-select option").count()) - 1 });
  const chosen = await page.locator("#version-select").inputValue();
  expect(chosen).not.toBe("");

  // Open the upload dialog and stage a real.run file so the received-file state exists before the switch.
  await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
  const dialog = page.locator("dialog[open]");
  await expect(dialog).toBeVisible();
  const body = Buffer.alloc(10 * 1024 * 1024, 0x41);
  body.write(
    '#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04"\n',
  );
  await expect(dialog.locator("#upload-files")).toBeEnabled();
  await dialog.locator("#upload-files").setInputFiles({
    name: "theme-check.run",
    mimeType: "application/octet-stream",
    buffer: body,
  });

  await expect(dialog.locator(".upload-summary")).toContainText("theme-check.run");
  await page.evaluate(() => {
    (document.querySelector("dialog[open]") as HTMLElement).dataset["probe"] = "alive";
  });

  await switchWithoutRequests(page, "plate-dark");

  await expect(page.locator('dialog[data-probe="alive"]')).toHaveCount(1);
  await expect(page.locator("dialog[open]")).toBeVisible();
  await expect(page.locator(".upload-summary")).toContainText("theme-check.run");
  expect(await boxes.nth(1).isChecked()).toBe(true);
  expect(await page.locator("#version-select").inputValue()).toBe(chosen);
  await dialog.locator(".upload-footer").getByRole("button", { name: /Close|关闭/ }).click();
});

type HomeOverride = { active: boolean; interrupted: boolean };

/** A fabricated task-detail body with the given status for the theme dot tests. */
function themeDetailBody(id: string, status: string): Record<string, unknown> {
  return {
    task_id: id,
    status,
    state_read_error: null,
    state_note: null,
    phase: "download",
    plan: null,
    loaded_version_start: null,
    loaded_version_end: null,
    download: null,
    replace: [],
    checks: [],
    restore: null,
    error: null,
    gpu_users: null,
    version_change: false,
    notice: null,
    log: { lines: [], next: 0, reset: false },
    unit_query: null,
  };
}

test("a very long file name wraps inside the upload dialog and the last CTA stays reachable", async ({
  page,
  request,
}) => {
  test.setTimeout(90_000);
  await setScenario(request, "mixed_1b80_2b85");
  await storeTheme(page, "plate-light");
  await page.goto("/");
  await signIn(page);
  // Within the server's own name limit; long enough to stretch a layout that does not really wrap.
  const longName = `${"a".repeat(190)}.run`;
  const body = Buffer.alloc(10 * 1024 * 1024, 0x41);
  body.write('#!/bin/sh\nlabel="NVIDIA Accelerated Graphics Driver for Linux-x86_64 580.178.04"\n');
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    await page.goto("/#/update");
    // With two counted GPUs the version list is deliberately empty until a GPU is picked (rules.ts
    // noneSelected); pick one like a real user.
    const boxes = page.locator("fieldset.gpu-select input[type=checkbox]");
    await boxes.nth(0).check();
    await page.waitForFunction(() => (document.getElementById("version-select")?.options.length ?? 0) > 0);
    await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
    const dialog = page.locator("dialog[open]");
    await expect(dialog).toBeVisible();
    await expect(dialog.locator("#upload-files")).toBeEnabled();
    await dialog.locator("#upload-files").setInputFiles({
      name: longName,
      mimeType: "application/octet-stream",
      buffer: body,
    });

    await expect(dialog.locator(".upload-summary").getByText(longName).first()).toBeVisible();

    await expectContentFits(dialog.locator(".upload-summary .upload-receipt"), "a notice stretches its dialog");
    const dbox = await bounds(dialog);
    expect(dbox.x).toBeGreaterThanOrEqual(0);
    expect(dbox.x + dbox.width).toBeLessThanOrEqual(width);
    // The dialog itself never grows a horizontal scrollbar for long tokens; the overflow stays
    // vertical with the last CTA reachable.
    const dlgScroll = await dialog.evaluate((n) => ({ s: n.scrollWidth, c: n.clientWidth }));
    const offenders = await dialog.evaluate((n) => {
      const limit = n.getBoundingClientRect().right - 16; // dialog padding-right
      const out: string[] = [];
      for (const el of Array.from(n.querySelectorAll("*"))) {
        const r = el.getBoundingClientRect();
        if (r.right > limit + 1) {
          out.push(`${el.tagName}.${String(el.className)} right=${Math.round(r.right)}`);
        }
      }
      return out.slice(0, 6);
    });
    expect(dlgScroll.s, `dialog content clips horizontally: ${offenders.join(" | ")}`).toBeLessThanOrEqual(
      dlgScroll.c + 1,
    );

    await dialog.locator(".upload-footer").getByRole("button", { name: /Close|关闭/ }).click();
    await expect(dialog).toHaveCount(0);
  }
});

test("driver-row notices keep the document inside the viewport at every theme, language and width", async ({
  page,
  request,
}) => {
  test.setTimeout(180_000);
  // The real drivers fixture: four rows whose kept and in-use entries render the shared notice layout
  // inside the wide table — the parent container that could push the document past a phone viewport.
  await setScenario(request, "drivers_rows");
  await page.goto("/");
  await signIn(page);

  for (const { theme, lang } of APPEARANCES) {
    for (const width of [320, 390, 1280]) {
      await page.setViewportSize({ width, height: 800 });
      await page.goto("/#/drivers");
      await expect(page.locator("table.drivers")).toBeVisible();
      // A hash-only goto would not rerun any stored init script, so the theme is really switched
      // through the page's own control every round and the applied attribute is read back: all four
      // themes are truly under test, not just the first one.
      await selectAppearance(page, theme, lang);
      await expect(page.locator("table.drivers")).toBeVisible();

      // The fixture is not empty: the kept and in-use rows really carry their inline notices before
      // anything is measured.
      const reasons = page.locator("td .notice.inline-reason");
      await expect(reasons).toHaveCount(2);

      // Real geometry, not style text: the document must never be wider than the viewport; the table
      // may scroll inside its own area.
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
        `document overflows at ${width} (${theme}, ${lang})`,
      ).toBeLessThanOrEqual(width);
      expect(await page.evaluate(() => document.body.scrollWidth)).toBeLessThanOrEqual(width);

      // Every notice in the main content keeps its own content inside its box (long tokens wrap; nothing is cut off).
      await expectContentFits(page.locator(".app-main .notice"), `a notice clips its content at ${width} (${theme}, ${lang})`);

      // The sr-only level word survives: attached, real announced text, and its own box lies inside
      // its notice instead of escaping the page.
      for (const notice of await reasons.all()) {
        const word = notice.locator(".sr-only");
        await expect(word).toBeAttached();
        expect((await word.textContent()) ?? "").not.toBe("");
        const nb = await bounds(notice);
        const wb = await bounds(word);
        expect(wb.x, `sr-only escapes its notice at ${width} (${theme}, ${lang})`).toBeGreaterThanOrEqual(
          nb.x - 1,
        );
        expect(wb.x + wb.width).toBeLessThanOrEqual(nb.x + nb.width + 1);
      }

      // The visible level icon column is a real, non-zero box. It may sit inside the table's own
      // horizontal scroll at 320 — allowed.
      for (const icon of await page.locator("td .notice .notice-icon").all()) {
        const r = await icon.boundingBox();
        expect(r, "the notice icon has no box").not.toBeNull();
        expect(r!.width).toBeGreaterThan(0);
        expect(r!.height).toBeGreaterThan(0);
      }
    }
  }
});

test("a late terminal task-detail response for a page left behind refreshes nothing", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  let release: (() => void) | null = null;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/tasks/T-LATE*", async (route) => {
    await gate; // hold the detail response until the page has moved on
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      json: themeDetailBody("T-LATE-0001", "SUCCEEDED"),
    });
  });
  await page.route("**/api/tasks", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    body["tasks"] = [
      {
        task_id: "T-LATE-0001",
        status: "RUNNING",
        phase: "download",
        source: { kind: "official" },
        version: "580.178.04",
        created_at: 1100,
        finished_at: null,
      },
    ];
    body["active_task"] = null;
    await route.fulfill({ response, json: body });
  });

  await page.goto("/");
  await signIn(page);
  const loginReads: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/api/login")) loginReads.push(r.url());
  });
  await page.goto("/#/tasks");
  await page.locator("table.tasks a").first().click();
  // Leave the task page while its first task-detail response is still held — home deliberately fires
  // no login status of its own, so the held read stays the newest.
  await page.goto("/#/home");
  await expect(page.locator(".readings .reading").first()).toBeVisible();
  const readsBefore = loginReads.length;
  release?.();
  await page.waitForTimeout(1500);
  // The late terminal response ran no follow-up login status and ended no session.
  expect(loginReads.length).toBe(readsBefore);
  await expect(page.locator("#login-user")).toHaveCount(0);
  await expect(page.locator(".readings .reading").first()).toBeVisible();
});

test("the mobile dot anchors to the icon container corner at 320 and 390 without overflow", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  await storeTheme(page, "plate-dark");
  const override = { active: true, interrupted: false, unknown: false };
  await page.route("**/api/home", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    body["active_task"] = override.active
      ? { task_id: "T-THEME-0001", status: override.unknown ? "UNKNOWN" : "RUNNING" }
      : null;
    body["interrupted"] = override.interrupted
      ? { task_id: "T-THEME-0001", phase: "download" }
      : null;
    await route.fulfill({ response, json: body });
  });
  await page.goto("/");
  await signIn(page);
  await expect(page.locator(".readings .reading").first()).toBeVisible();

  for (const width of [320, 390]) {
    await page.setViewportSize({ width, height: 800 });
    await expect(page.locator(".app-nav a[data-route='#/tasks'] .nav-dot")).toBeVisible();
    const link = page.locator(".app-nav a[data-route='#/tasks']");
    const state = link.locator(".nav-task-state");
    await expect(state).toBeAttached();
    expect((await state.textContent()) ?? "").not.toBe("");

    const lb = await bounds(link);
    const db = await bounds(link.locator(".nav-dot"));
    expect(Math.round(db.width)).toBe(6);
    expect(Math.round(db.height)).toBe(6);

    expect(db.x).toBeGreaterThanOrEqual(lb.x);
    expect(db.y).toBeGreaterThanOrEqual(lb.y);
    expect(db.x + db.width).toBeLessThanOrEqual(lb.x + lb.width);
    expect(db.y + db.height).toBeLessThanOrEqual(lb.y + lb.height);

    const icon = await bounds(link.locator(".nav-icon"));
    expect(Math.round(db.y)).toBe(Math.round(icon.y));
    expect(Math.round(db.x + db.width)).toBe(Math.round(icon.x + icon.width + 2));
    const svg = await bounds(link.locator(".nav-icon svg"));
    const overlaps =
      !(db.x + db.width <= svg.x) &&
      !(svg.x + svg.width <= db.x) &&
      !(db.y + db.height <= svg.y) &&
      !(svg.y + svg.height <= db.y);
    expect(overlaps).toBe(false);
    // The readings stack: a real grid layout (label column / value line, no stuck-together inline
    // pairs), and no long value is clipped inside the nameplate — measured inside the panel, not just
    // on the document.
    expect(await page.locator(".reading").first().evaluate((n) => getComputedStyle(n).display)).toBe("grid");
    await expectContentFits(page.locator(".reading"), "a reading row clips its content");
    const readings = await page.locator(".readings").evaluate((n) => ({
      scroll: n.scrollWidth,
      client: n.clientWidth,
    }));
    expect(readings.scroll).toBeLessThanOrEqual(readings.client + 1);

    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    for (const route of ["#/update", "#/drivers", "#/tasks"]) {
      await page.goto(`/${route}`);
      await page.waitForTimeout(200);
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
        `overflow at ${width} ${route}`,
      ).toBeLessThanOrEqual(width);
    }

    await page.goto("/#/update");
    await expect(page.locator("#version-select")).toBeVisible();
    await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
    const dialog = page.locator("dialog[open]");
    await expect(dialog).toBeVisible();
    const dbox = await bounds(dialog);
    expect(dbox.x).toBeGreaterThanOrEqual(0);
    expect(dbox.x + dbox.width).toBeLessThanOrEqual(width);
    await dialog.locator(".upload-footer").getByRole("button", { name: /Close|关闭/ }).click();
    await expect(dialog).toHaveCount(0);

    await page.goto("/#/home");
    await expect(page.locator(".readings .reading").first()).toBeVisible();
  }

  const tasksLink = page.locator(".app-nav a[data-route='#/tasks']");
  override.active = false;
  override.interrupted = true;
  await page.goto("/#/tasks");
  await page.goto("/#/home");
  await expect(page.locator(".readings .reading").first()).toBeVisible();
  await expect(tasksLink.locator(".nav-task-state")).toHaveText(
    /Interrupted task needs attention|有中断的任务待处理/,
  );

  override.active = true;
  override.unknown = true;
  override.interrupted = false;
  await page.goto("/#/tasks");
  await page.goto("/#/home");
  await expect(page.locator(".readings .reading").first()).toBeVisible();
  await expect(tasksLink.locator(".nav-task-state")).toHaveText("");
  await expect(tasksLink.locator(".nav-dot")).toBeHidden();

  // With no task fact at all, the empty hook paints no card: the family :not(:empty) rules must win
  // over the empty element.
  override.active = false;
  await page.goto("/#/tasks");
  await page.goto("/#/home");
  await expect(page.locator(".readings .reading").first()).toBeVisible();
  const emptyBox = await page
    .locator(".task-state")
    .evaluate((n) => getComputedStyle(n).backgroundColor);
  expect(emptyBox, "the empty task-state hook paints a card").toBe("rgba(0, 0, 0, 0)");

  // Real touch entries measure at least 44px tall on the control itself, including the header's own text links.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/#/update");
  await page.waitForFunction(() => (document.getElementById("version-select")?.options.length ?? 0) > 0);
  for (const probe of ["#version-select", ".actions .btn-primary", ".app-nav a", ".app-actions a"]) {
    const heights = await page.locator(probe).evaluateAll((nodes) =>
      nodes.map((n) => n.getBoundingClientRect().height),
    );
    expect(heights.length, probe).toBeGreaterThan(0);
    for (const h of heights) expect(h, `${probe} height`).toBeGreaterThanOrEqual(44);
  }
  await page.goto("/#/drivers");
  await expect(page.locator("table.drivers")).toBeVisible();
  const sortHeights = await page
    .locator("th button")
    .evaluateAll((nodes) => nodes.map((n) => n.getBoundingClientRect().height));
  expect(sortHeights.length).toBeGreaterThan(0);
  for (const h of sortHeights) expect(h, "sort button height").toBeGreaterThanOrEqual(44);
});

test("the dot follows the real task-detail task state and the task page keeps polling and log scroll", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  let status = "RUNNING";
  const detailCalls: number[] = [];
  // The fake task does not exist server-side, so the mock must answer with an explicit 200 —
  // inheriting the real route's 404 would make the API layer drop the body and stop polling, which is
  // not a theme behavior at all.
  const lines = Array.from({ length: 200 }, (_, i) => ({
    t: 1000 + i,
    key: "log.phase_enter",
    p: { phase: "download" },
  }));
  await page.route("**/api/tasks/T-THEME*", async (route) => {
    detailCalls.push(Date.now());
    await route.fetch().catch(() => null); // the request itself still goes out
    const body = {
      ...themeDetailBody("T-THEME-0001", status),
      log: { lines, next: 0, reset: false },
    };
    await route.fulfill({ status: 200, contentType: "application/json", json: body });
  });

  await page.route("**/api/tasks", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    body["tasks"] = [
      {
        task_id: "T-THEME-0001",
        status,
        phase: "download",
        source: { kind: "official" },
        version: "580.178.04",
        created_at: 1000,
        finished_at: null,
      },
    ];
    body["active_task"] = status === "RUNNING" ? { task_id: "T-THEME-0001", status } : null;
    await route.fulfill({ response, json: body });
  });

  await page.goto("/");
  await signIn(page);
  await page.goto("/#/tasks");
  await page.locator("table.tasks a").first().click();
  await expect(page.locator(".log-tail")).toBeVisible();
  const tasksLink = page.locator(".app-nav a[data-route='#/tasks']");
  const state = tasksLink.locator(".nav-task-state");
  await expect(state).toBeAttached();
  expect((await state.textContent()) ?? "").not.toBe("");

  // The theme switch must not restart polling, reset the log scroll or redraw the page. The scroll is
  // asserted non-zero first: 0 → 0 proves nothing.
  await page.locator(".log-tail").evaluate((node) => {
    (node as HTMLElement).dataset["probe"] = "alive";
    node.scrollTop = node.scrollHeight;
  });
  const scrollBefore = await page.locator(".log-tail").evaluate((node) => node.scrollTop);
  expect(scrollBefore).toBeGreaterThan(0);
  const callsBefore = detailCalls.length;
  const allRequests: string[] = [];
  page.on("request", (r) => allRequests.push(r.url()));
  await changeThemeProgrammatically(page, "plate-dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "plate-dark");
  await expect(page.locator('.log-tail[data-probe="alive"]')).toHaveCount(1);
  expect(await page.locator(".log-tail").evaluate((node) => node.scrollTop)).toBe(scrollBefore);
  await page.waitForTimeout(4500);
  const callsAfterSwitch = detailCalls.length - callsBefore;
  expect(callsAfterSwitch).toBeGreaterThanOrEqual(1);
  expect(callsAfterSwitch).toBeLessThanOrEqual(2); // 4 s cadence, no burst
  // The switch itself added no request; the only requests in the window are the task-detail polls
  // already counted above.
  expect(allRequests.filter((u) => !u.includes("/api/tasks/T-THEME"))).toEqual([]);

  // UNKNOWN (state-read-error notice) is not a confirmed running state: the dot must clear, and return
  // only when the real RUNNING status comes back.
  status = "UNKNOWN";
  await expect(state).toHaveText("", { timeout: 6000 });
  status = "RUNNING";
  await expect(state).toHaveText(/Task in progress|有正在进行的任务/, { timeout: 6000 });
  // When the confirmed state reaches its terminal value the state text and the dot clear along the
  // same existing response.
  status = "SUCCEEDED";
  await expect(state).toHaveText("", { timeout: 6000 });
  await expect(page.locator(".app-nav .nav-dot")).toBeHidden();
});

test("a historical task's terminal task-detail cannot clear another task's global dot", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  const detailStatus: Record<string, string> = {
    "T-A-HISTORY-0001": "SUCCEEDED",
    "T-B-GLOBAL-0002": "RUNNING",
  };
  let bFails = false;
  await page.route("**/api/tasks/T-*", async (route) => {
    const url = route.request().url();
    const id = url.includes("T-B-GLOBAL") ? "T-B-GLOBAL-0002" : "T-A-HISTORY-0001";
    if (id === "T-B-GLOBAL-0002" && bFails) {
      await route.fulfill({
        status: 500,
        contentType: "application/json",
        json: { error: "internal", params: {} },
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      json: themeDetailBody(id, detailStatus[id] ?? "RUNNING"),
    });
  });
  await page.route("**/api/tasks", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    body["tasks"] = [
      {
        task_id: "T-A-HISTORY-0001",
        status: "SUCCEEDED",
        phase: "check",
        source: { kind: "official" },
        version: "580.173.02",
        created_at: 900,
        finished_at: 1000,
      },
      {
        task_id: "T-B-GLOBAL-0002",
        status: "RUNNING",
        phase: "download",
        source: { kind: "official" },
        version: "580.178.04",
        created_at: 1100,
        finished_at: null,
      },
    ];
    body["active_task"] = { task_id: "T-B-GLOBAL-0002", status: "RUNNING" };
    await route.fulfill({ response, json: body });
  });

  await page.goto("/");
  await signIn(page);
  await page.goto("/#/tasks");
  await expect(page.locator("table.tasks a").first()).toBeVisible();
  const state = page.locator(".app-nav a[data-route='#/tasks'] .nav-task-state");
  await expect(state).toHaveText(/Task in progress|有正在进行的任务/);

  await page.locator("table.tasks a").first().click();
  await expect(page.locator(".log-tail")).toBeVisible();
  await page.waitForTimeout(2500);
  await expect(state).toHaveText(/Task in progress|有正在进行的任务/);

  await page.goto("/#/tasks");
  await expect(page.locator("table.tasks a").first()).toBeVisible();
  await page.locator("table.tasks a").nth(1).click();
  await expect(page.locator(".log-tail")).toBeVisible();
  await expect(state).toHaveText(/Task in progress|有正在进行的任务/, { timeout: 6000 });

  // A failed read stops confirming this task as RUNNING; a recovered read brings it back. A global
  // claim about another task survives all of it.
  bFails = true;
  await expect(state).toHaveText("", { timeout: 6000 });
  bFails = false;
  await expect(state).toHaveText(/Task in progress|有正在进行的任务/, { timeout: 6000 });
});

test("the task page's own task id decides whose running claim survives", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  const detailStatus: Record<string, string> = {
    "T-B-DIRECT-0001": "SUCCEEDED",
    "T-A-RUNNING-0002": "RUNNING",
    "T-C-HISTORY-0003": "SUCCEEDED",
  };
  await page.route("**/api/tasks/T-*", async (route) => {
    const url = route.request().url();
    const id =
      ["T-B-DIRECT-0001", "T-A-RUNNING-0002", "T-C-HISTORY-0003"].find((k) =>
        url.includes(k),
      ) ?? "T-B-DIRECT-0001";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      json: themeDetailBody(id, detailStatus[id] ?? "RUNNING"),
    });
  });
  await page.route("**/api/tasks", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    body["tasks"] = [
      {
        task_id: "T-B-DIRECT-0001",
        status: "RUNNING",
        phase: "download",
        source: { kind: "official" },
        version: "580.178.04",
        created_at: 1100,
        finished_at: null,
      },
      {
        task_id: "T-C-HISTORY-0003",
        status: "SUCCEEDED",
        phase: "check",
        source: { kind: "official" },
        version: "580.173.02",
        created_at: 900,
        finished_at: 1000,
      },
    ];
    body["active_task"] = { task_id: "T-B-DIRECT-0001", status: "RUNNING" };
    await route.fulfill({ response, json: body });
  });

  await page.goto("/");
  await signIn(page);
  await page.goto("/#/tasks");
  await expect(page.locator("table.tasks a").first()).toBeVisible();
  const state = page.locator(".app-nav a[data-route='#/tasks'] .nav-task-state");
  await expect(state).toHaveText(/Task in progress|有正在进行的任务/);

  // Case 1: B's own detail already says SUCCEEDED. The global claim was about B, so entering B clears
  // it even though the first read is not RUNNING.
  await page.locator("table.tasks a").first().click();
  await expect(page.locator(".log-tail")).toBeVisible();
  await expect(state).toHaveText("", { timeout: 6000 });

  // Case 2: A's own RUNNING detail claims A; opening historical C afterwards must not erase A's claim
  // (C is a different task id).
  await page.goto("/#/tasks/T-A-RUNNING-0002");
  await expect(page.locator(".log-tail")).toBeVisible();
  await expect(state).toHaveText(/Task in progress|有正在进行的任务/, { timeout: 6000 });
  await page.goto("/#/tasks/T-C-HISTORY-0003");
  await expect(page.locator(".log-tail")).toBeVisible();
  await page.waitForTimeout(2500);
  await expect(state).toHaveText(/Task in progress|有正在进行的任务/);
});

test("a live task page's terminal login status follow-up cannot overwrite a newer session state", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  let loginReadCount = 0;
  await page.route("**/api/login", async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    // Fetch immediately: at that moment the session is whatever it really is.
    const response = await route.fetch();
    loginReadCount += 1;
    // The third login status read is the task page's terminal follow-up; hold its already-fetched
    // result so it settles after the sign-out below.
    if (loginReadCount === 3) await new Promise((r) => setTimeout(r, 2000));
    await route.fulfill({ response });
  });
  await page.route("**/api/tasks/T-TERM*", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      json: themeDetailBody("T-TERM-0001", "SUCCEEDED"),
    });
  });
  await page.route("**/api/tasks", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    body["tasks"] = [
      {
        task_id: "T-TERM-0001",
        status: "SUCCEEDED",
        phase: "check",
        source: { kind: "official" },
        version: "580.173.02",
        created_at: 900,
        finished_at: 1000,
      },
    ];
    body["active_task"] = null;
    await route.fulfill({ response, json: body });
  });

  await page.goto("/");
  await signIn(page);
  await page.goto("/#/tasks");
  await page.locator("table.tasks a").first().click();
  await expect(page.locator(".log-tail")).toBeVisible();
  // The terminal state fired its login status follow-up (held in flight). Sign out while it is
  // pending: the newer read wins the global state.
  await page.getByRole("button", { name: /Sign out|退出登录/ }).click();
  await expect(page.locator("#login-user")).toBeVisible();
  await page.waitForTimeout(2500); // the held login-status read settles here
  // A stale signed-in result must not sign this browser back in.
  await expect(page.locator("#login-user")).toBeVisible();
  await page.goto("/#/home");
  await expect(page.locator("#login-user")).toBeVisible();
});

test("a late home response cannot revive the dot after the page has moved on", async ({
  page,
  request,
}) => {
  await setScenario(request, "fresh");
  let release: (() => void) | null = null;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/home", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    body["active_task"] = { task_id: "T-LATE-0001", status: "RUNNING" };
    await gate; // hold every home response until the page has navigated away
    await route.fulfill({ response, json: body });
  });
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/drivers");
  await expect(page.locator("table.drivers")).toBeVisible();
  release?.();
  await page.waitForTimeout(1500);
  const tasksLink = page.locator(".app-nav a[data-route='#/tasks']");
  await expect(tasksLink.locator(".nav-dot")).toBeHidden();
  await expect(tasksLink.locator(".nav-task-state")).toHaveText("");
});

test("all four themes render the sign-in, password and key routes plus a native dialog", async ({
  page,
  request,
}) => {
  test.setTimeout(180_000);
  for (const theme of THEME_VALUES) {
    await setScenario(request, "fresh");
    await storeTheme(page, theme);
    await page.goto("/");
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await expect(page.locator("#theme-control")).toHaveValue(theme);
    await signIn(page);
    await expect(page.locator(".readings .reading").first()).toBeVisible();
    for (const { route, selector, text } of [
      { route: "update", selector: "#version-select" },
      { route: "drivers", selector: ".app-main", text: /Local drivers|本地驱动/ },
      { route: "tasks", selector: ".app-main h2", text: /Tasks|任务/ },
    ]) {
      await page.goto(`/#/${route}`);
      await expect(page.locator(selector)).toBeVisible();
      if (text) await expect(page.locator(selector)).toContainText(text);
    }

    await page.getByRole("link", { name: /Account|账户/ }).click();
    await expect(page.locator("#pass-new")).toBeVisible();

    // The pre-update check or confirm dialog follows the theme and keeps the native Escape and
    // focus-return contract.
    await page.goto("/#/update");
    await page.locator("#version-select").selectOption({ index: 0 });
    await page.getByRole("button", { name: "Check and continue", exact: true }).click();
    const dialog = page.locator("dialog.nvm-dialog");
    await expect(dialog).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(page.evaluate(() => document.activeElement?.textContent)).resolves.toBe("Check and continue");

    await page.getByRole("button", { name: /Sign out|退出登录/ }).click();
    await expect(page.locator("#login-user")).toBeVisible();
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
  }
});

test("capture the official theme screenshots from the real app", async ({ page, request }) => {
  test.skip(process.env["NVM_E2E_SCREENSHOTS"] !== "1", "Set NVM_E2E_SCREENSHOTS=1 to capture screenshots");
  test.setTimeout(240_000);
  mkdirSync(SHOT_DIR, { recursive: true });
  for (const theme of THEME_VALUES) {
    await setScenario(request, "fresh");
    await storeTheme(page, theme);
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/");
    await signIn(page);
    await page.goto("/#/home");
    await expect(page.locator(".readings .reading").first()).toBeVisible();
    await page.waitForTimeout(300);
    await page.screenshot({ path: join(SHOT_DIR, `desktop-${theme}-home.png`), fullPage: true });

    // The update page only counts as loaded when the real catalog options have arrived; a
    // visible-but-empty select is not a loaded page. The screenshot shows the page mid-flow: a version
    // is chosen and the primary button hierarchy is real.
    await page.goto("/#/update");
    await page.waitForFunction(() => (document.getElementById("version-select")?.options.length ?? 0) > 0);
    await page.locator("#version-select").selectOption({ index: 0 });
    await page.waitForTimeout(300);
    await page.screenshot({ path: join(SHOT_DIR, `desktop-${theme}-update.png`), fullPage: true });

    await page.locator("#lang-control").selectOption("zh");
    await page.waitForFunction(() => (document.getElementById("version-select")?.options.length ?? 0) > 0);
    await page.waitForTimeout(300);
    await page.screenshot({ path: join(SHOT_DIR, `desktop-${theme}-update-zh.png`), fullPage: true });
    await page.locator("#lang-control").selectOption("en");

    // The native confirm/check dialog in this theme. The language switches above re-rendered the page
    // and the deliberate no-auto-select contract resets the choice, so pick the version again for this
    // step.
    await page.waitForFunction(() => (document.getElementById("version-select")?.options.length ?? 0) > 0);
    await page.locator("#version-select").selectOption({ index: 0 });
    await page.getByRole("button", { name: "Check and continue", exact: true }).click();
    const dialog = page.locator("dialog.nvm-dialog");
    await expect(dialog).toBeVisible();
    await page.waitForTimeout(200);
    await page.screenshot({ path: join(SHOT_DIR, `desktop-${theme}-dialog.png`) });
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);

    await page.setViewportSize({ width: 390, height: 844 });

    await page.goto("/#/update");
    await page.waitForFunction(() => (document.getElementById("version-select")?.options.length ?? 0) > 0);
    await page.getByRole("button", { name: /Upload files manually…|手动上传文件…/ }).click();
    const upload = page.locator("dialog[open]");
    await expect(upload).toBeVisible();
    await page.waitForTimeout(200);
    await page.screenshot({ path: join(SHOT_DIR, `mobile-${theme}-upload.png`) });
    await upload.locator(".upload-footer").getByRole("button", { name: /Close|关闭/ }).click();
    await expect(upload).toHaveCount(0);

    await page.locator("#version-select").selectOption({ index: 0 });
    await page.getByRole("button", { name: "Check and continue", exact: true }).click();
    const confirmDialog = page.locator("dialog.nvm-dialog");
    await expect(confirmDialog).toBeVisible();
    await page.waitForTimeout(200);
    await page.screenshot({ path: join(SHOT_DIR, `mobile-${theme}-confirm.png`) });
    await page.keyboard.press("Escape");
    await expect(confirmDialog).toHaveCount(0);

    await page.goto("/#/home");
    await expect(page.locator(".readings .reading").first()).toBeVisible();
    await page.waitForTimeout(300);
    await page.screenshot({ path: join(SHOT_DIR, `mobile-${theme}-home.png`), fullPage: true });
  }

  for (const theme of THEME_VALUES) {
    for (const name of [
      `desktop-${theme}-home.png`,
      `desktop-${theme}-update.png`,
      `desktop-${theme}-update-zh.png`,
      `desktop-${theme}-dialog.png`,
      `mobile-${theme}-home.png`,
      `mobile-${theme}-upload.png`,
      `mobile-${theme}-confirm.png`,
    ]) {
      const size = statSync(join(SHOT_DIR, name)).size;
      expect(size, name).toBeGreaterThan(10_000);
    }
  }
});
