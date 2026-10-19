import { test, expect, type Locator, type Page } from "@playwright/test";
import { setScenario, signIn } from "./helpers.js";

const THEMES = ["focused-light", "focused-dark", "plate-light", "plate-dark"] as const;
const LANGUAGES = ["en", "zh"] as const;
const WIDTHS = [1440, 390] as const;

async function appearance(page: Page, theme: string, lang: string, width: number): Promise<void> {
  await page.setViewportSize({ width, height: 900 });
  await page.locator("#theme-control").selectOption(theme);
  await page.locator("#lang-control").selectOption(lang);
  await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
  await expect(page.locator("html")).toHaveAttribute("lang", lang === "zh" ? "zh-CN" : "en");
}

async function box(locator: Locator) {
  const result = await locator.boundingBox();
  expect(result).not.toBeNull();
  return result!;
}

async function pageFits(page: Page): Promise<void> {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)).toBe(true);
}

async function textLines(locator: Locator, breakAtHyphens = false) {
  return locator.evaluate((node, breakAtHyphens) => {
    const characters: { token: number; top: number }[] = [];
    const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
    let token = 0;
    let current: Node | null;
    while ((current = walker.nextNode())) {
      const text = current.textContent ?? "";
      for (let offset = 0; offset < text.length; offset += 1) {
        if (/\s/.test(text[offset]!)) {
          token += 1;
          continue;
        }
        const range = document.createRange();
        range.setStart(current, offset);
        range.setEnd(current, offset + 1);
        for (const rect of range.getClientRects()) {
          if (rect.width > 0 && rect.height > 0) characters.push({ token, top: rect.top });
        }
        if (breakAtHyphens && text[offset] === "-") token += 1;
      }
    }
    const countLines = (tops: number[]) => {
      const lines: number[] = [];
      for (const top of tops) {
        if (!lines.some((line) => Math.abs(line - top) <= 1)) lines.push(top);
      }
      return lines.length;
    };
    const tokens = [...new Set(characters.map((character) => character.token))];
    return {
      lineCount: countLines(characters.map((character) => character.top)),
      tokenLineCounts: tokens.map((value) => countLines(characters
        .filter((character) => character.token === value).map((character) => character.top))),
    };
  }, breakAtHyphens);
}

test("running task readings stay paired and explain unfinished work in every appearance", async ({ page, request }) => {
  test.slow();
  await setScenario(request, "running_task");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/tasks/20260928T101530Z-3f9a0c12");

  for (const theme of THEMES) {
    for (const lang of LANGUAGES) {
      for (const width of WIDTHS) {
        await appearance(page, theme, lang, width);
        const state = page.locator(".task-state");
        await expect(state).toHaveAttribute("role", "status");
        await expect(state.locator(":scope > .reading")).toHaveCount(2);
        for (const reading of await state.locator(":scope > .reading").all()) {
          const label = reading.locator(":scope > .reading-label");
          const value = reading.locator(":scope > .reading-value");
          await expect(label).toHaveCount(1);
          await expect(value).toHaveCount(1);
          await expect(value).not.toHaveText("");
          const labelBox = await box(label);
          const valueBox = await box(value);
          expect(valueBox.x >= labelBox.x + labelBox.width - 1
            || valueBox.y >= labelBox.y + labelBox.height - 1).toBe(true);
        }
        const finished = page.locator(".task-times > .reading").nth(1).locator(".reading-value");
        await expect(finished).toHaveText(lang === "en" ? "Not finished yet" : "尚未结束");
        await expect(page.locator(".task-times")).not.toContainText("UNKNOWN");
        await expect(page.locator(".task-details")).toHaveCount(0);
        await pageFits(page);
      }
    }
  }
});

