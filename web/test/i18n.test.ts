import { test } from "node:test";
import assert from "node:assert/strict";

import { detectLang, isLang } from "../src/i18n.js";

test("a stored choice wins over the browser languages", () => {
  assert.equal(detectLang("en", ["zh-CN"]), "en");
  assert.equal(detectLang("zh", ["en-US"]), "zh");
});

test("without a stored choice the first browser language decides", () => {
  assert.equal(detectLang(null, ["zh-CN", "en"]), "zh");
  assert.equal(detectLang(null, ["zh"]), "zh");
  assert.equal(detectLang(null, ["zh-Hant-TW"]), "zh");
  assert.equal(detectLang(null, ["en-US"]), "en");
  assert.equal(detectLang(null, ["de-DE", "zh-CN"]), "en");
  assert.equal(detectLang(null, []), "en");
});

test("a stored value that is not a language is ignored", () => {
  assert.equal(detectLang("fr", ["zh-CN"]), "zh");
  assert.equal(detectLang("", ["en-US"]), "en");
});

test("isLang accepts exactly the two languages", () => {
  assert.equal(isLang("zh"), true);
  assert.equal(isLang("en"), true);
  assert.equal(isLang("fr"), false);
  assert.equal(isLang(undefined), false);
});
