import { describe, expect, it } from "vitest";
import {
  buildBatchCreateTransaction,
  MAX_BATCH_SIZE,
  parseCsv,
  submitBatchWithRecovery,
  type SubmitOutcome,
  type ParsedBatchStream,
} from "./batch_create.ts";

const HEADER = "recipient,amount,duration_days,cliff_days";
const OPTIONS = {
  sender: "sender",
  token: "token",
  startTime: 1_700_000_000,
  cancellable: true,
  pausable: true,
  transferable: true,
};

function csvRow(recipient = "recipient", amount = "86400", duration = "1", cliff = "0") {
  return `${recipient},${amount},${duration},${cliff}`;
}

function parsedRow(overrides: Partial<ParsedBatchStream> = {}): ParsedBatchStream {
  return {
    recipient: "recipient",
    amount: 86_400n,
    durationDays: 1,
    cliffDays: 0,
    ...overrides,
  };
}

describe("batch stream creation", () => {
  it("parses reordered columns and preserves a zero-day cliff", () => {
    const rows = parseCsv(
      "cliff_days,duration_days,amount,recipient\n0,1,86400,recipient",
    );

    expect(rows).toEqual([parsedRow()]);
    expect(buildBatchCreateTransaction(rows, OPTIONS).operations[0].createStream).toMatchObject({
      startTime: OPTIONS.startTime,
      cliffTime: OPTIONS.startTime,
      endTime: OPTIONS.startTime + 86_400,
    });
  });

  it("accepts an exact i128 deposit and duration product boundary", () => {
    const maxDepositForOneDay = ((1n << 127n) - 1n) / 86_400n;
    const rows = parseCsv(
      `${HEADER}\n${csvRow("recipient", maxDepositForOneDay.toString(), "1", "0")}`,
    );

    expect(rows[0].amount * 86_400n).toBeLessThanOrEqual((1n << 127n) - 1n);
  });

  it.each([
    ["amount outside i128", csvRow("recipient", "170141183460469231731687303715884105728")],
    ["unsafe duration", csvRow("recipient", "86400", "9007199254740992")],
    ["non-numeric cliff", csvRow("recipient", "86400", "1", "NaN")],
    ["empty recipient", csvRow(" ", "86400")],
    ["deposit rate too low", csvRow("recipient", "86399")],
    ["accrual product overflow", csvRow("recipient", "170141183460469231731687303715884105727")],
  ])("rejects %s during CSV parsing", (_case, row) => {
    expect(() => parseCsv(`${HEADER}\n${row}`)).toThrow();
  });

  it("rejects more than the on-chain batch limit while parsing", () => {
    const rows = Array.from({ length: MAX_BATCH_SIZE + 1 }, () => csvRow()).join("\n");

    expect(() => parseCsv(`${HEADER}\n${rows}`)).toThrow(/MAX_BATCH_SIZE/);
  });

  it("revalidates direct builder input and checks timestamp overflow", () => {
    expect(() => buildBatchCreateTransaction([parsedRow({ durationDays: 0 })], OPTIONS)).toThrow(
      /duration_days/,
    );
    expect(() =>
      buildBatchCreateTransaction([parsedRow()], { ...OPTIONS, startTime: Number.MAX_SAFE_INTEGER }),
    ).toThrow(/timestamp/);
  });

  it("constructs a full batch in input order", () => {
    const rows = Array.from({ length: MAX_BATCH_SIZE }, (_, index) =>
      parsedRow({ recipient: `recipient-${index}` }),
    );
    const transaction = buildBatchCreateTransaction(rows, OPTIONS);

    expect(transaction.operations).toHaveLength(MAX_BATCH_SIZE);
    expect(transaction.operations.map(({ createStream }) => createStream.recipient)).toEqual(
      rows.map(({ recipient }) => recipient),
    );
  });
});

