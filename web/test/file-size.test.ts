import { test } from "node:test";
import assert from "node:assert/strict";
import { expandText, formatSize } from "../src/dom.js";

for (const [bytes, expected] of [
  [0, "0 KB"], [1, "1 KB"], [499, "1 KB"], [1000, "1 KB"],
  [1499, "1 KB"], [1500, "2 KB"], [20_804, "21 KB"],
  [999_499, "999 KB"], [999_500, "1.0 MB"], [999_999, "1.0 MB"],
  [1_000_000, "1.0 MB"], [1_150_000, "1.2 MB"], [1_236_052, "1.2 MB"], [1_501_496, "1.5 MB"],
  [5_993_348, "6.0 MB"], [6_350_000, "6.4 MB"], [9_949_999, "9.9 MB"],
  [9_950_000, "10 MB"], [9_999_999, "10 MB"], [10_000_000, "10 MB"],
  [10_450_000, "10 MB"], [10_499_999, "10 MB"], [10_500_000, "11 MB"], [316_187_987, "316 MB"],
] as const) {
  test(`decimal file size ${bytes} bytes reads ${expected}`, () => {
    assert.equal(formatSize(bytes), expected);
  });
}

test("size templates use the formatter's units exactly once in both languages", () => {
  for (const lang of ["en", "zh"] as const) {
    assert.equal(expandText("upload.receipt", { file_name: "tools.deb", size: formatSize(20_804) }, lang), "tools.deb · 21 KB");
    assert.equal(expandText("upload.progress.bytes", { done: formatSize(20_804), total: formatSize(1_236_052) }, lang), "21 KB / 1.2 MB");
    assert.equal(expandText("drivers.size_value", { size: formatSize(316_187_987) }, lang), "316 MB");
    assert.equal(expandText("download.progress", { done: formatSize(20_804), total: formatSize(316_187_987) }, lang),
      lang === "en" ? "Downloading 21 KB of 316 MB" : "正在下载 21 KB / 316 MB");
  }
});