test("driver columns and actions remain readable with an accessible local scroll area", async ({ page, request }) => {
  test.slow();
  await setScenario(request, "drivers_rows");
  await page.goto("/");
  await signIn(page);
  await page.goto("/#/drivers");

  // Relative tracking widens every font on every platform without requiring an installed face.
  for (const tracking of ["normal", "0.1em"]) {
    // Use the same-origin stylesheet's CSSOM so the app's strict CSP remains enabled.
    const styleIndex = await page.evaluate((value) => {
      const sheet = document.styleSheets[0]!;
      return sheet.insertRule(`.drivers, .drivers * { letter-spacing: ${value} !important; }`, sheet.cssRules.length);
    }, tracking);
    for (const theme of THEMES) {
      for (const lang of LANGUAGES) {
        for (const width of WIDTHS) {
          await appearance(page, theme, lang, width);
          const scroll = page.locator(".drivers-scroll");
          await expect(scroll).toHaveAttribute("role", "region");
          await expect(scroll).toHaveAttribute("tabindex", "0");
          await expect(scroll).toHaveAccessibleName(lang === "en" ? "Local drivers" : "本地驱动");
          await expect(scroll.locator("table.drivers")).toBeVisible();
          if (tracking !== "normal") {
            for (const target of [scroll.locator("table.drivers"), scroll.locator(".driver-actions button").first()]) {
              expect(await target.evaluate((node) => {
                const style = getComputedStyle(node);
                return Math.abs(parseFloat(style.letterSpacing) / parseFloat(style.fontSize) - 0.1);
              }), "font-width stress is applied to table text and controls").toBeLessThan(0.001);
            }
          }
          for (const cell of await scroll.locator("table.drivers th, table.drivers td").all()) {
            await expect(cell).toHaveCSS("overflow-wrap", "normal");
            expect(await cell.evaluate((node) => node.scrollWidth <= node.clientWidth + 1),
              `${theme}/${lang}/${width}/${tracking}: cell content fits`).toBe(true);
          }
          await expect(page.locator(".driver-version")).toHaveCount(4);
          await expect(page.locator(".driver-kernel")).toHaveText([
            "6.12.96-production+truenas",
            "6.12.105-production+truenas",
            "6.12.105-production+truenas",
            "6.12.105-production+truenas",
          ]);
          await expect(page.locator(".driver-actions-cell .driver-actions")).toHaveCount(4);
          for (const selector of [".driver-source", ".driver-size", ".driver-status", ".driver-keep"]) {
            await expect(scroll.locator(selector)).toHaveCount(4);
            for (const cell of await scroll.locator(selector).all()) {
              const lines = await textLines(cell);
              expect(lines.lineCount, `${theme}/${lang}/${width}: ${selector} stays on one line`).toBeLessThanOrEqual(1);
              if (selector !== ".driver-keep") expect(lines.lineCount).toBe(1);
              expect(await cell.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
            }
          }
          const keepHint = scroll.locator(".driver-keep .hint");
          await expect(keepHint).toHaveCount(1);
          const hintBox = await box(keepHint);
          const switchBox = await box(scroll.locator(".driver-keep input[role=switch]"));
          expect(Math.min(hintBox.y + hintBox.height, switchBox.y + switchBox.height)
            - Math.max(hintBox.y, switchBox.y), "the keep label shares the switch row").toBeGreaterThan(0);
          await expect(scroll.locator(".driver-date")).toHaveCount(4);
          for (const cell of await scroll.locator(".driver-date").all()) {
            await expect(cell).toHaveText(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
            const lines = await textLines(cell);
            expect(lines.lineCount, `${theme}/${lang}/${width}: date uses at most two lines`).toBeGreaterThan(0);
            expect(lines.lineCount).toBeLessThanOrEqual(2);
            expect(lines.tokenLineCounts.every((count) => count === 1), "date and time each stay intact").toBe(true);
          }
          for (const cell of await page.locator(".driver-version").all()) {
            expect((await textLines(cell)).lineCount, "versions stay intact").toBe(1);
            expect(await cell.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
          }
          for (const cell of await page.locator(".driver-kernel").all()) {
            const lines = await textLines(cell, true);
            expect(lines.lineCount).toBeGreaterThan(0);
            expect(lines.tokenLineCounts.every((count) => count === 1), "kernel segments only wrap after hyphens").toBe(true);
            expect(await cell.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
          }
          for (const actions of await page.locator(".driver-actions-cell .driver-actions").all()) {
            await expect(actions).toHaveCSS("display", "grid");
            const buttons = actions.locator("button");
            for (const button of await buttons.all()) {
              const size = await button.evaluate((node) => ({ text: node.textContent, scroll: node.scrollWidth, client: node.clientWidth }));
              expect(size.scroll, `${theme}/${lang}/${width}/${tracking}: ${size.text} fits ${size.client}px`).toBeLessThanOrEqual(size.client + 1);
            }
            if (await buttons.count() === 2) {
              const first = await box(buttons.nth(0));
              const second = await box(buttons.nth(1));
              expect(second.x >= first.x + first.width - 1
                || second.y >= first.y + first.height - 1).toBe(true);
            }
          }
          await pageFits(page);
          if (width === 1440) {
            expect(await scroll.evaluate((node) => node.scrollWidth <= node.clientWidth + 1),
              `${theme}/${lang}/${width}/${tracking}: wide table fits`).toBe(true);
          }
          if (width === 390) {
            expect((await box(scroll.locator("table.drivers"))).width).toBeGreaterThanOrEqual(1024);
            expect(await scroll.evaluate((node) => node.scrollWidth > node.clientWidth)).toBe(true);
            await scroll.evaluate((node) => { node.scrollLeft = 0; });
            await scroll.focus();
            await expect(scroll).toBeFocused();
            await page.keyboard.press("ArrowRight");
            await expect.poll(() => scroll.evaluate((node) => node.scrollLeft)).toBeGreaterThan(0);
          }
        }
      }
    }
    await page.evaluate((index) => document.styleSheets[0]!.deleteRule(index), styleIndex);
  }
});
