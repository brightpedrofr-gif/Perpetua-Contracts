import { describe, expect, it } from "vitest";
import { InvariantViolation, MAX_STREAMS, summarize, vestedPct, type DashStream } from "./streamMath";

const base: DashStream = { id: 1, recipient: "G", deposited: 100, vested: 50, ratePerSec: 1, status: "active" };
const code = (fn: () => unknown) => {
  try { fn(); } catch (e) { return (e as InvariantViolation).code; }
  return null;
};

describe("streamMath invariants", () => {
  it("summarizes valid rows", () => {
    expect(summarize([base, { ...base, id: 2, status: "paused" }])).toEqual({ active: 1, totalVested: 100, totalDeposited: 200 });
  });
  it("rejects degenerate rows with deterministic codes", () => {
    expect(code(() => summarize([{ ...base, deposited: 0, vested: 0 }]))).toBe("ZERO_DEPOSIT");
    expect(code(() => summarize([{ ...base, vested: 101 }]))).toBe("VESTED_EXCEEDS_DEPOSIT");
    expect(code(() => summarize([{ ...base, vested: -1 }]))).toBe("NEGATIVE_VALUE");
    expect(code(() => summarize([{ ...base, ratePerSec: NaN }]))).toBe("NON_FINITE");
    expect(code(() => summarize([{ ...base, deposited: Infinity }]))).toBe("NON_FINITE");
    expect(code(() => summarize([{ ...base, deposited: 2 ** 60, vested: 0 }]))).toBe("UNSAFE_INTEGER");
  });
  it("rejects aggregate overflow and oversized batches", () => {
    const big = Number.MAX_SAFE_INTEGER - 1;
    expect(code(() => summarize([{ ...base, deposited: big, vested: 0 }, { ...base, id: 2, deposited: 10, vested: 0 }]))).toBe("UNSAFE_INTEGER");
    expect(code(() => summarize(new Array(MAX_STREAMS + 1).fill(base)))).toBe("BATCH_TOO_LARGE");
    expect(summarize(new Array(MAX_STREAMS).fill(base)).active).toBe(MAX_STREAMS);
  });
  it("vestedPct never yields NaN/Infinity and stays in [0,100]", () => {
    expect(vestedPct(5, 0)).toBe(0);
    expect(vestedPct(NaN, 10)).toBe(0);
    expect(vestedPct(-5, 10)).toBe(0);
    expect(vestedPct(500, 10)).toBe(100);
    expect(vestedPct(5, 10)).toBe(50);
    for (let i = 0; i < 200; i++) {
      const p = vestedPct(Math.random() * 1e6 - 1e3, Math.random() * 1e6);
      expect(p).toBeGreaterThanOrEqual(0);
      expect(p).toBeLessThanOrEqual(100);
    }
  });
});
