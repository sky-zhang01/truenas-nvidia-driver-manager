import { test } from "node:test";
import assert from "node:assert/strict";
import { renderLogin } from "../src/views/login.js";
import { FakeElement, installDom } from "./dom-shim.js";

test("all account reset help uses a YAML data-folder placeholder and ignores host paths", () => {
  installDom();
  for (const lang of ["zh", "en"] as const) {
    for (const show_reset_help of [false, true]) {
      const container = new FakeElement("main");
      const login = {
        logged_in: false, setup: { problems: [] }, credentials: "ok", show_reset_help,
        get credentials_reset_path(): never { throw new Error("host paths must never be consumed"); },
      };
      renderLogin({ lang, login } as never, container as never);
      assert.match(container.textContent, lang === "zh"
        ? /<数据目录>\/manager\/secrets\/web-credentials\.json/
        : /<data directory>\/manager\/secrets\/web-credentials\.json/);
      assert.match(container.textContent, /YAML/);
      assert.match(container.textContent, lang === "zh" ? /数据目录.*YAML/ : /data directory.*YAML/);
    }
  }
});
