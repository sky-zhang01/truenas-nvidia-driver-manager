import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { MESSAGES, messageKeys } from "../src/messages.js";

// Approved key and placeholder contract; copy changes must preserve the input fields consumed by UI.
const contract = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../test/fixtures/message-contract.json"), "utf8")) as Record<string, string[]>;
const placeholders = (text: string): string[] => [...new Set([...text.matchAll(/\{([A-Za-z_][A-Za-z0-9_]*)\}/g)].map((match) => match[1] as string))].sort();
const segments = (text: string): string[] => text.split("\n").map((line) => line.match(/^(?:(?:info|warning|error)\|)?(?:(?:title|body|action|list|note)\|)?/)?.[0] ?? "");

test("the approved catalog has every retained key and no deleted or merged aliases", () => {
  assert.equal(Object.keys(contract).length, 775);
  assert.deepEqual(messageKeys(), Object.keys(contract).sort());
});

test("approved placeholders survive rewriting and Chinese and English segments stay paired", () => {
  for (const [key, fields] of Object.entries(contract)) {
    const message = MESSAGES[key];
    assert.ok(message, key);
    for (const lang of ["zh", "en"] as const) {
      assert.ok(message[lang].trim(), `${key}.${lang} is nonempty`);
      assert.deepEqual(placeholders(message[lang]), fields, `${key}.${lang} placeholders`);
    }
    assert.deepEqual(segments(message.zh), segments(message.en), `${key} segment pairing`);
  }
});

test("interface copy excludes rhetorical questions, contrastive claims and emphasis", () => {
  for (const [key, message] of Object.entries(MESSAGES)) {
    // Backtick literals and link targets may contain punctuation that is not interface prose.
    const prose = (value: string): string => value.replace(/`[^`]*`/g, "").replace(/\[\[([^|]+)\|[^\]]+\]\]/g, "$1");
    assert.equal(/不是[^。\n]*而是|并非|永远|一定|务必|千万|完全|请只|[？?]|这不是出错/.test(prose(message.zh)), false, `${key}.zh forbidden sentence`);
    assert.equal(/rather than|\bplease\b|\bthis program\b|\?/i.test(prose(message.en)), false, `${key}.en forbidden sentence`);
  }
});

test("unreadable credentials retain an actionable account reset instruction", () => {
  assert.ok(MESSAGES["credentials.unreadable"]!.zh.includes("请重置账号"));
  assert.ok(MESSAGES["credentials.unreadable"]!.en.includes("Reset the account."));
});


test("both original-driver choices name what is saved and explain the failure consequence once", () => {
  assert.equal(MESSAGES["confirm.save"]!.zh, "保存原版驱动并开始");
  assert.equal(MESSAGES["confirm.skip"]!.zh, "不保存原版驱动并开始");
  assert.equal(MESSAGES["confirm.save"]!.en, "Save original driver and start");
  assert.equal(MESSAGES["confirm.skip"]!.en, "Start without saving original driver");
  assert.equal(MESSAGES["original.skip_warning"]!.zh, "warning|不保存时，本次更新失败将没有原版驱动可恢复。");
  assert.equal(MESSAGES["original.skip_warning"]!.en, "warning|If you skip saving and this update fails, there is no original driver to restore.");
});

test("official catalog and footer mention manual upload only in the catalog guidance", () => {
  assert.equal(MESSAGES["catalog.help"]!.zh, "info|列表为各分支在本机可构建的最新版本。离线或自备文件请用「手动上传文件…」。");
  assert.equal(MESSAGES["catalog.help"]!.en, 'info|The list shows the latest version of each branch that builds on this host. For offline or your own files, use "Upload files manually…".');
  assert.equal(MESSAGES["upload.entry.hint"]!.zh, "选好版本后，程序从 NVIDIA 下载并校验。");
  assert.equal(MESSAGES["upload.entry.hint"]!.en, "After you pick a version, the program downloads it from NVIDIA and checks it.");
  assert.equal(Object.hasOwn(MESSAGES, "catalog.help_upload"), false);
});

test("read failures use one Chinese term while uncertain usage remains a separate state", () => {
  for (const [key, message] of Object.entries(MESSAGES)) {
    assert.equal(/读不到|读不出|使用情况/.test(message.zh), false, key);
  }
  assert.equal(MESSAGES["gpu.popup.title_unreadable"]!.zh, "无法更新：无法读取显卡使用状态");
  assert.equal(MESSAGES["gpu.list.loading"]!.zh, "正在读取显卡使用状态…");
  assert.equal(MESSAGES["gpu.badge.unknown"]!.zh, "无法判断");
  assert.equal(MESSAGES["gpu.badge.unknown.hint"]!.zh, "无法判断是否在使用显卡，建议一并停止。");
});
