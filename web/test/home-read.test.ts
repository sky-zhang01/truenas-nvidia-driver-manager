import { test } from "node:test";
import assert from "node:assert/strict";

import { HomeRead } from "../src/home-read.js";

test("the caller handles a rejected home read", async () => {
  const unhandled: unknown[] = [];
  const listener = (reason: unknown): void => {
    unhandled.push(reason);
  };
  process.on("unhandledRejection", listener);
  try {
    const reads = new HomeRead<string>();
    const pending = reads.start(
      () => true,
      () => Promise.reject(new Error("boom")),
      () => {},
    );
    let rejected = false;
    try {
      await pending;
    } catch (err) {
      rejected = true;
      assert.match(String(err), /boom/);
    }
    assert.equal(rejected, true);
    await Promise.resolve();
    assert.deepEqual(unhandled, []);
  } finally {
    process.off("unhandledRejection", listener);
  }
});

test("a new epoch does not join or keep the previous read", async () => {
  const reads = new HomeRead<string>();
  let releaseFirst: (value: string) => void = () => {};
  const first = reads.start(
    () => true,
    () =>
      new Promise((resolve) => {
        releaseFirst = resolve;
      }),
    () => {},
  );
  reads.invalidate();
  let releaseSecond: (value: string) => void = () => {};
  const second = reads.start(
    () => true,
    () =>
      new Promise((resolve) => {
        releaseSecond = resolve;
      }),
    () => {},
  );
  assert.notEqual(first, second);
  releaseFirst("old");
  assert.equal(await first, "old");
  assert.equal(reads.snapshot, null);
  releaseSecond("new");
  assert.equal(await second, "new");
  assert.equal(reads.snapshot, "new");
});

test("an old rejection does not reject the read started after it", async () => {
  const reads = new HomeRead<string>();
  let rejectFirst: (err: Error) => void = () => {};
  const first = reads.start(
    () => true,
    () =>
      new Promise((_, reject) => {
        rejectFirst = reject;
      }),
    () => {},
  );
  reads.invalidate();
  const second = reads.start(
    () => true,
    () => Promise.resolve("new"),
    () => {},
  );
  let rejected = false;
  const caught = first.then(
    () => {},
    (err: unknown) => {
      rejected = true;
      assert.match(String(err), /old-401/);
    },
  );
  rejectFirst(new Error("old-401"));
  await caught;
  assert.equal(rejected, true);
  assert.equal(await second, "new");
  assert.equal(reads.snapshot, "new");
});
