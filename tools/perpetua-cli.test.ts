import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import {
  buildCreateOperation,
  buildWithdrawOperation,
  buildExtendOperation,
  buildInspectQuery,
  classifyFailure,
  isArchivedStream,
  isLedgerSkewed,
  parsePositiveAmount,
  retryIdempotent,
  CLI_EVENT_PAYLOAD_SCHEMAS,
  CLI_EVENT_TOPIC_LAYOUTS,
  CliInvariantError,
  I128_MAX,
} from "./perpetua-cli.ts";

describe("perpetua-cli schema parity", () => {
  it("matches the withdrawn golden fixture", () => {
    const fx = JSON.parse(
      readFileSync(new URL("../tests/fixtures/event_snapshots/withdrawn.json", import.meta.url), "utf8"),
    );
    expect(CLI_EVENT_TOPIC_LAYOUTS.withdrawn).toEqual(fx.topic_layout);
    expect(CLI_EVENT_PAYLOAD_SCHEMAS.withdrawn).toEqual(fx.payload_schema);
  });
});

describe("perpetua-cli recovery", () => {
  it("detects archival and skew", () => {
    expect(isArchivedStream(false, 10n, 3)).toBe(true);
    expect(isArchivedStream(false, 3n, 3)).toBe(false);
    expect(isLedgerSkewed(99, 100)).toBe(true);
    expect(isLedgerSkewed(100, 100)).toBe(false);
    expect(classifyFailure("entry archived").kind).toBe("archived");
  });

  it("retries without double-submitting", async () => {
    let calls = 0;
    const r = await retryIdempotent(
      async () => {
        if (++calls < 2) throw new Error("TRY_AGAIN_LATER");
        return "ok";
      },
      async () => undefined,
    );
    expect(r).toBe("ok");
    expect(await retryIdempotent(async () => "again", async () => "landed")).toBe("landed");
    await expect(retryIdempotent(async () => { throw new Error("contract error"); }, async () => undefined)).rejects.toThrow();
  });
});

describe("perpetua-cli invariants", () => {
  const base = { recipient: "G", amount: "1000000000", durationDays: 1, startTime: 1000 };

  it("builds a valid create op", () => {
    const op = buildCreateOperation({ ...base, cliffDays: 1 });
    expect(op.createStream.endTime).toBe(1000 + 86_400);
    expect(op.createStream.cliffTime).toBe(op.createStream.endTime);
  });

  it("rejects zero/negative/fractional duration", () => {
    for (const d of [0, -1, 1.5, NaN]) {
      expect(() => buildCreateOperation({ ...base, durationDays: d })).toThrow(CliInvariantError);
    }
  });

  it("rejects cliff beyond duration", () => {
    expect(() => buildCreateOperation({ ...base, cliffDays: 2 })).toThrow(/InvalidCliff/);
    expect(() => buildCreateOperation({ ...base, cliffDays: -1 })).toThrow(/InvalidCliff/);
  });

  it("enforces i128::MAX amount cap and rejects malformed amounts", () => {
    expect(parsePositiveAmount(I128_MAX.toString())).toBe(I128_MAX);
    expect(() => parsePositiveAmount((I128_MAX + 1n).toString())).toThrow(/i128::MAX/);
    for (const a of ["0", "-5", "1.5", "", "0x10", " 1"]) {
      expect(() => parsePositiveAmount(a)).toThrow(CliInvariantError);
    }
  });

  it("rejects deposit below one unit per second", () => {
    expect(() => buildCreateOperation({ ...base, amount: "86399" })).toThrow(/DepositRateTooLow/);
    expect(() => buildCreateOperation({ ...base, amount: "86400" })).not.toThrow();
  });

  it("rejects invalid stream ids, withdraw amounts and ttl", () => {
    expect(() => buildWithdrawOperation({ streamId: -1 })).toThrow(/MalformedStreamId/);
    expect(() => buildInspectQuery({ streamId: 1.2 })).toThrow(/MalformedStreamId/);
    expect(() => buildWithdrawOperation({ streamId: 1, amount: "0" })).toThrow(/InvalidAmount/);
    expect(() => buildExtendOperation({ streamId: 1, ttlLedgers: 0 })).toThrow(/InvalidTtl/);
    expect(buildExtendOperation({ streamId: 1 }).extend.ttlLedgers).toBeUndefined();
  });

  it("property: valid random inputs never produce cliff > end or silent wrap", () => {
    for (let i = 0; i < 500; i++) {
      const dur = 1 + Math.floor(Math.random() * 3650);
      const cliff = Math.floor(Math.random() * (dur + 1));
      const op = buildCreateOperation({ ...base, durationDays: dur, cliffDays: cliff, amount: (BigInt(dur) * 86400n).toString() });
      expect(op.createStream.cliffTime).toBeLessThanOrEqual(op.createStream.endTime);
      expect(op.createStream.deposit).toBeGreaterThan(0n);
    }
  });

  it("bench: 10k builds stay within a fixed time cap (no per-call blowup)", () => {
    const t = performance.now();
    for (let i = 0; i < 10_000; i++) buildCreateOperation(base);
    expect(performance.now() - t).toBeLessThan(2000);
  });
});
