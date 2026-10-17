// Only one read may be in flight, including the first read. Pause while hidden and refresh immediately
// when visible.

export const POLL_MS = 4000;

export interface Poller {

  tick(): Promise<void>;
  stop(): void;
}

// Callers own error display; rejected reads retain the previous display and allow the next tick to retry.
export function startPolling(
  intervalMs: number,
  read: () => Promise<void>,
): Poller {
  let stopped = false;
  let running = false;
  const doc: Document | null = typeof document === "undefined" ? null : document;
  let paused = doc !== null && doc.hidden === true;

  const tick = async (): Promise<void> => {
    if (stopped || running || paused) return;
    running = true;
    try {
      await read();
    } catch {

    } finally {
      running = false;
    }
  };

  const onVisibility = (): void => {
    paused = doc !== null && doc.hidden === true;
    if (!paused) void tick();
  };

  const timer = window.setInterval(() => {
    void tick();
  }, intervalMs);
  if (doc !== null && typeof doc.addEventListener === "function") {
    doc.addEventListener("visibilitychange", onVisibility);
  }

  return {
    tick,
    stop(): void {
      stopped = true;
      window.clearInterval(timer);
      if (doc !== null && typeof doc.removeEventListener === "function") {
        doc.removeEventListener("visibilitychange", onVisibility);
      }
    },
  };
}