describe("buildBatchCreateTransaction option and row errors", () => {
  it("rejects empty and oversized row sets and bad startTime", () => {
    expect(() => buildBatchCreateTransaction([], OPTIONS)).toThrow(/No rows/);
    const many = Array.from({ length: MAX_BATCH_SIZE + 1 }, () => parsedRow());
    expect(() => buildBatchCreateTransaction(many, OPTIONS)).toThrow(/MAX_BATCH_SIZE/);
    expect(() => buildBatchCreateTransaction([parsedRow()], { ...OPTIONS, startTime: -1 })).toThrow(/startTime/);
  });

  it("rejects cliff beyond duration, bad CSV shape, header problems", () => {
    expect(() => buildBatchCreateTransaction([parsedRow({ cliffDays: 2 })], OPTIONS)).toThrow(/cliff_days/);
    expect(() => parseCsv("")).toThrow(/header/);
    expect(() => parseCsv("recipient,amount\nr,1")).toThrow(/Missing required/);
    expect(() => parseCsv(`${HEADER},amount\n${csvRow()},1`)).toThrow(/duplicate/);
    expect(() => parseCsv(`${HEADER}\nr,86400,1`)).toThrow(/columns/);
    expect(() => parseCsv(`${HEADER}\nr,abc,1,0`)).toThrow(/integer/);
  });
});

describe("submitBatchWithRecovery", () => {
  const sleep = async () => {};
  function deps(outcomes: SubmitOutcome[], counts: Array<[bigint, number]>, restore = async () => {}) {
    let s = 0;
    let c = 0;
    return {
      submitCalls: () => s,
      d: {
        submit: async () => outcomes[s++],
        readStreamCount: async () => {
          const [streamCount, ledger] = counts[Math.min(c++, counts.length - 1)];
          return { streamCount, ledger };
        },
        restore,
        sleep,
      },
    };
  }

  it("succeeds first try", async () => {
    const { d } = deps([{ status: "SUCCESS" }], [[10n, 1]]);
    expect(await submitBatchWithRecovery(2, d)).toEqual({ attempts: 1, restored: false, alreadyLanded: false });
  });

  it("restores archived entries then retries the same tx", async () => {
    let restores = 0;
    const { d, submitCalls } = deps([{ status: "ARCHIVED" }, { status: "SUCCESS" }], [[0n, 1]], async () => {
      restores++;
    });
    const r = await submitBatchWithRecovery(1, d);
    expect(r).toMatchObject({ attempts: 2, restored: true });
    expect(restores).toBe(1);
    expect(submitCalls()).toBe(2);
  });

  it("does not resubmit a timed-out batch that already landed", async () => {
    const { d, submitCalls } = deps([{ status: "TIMEOUT", ledger: 5 }], [[10n, 1], [10n, 4], [12n, 5]]);
    const r = await submitBatchWithRecovery(2, d);
    expect(r.alreadyLanded).toBe(true);
    expect(submitCalls()).toBe(1);
  });

  it("resubmits when a timeout did not land", async () => {
    const { d } = deps([{ status: "TRY_AGAIN_LATER", ledger: 5 }, { status: "SUCCESS" }], [[10n, 1], [10n, 6]]);
    expect(await submitBatchWithRecovery(2, d)).toMatchObject({ attempts: 2, alreadyLanded: false });
  });

  it("fails on persistent RPC skew, partial delta, chain failure, exhaustion, bad size", async () => {
    await expect(
      submitBatchWithRecovery(1, deps([{ status: "TIMEOUT", ledger: 9 }], [[0n, 1]]).d, { maxSkewReads: 2 }),
    ).rejects.toThrow(/read skew/);
    await expect(
      submitBatchWithRecovery(2, deps([{ status: "TIMEOUT", ledger: 1 }], [[0n, 1], [1n, 2]]).d),
    ).rejects.toThrow(/delta/);
    await expect(submitBatchWithRecovery(1, deps([{ status: "FAILED" }], [[0n, 1]]).d)).rejects.toThrow(/failed on chain/);
    await expect(
      submitBatchWithRecovery(1, deps([{ status: "ARCHIVED" }, { status: "ARCHIVED" }], [[0n, 1]]).d, { maxAttempts: 2 }),
    ).rejects.toThrow(/not confirmed/);
    await expect(submitBatchWithRecovery(0, deps([], [[0n, 1]]).d)).rejects.toThrow(/batchSize/);
  });
});
