import { test } from "node:test";
import assert from "node:assert/strict";

import { DEFAULT_THEME, THEMES, detectTheme, isTheme } from "../src/theme.js";

test("the closed set is exactly the four official themes, focused family first", () => {
  // The one public order: the default first, then its dark sibling, then the plate family. The real
  // dropdown reads this array (main.ts).
  assert.deepEqual([...THEMES], ["focused-light", "focused-dark", "plate-light", "plate-dark"]);
});

test("the first-use default is focused-light", () => {
  assert.equal(DEFAULT_THEME, "focused-light");
});

test("the default theme is the first entry of the public order", () => {
  assert.equal(THEMES[0], DEFAULT_THEME);
});

test("isTheme accepts exactly the four themes", () => {
  for (const theme of THEMES) {
    assert.equal(isTheme(theme), true);
  }
  assert.equal(isTheme("system"), false);
  assert.equal(isTheme("auto"), false);
  assert.equal(isTheme("plate"), false);
  assert.equal(isTheme("PLATE-LIGHT"), false);
  assert.equal(isTheme(""), false);
  assert.equal(isTheme(undefined), false);
  assert.equal(isTheme(null), false);
  assert.equal(isTheme(42), false);
});

test("a stored choice wins when it is one of the four", () => {
  assert.equal(detectTheme("plate-light"), "plate-light");
  assert.equal(detectTheme("plate-dark"), "plate-dark");
  assert.equal(detectTheme("focused-light"), "focused-light");
  assert.equal(detectTheme("focused-dark"), "focused-dark");
});

test("an absent or non-theme stored value falls back to the default", () => {
  assert.equal(detectTheme(null), "focused-light");
  assert.equal(detectTheme(""), "focused-light");
  assert.equal(detectTheme("system-auto"), "focused-light");
  assert.equal(detectTheme("dark"), "focused-light");
  assert.equal(detectTheme("plate-light "), "focused-light");
});
