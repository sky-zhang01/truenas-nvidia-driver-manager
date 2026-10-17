import { test } from "node:test";
import assert from "node:assert/strict";

import { POLL_MS, startPolling } from "../src/poll.js";
import { installDom, domShim } from "./dom-shim.js";

installDom();

let timers: Array<() => void> = [];
(globalThis as unknown as { window: unknown }).window = {
  setInterval: (fn: () => void): number => {
    timers.push(fn);
    return timers.length;
  },
  clearInterval: (): void => {},
};

/** Let every pending microtask and the zero-delay timer run. */
const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

test("the interval is four seconds", () => {
  assert.equal(POLL_MS, 4000);
});

test("the first read goes through the same in-flight guard", async () => {
  timers = [];
  let calls = 0;
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const poller = startPolling(POLL_MS, async () => {
    calls += 1;
    await gate;
  });
  const first = poller.tick();
  // The timer fires while the first read is still in flight: it must not start a second request.
  for (const fire of timers) fire();
  await settle();
  assert.equal(calls, 1, "the timer ran a concurrent read");
  release();
  await first;
  assert.equal(calls, 1);
  poller.stop();
});

test("a hidden page pauses and coming back reads once immediately", async () => {
  timers = [];
  let calls = 0;
  const poller = startPolling(POLL_MS, async () => {
    calls += 1;
  });
  await poller.tick();
  assert.equal(calls, 1);

  domShim().setHidden(true);
  for (const fire of timers) fire();
  await settle();
  assert.equal(calls, 1, "a hidden page must not read");

  domShim().setHidden(false);
  await settle();
  assert.equal(calls, 2, "coming back reads once immediately");

  poller.stop();
  for (const fire of timers) fire();
  domShim().setHidden(true);
  domShim().setHidden(false);
  await settle();
  assert.equal(calls, 2, "a stopped poller never reads again");
  await poller.tick();
  assert.equal(calls, 2, "tick after stop is empty");
});
