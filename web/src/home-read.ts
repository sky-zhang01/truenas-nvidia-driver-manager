// Join only reads from the current epoch. Return the settling promise directly so a rejection is not
// duplicated on an unconsumed finally promise.

export class HomeRead<T> {
  snapshot: T | null = null;
  private inflight: Promise<T> | null = null;
  private inflightEpoch = -1;
  private epoch = 0;

  get currentEpoch(): number {
    return this.epoch;
  }

  // Invalidate the snapshot and prevent new callers from joining the in-flight read.
  invalidate(): void {
    this.epoch += 1;
    this.snapshot = null;
  }

  inFlight(): boolean {
    return this.inflight !== null && this.inflightEpoch === this.epoch;
  }

  start(signedIn: () => boolean, read: () => Promise<T>, onFresh: (value: T) => void): Promise<T> {
    if (this.inflight && this.inflightEpoch === this.epoch) return this.inflight;
    const epoch = this.epoch;
    const pending = read().then((value) => {
      if (epoch === this.epoch && signedIn()) {
        this.snapshot = value;
        onFresh(value);
      }
      return value;
    });
    let settled!: Promise<T>;
    settled = pending.finally(() => {
      if (this.inflight === settled) this.inflight = null;
    });
    this.inflight = settled;
    this.inflightEpoch = epoch;
    return settled;
  }
}
