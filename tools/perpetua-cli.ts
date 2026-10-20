import { parse, buildBatchCreateTransaction, type BatchCreateTransaction, type ParsedBatchStream } from "./sdk/batch_create.ts";

export type CliCommand = "create" | "withdraw" | "inspect" | "extend";

export interface CliContext {
  contractId: string;
  rpcUrl: string;
  networkPassphrase: string;
  sourceSecret?: string;
}

export interface CreateCliInput {
  recipient: string;
  amount: string;
  durationDays: number;
  cliffDays?: number;
  startTime?: number;
  token?: string;
  cancellable?: boolean;
  pausable?: boolean;
  transferable?: boolean;
}

export interface WithdrawCliInput {
  streamId: number;
  amount?: string;
}

export interface InspectCliInput {
  streamId: number;
}

export interface ExtendCliInput {
  streamId: number;
  ttlLedgers?: number;
}

export interface PerpetuaCliResult {
  ok: boolean;
  output: string;
  error?: string;
}

/** i128::MAX, the largest deposit the contract accepts. */
export const I128_MAX = (1n << 127n) - 1n;
export const SECONDS_PER_DAY = 86_400;
const U64_MAX = (1n << 64n) - 1n;

export class CliInvariantError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(`${code}: ${message}`);
    this.code = code;
    this.name = "CliInvariantError";
  }
}

function requirePositiveInt(name: string, v: number, code: string): void {
  if (!Number.isSafeInteger(v) || v <= 0) {
    throw new CliInvariantError(code, `${name} must be a positive safe integer, got ${v}`);
  }
}

/** Parse a decimal string into a bigint in (0, i128::MAX]; rejects anything else. */
export function parsePositiveAmount(raw: string, code = "InvalidAmount"): bigint {
  if (typeof raw !== "string" || !/^[0-9]+$/.test(raw)) {
    throw new CliInvariantError(code, `amount must be a decimal integer string, got ${JSON.stringify(raw)}`);
  }
  const v = BigInt(raw);
  if (v <= 0n) throw new CliInvariantError(code, "amount must be positive");
  if (v > I128_MAX) throw new CliInvariantError(code, "amount exceeds i128::MAX");
  return v;
}

export function buildCreateOperation(input: CreateCliInput) {
  const startTime = input.startTime ?? Math.floor(Date.now() / 1000);
  const cliffDays = input.cliffDays ?? 0;
  if (!Number.isSafeInteger(startTime) || startTime < 0) {
    throw new CliInvariantError("InvalidTimeRange", `startTime invalid: ${startTime}`);
  }
  requirePositiveInt("durationDays", input.durationDays, "InvalidTimeRange");
  if (!Number.isSafeInteger(cliffDays) || cliffDays < 0 || cliffDays > input.durationDays) {
    throw new CliInvariantError("InvalidCliff", `cliffDays must be within [0, durationDays], got ${cliffDays}`);
  }
  const deposit = parsePositiveAmount(input.amount, "InvalidDeposit");
  const cliffTime = startTime + cliffDays * SECONDS_PER_DAY;
  const endTime = startTime + input.durationDays * SECONDS_PER_DAY;
  if (BigInt(endTime) > U64_MAX || !Number.isSafeInteger(endTime)) {
    throw new CliInvariantError("Overflow", "endTime overflows");
  }
  // Deposit must cover at least one unit per second of duration.
  if (deposit < BigInt(endTime - startTime)) {
    throw new CliInvariantError("DepositRateTooLow", "deposit is smaller than the stream duration in seconds");
  }

  return {
    createStream: {
      sender: "",
      recipient: input.recipient,
      token: input.token ?? "",
      deposit,
      startTime,
      endTime,
      cliffTime,
      cancellable: input.cancellable ?? true,
      pausable: input.pausable ?? true,
      transferable: input.transferable ?? true,
    },
  };
}

export function buildWithdrawOperation(input: WithdrawCliInput) {
  if (!Number.isSafeInteger(input.streamId) || input.streamId < 0) {
    throw new CliInvariantError("MalformedStreamId", `invalid streamId ${input.streamId}`);
  }
  return {
    withdraw: {
      streamId: input.streamId,
      amount: input.amount !== undefined ? parsePositiveAmount(input.amount) : undefined,
    },
  };
}

export function buildInspectQuery(input: InspectCliInput) {
  if (!Number.isSafeInteger(input.streamId) || input.streamId < 0) {
    throw new CliInvariantError("MalformedStreamId", `invalid streamId ${input.streamId}`);
  }
  return {
    inspect: {
      streamId: input.streamId,
    },
  };
}

export function buildExtendOperation(input: ExtendCliInput) {
  if (!Number.isSafeInteger(input.streamId) || input.streamId < 0) {
    throw new CliInvariantError("MalformedStreamId", `invalid streamId ${input.streamId}`);
  }
  if (input.ttlLedgers !== undefined) requirePositiveInt("ttlLedgers", input.ttlLedgers, "InvalidTtl");
  return {
    extend: {
      streamId: input.streamId,
      ttlLedgers: input.ttlLedgers,
    },
  };
}

export function buildBatchCreateFromCsv(
  csv: string,
  sender: string,
  token: string,
  startTime: number,
): BatchCreateTransaction {
  const rows = parse(csv);
  return buildBatchCreateTransaction(rows, {
    sender,
    token,
    startTime,
    cancellable: true,
    pausable: true,
    transferable: true,
  });
}

export function formatResult(result: PerpetuaCliResult): string {
  if (!result.ok) {
    return `Error: ${result.error}`;
  }
  return result.output;
}
