export interface BatchStreamRow {
  recipient: string;
  amount: string;
  durationDays: string;
  cliffDays: string;
}

export interface ParsedBatchStream {
  recipient: string;
  amount: bigint;
  durationDays: number;
  cliffDays: number;
}

export const MAX_BATCH_SIZE = 16;
const SECONDS_PER_DAY = 86_400;
const I128_MAX = (1n << 127n) - 1n;

function validateRow(row: ParsedBatchStream, rowNumber: number): number {
  if (!row.recipient.trim()) {
    throw new Error(`Row ${rowNumber}: recipient must not be empty`);
  }
  if (row.amount <= 0n || row.amount > I128_MAX) {
    throw new Error(`Row ${rowNumber}: amount must be between 1 and i128::MAX`);
  }
  if (!Number.isSafeInteger(row.durationDays) || row.durationDays <= 0) {
    throw new Error(`Row ${rowNumber}: duration_days must be a positive safe integer`);
  }
  if (
    !Number.isSafeInteger(row.cliffDays) ||
    row.cliffDays < 0 ||
    row.cliffDays > row.durationDays
  ) {
    throw new Error(`Row ${rowNumber}: cliff_days must be between 0 and duration_days`);
  }

  const durationSeconds = BigInt(row.durationDays) * BigInt(SECONDS_PER_DAY);
  if (durationSeconds > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`Row ${rowNumber}: duration exceeds the safe timestamp range`);
  }
  if (row.amount < durationSeconds) {
    throw new Error(`Row ${rowNumber}: amount is below the minimum per-second deposit rate`);
  }
  if (row.amount * durationSeconds > I128_MAX) {
    throw new Error(`Row ${rowNumber}: amount multiplied by duration exceeds i128::MAX`);
  }
  return Number(durationSeconds);
}

export function parseCsv(input: string): ParsedBatchStream[] {
  const trimmedInput = input.trim();
  const lines = trimmedInput ? trimmedInput.split(/\r?\n/) : [];
  if (lines.length < 2) {
    throw new Error("CSV must contain a header row and at least one data row");
  }
  if (lines.length - 1 > MAX_BATCH_SIZE) {
    throw new Error(`Batch size exceeds MAX_BATCH_SIZE of ${MAX_BATCH_SIZE}; split client-side`);
  }

  const header = lines[0].split(",").map((cell) => cell.trim().toLowerCase());
  const required = ["recipient", "amount", "duration_days", "cliff_days"];
  for (const field of required) {
    if (!header.includes(field)) {
      throw new Error(`Missing required CSV column: ${field}`);
    }
  }
  if (new Set(header).size !== header.length) {
    throw new Error("CSV header contains duplicate columns");
  }

  const recipientIndex = header.indexOf("recipient");
  const amountIndex = header.indexOf("amount");
  const durationIndex = header.indexOf("duration_days");
  const cliffIndex = header.indexOf("cliff_days");

  const results: ParsedBatchStream[] = [];
  for (let i = 1; i < lines.length; i++) {
    const cells = lines[i].split(",");
    if (cells.length !== header.length) {
      throw new Error(`Row ${i + 1} has ${cells.length} columns, expected ${header.length}`);
    }

    const amountText = cells[amountIndex].trim();
    let amount: bigint;
    try {
      amount = BigInt(amountText);
    } catch {
      throw new Error(`Row ${i + 1}: amount must be an integer`);
    }
    const row: ParsedBatchStream = {
      recipient: cells[recipientIndex].trim(),
      amount,
      durationDays: Number(cells[durationIndex].trim()),
      cliffDays: Number(cells[cliffIndex].trim()),
    };
    validateRow(row, i + 1);
    results.push(row);
  }

  return results;
}

export interface BuildBatchCreateOptions {
  sender: string;
  token: string;
  startTime: number;
  cancellable: boolean;
  pausable: boolean;
  transferable: boolean;
}

export interface BatchCreateTransaction {
  source: string;
  operations: Array<{
    createStream: {
      sender: string;
      recipient: string;
      token: string;
      deposit: bigint;
      startTime: number;
      endTime: number;
      cliffTime: number;
      cancellable: boolean;
      pausable: boolean;
      transferable: boolean;
    };
  }>;
}

export function buildBatchCreateTransaction(
  rows: ParsedBatchStream[],
  options: BuildBatchCreateOptions,
): BatchCreateTransaction {
  if (rows.length === 0) {
    throw new Error("No rows to build into a transaction");
  }
  if (rows.length > MAX_BATCH_SIZE) {
    throw new Error(`Batch size exceeds MAX_BATCH_SIZE of ${MAX_BATCH_SIZE}; split client-side`);
  }
  if (!Number.isSafeInteger(options.startTime) || options.startTime < 0) {
    throw new Error("startTime must be a non-negative safe integer");
  }

  const operations: BatchCreateTransaction["operations"] = new Array(rows.length);
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i];
    const durationSeconds = validateRow(row, i + 1);
    const cliffSeconds = row.cliffDays * SECONDS_PER_DAY;
    const endTime = options.startTime + durationSeconds;
    const cliffTime = options.startTime + cliffSeconds;
    if (!Number.isSafeInteger(endTime) || !Number.isSafeInteger(cliffTime)) {
      throw new Error(`Row ${i + 1}: schedule exceeds the safe timestamp range`);
    }

    operations[i] = { createStream: {
      sender: options.sender,
      recipient: row.recipient,
      token: options.token,
      deposit: row.amount,
      startTime: options.startTime,
      endTime,
      cliffTime,
      cancellable: options.cancellable,
      pausable: options.pausable,
      transferable: options.transferable,
    } };
  }

  return {
    source: options.sender,
    operations,
  };
}

// ---------------------------------------------------------------------------
// Fault recovery: archival, RPC read skew, idempotent retry
// ---------------------------------------------------------------------------

export type SubmitStatus = "SUCCESS" | "FAILED" | "TIMEOUT" | "TRY_AGAIN_LATER" | "ARCHIVED";

export interface SubmitOutcome {
  status: SubmitStatus;
  ledger?: number;
}

export interface StreamCountRead {
  streamCount: bigint;
  /** Latest ledger the RPC node had applied when serving this read. */
  ledger: number;
}

export interface RecoveryDeps {
  /** Submit the (byte-identical) batch transaction. */
  submit: () => Promise<SubmitOutcome>;
  /** Read `stream_count()`; used to detect whether a timed-out submit landed. */
  readStreamCount: () => Promise<StreamCountRead>;
  /** Submit a RestoreFootprint transaction for archived entries (idempotent). */
  restore: () => Promise<void>;
  sleep?: (ms: number) => Promise<void>;
}

export interface RecoveryOptions {
  maxAttempts?: number;
  backoffMs?: number;
  /** Max reads while waiting for a lagging RPC node to reach the submit ledger. */
  maxSkewReads?: number;
}

export interface RecoveryResult {
  attempts: number;
  restored: boolean;
  /** True when a timed-out/retried submit was found to have already landed. */
  alreadyLanded: boolean;
}

/**
 * Submit a batch with deterministic recovery.
 *
 * - `ARCHIVED`: restore the footprint, then retry the same transaction.
 * - `TIMEOUT` / `TRY_AGAIN_LATER`: never blind-resubmit. Compare `stream_count`
 *   with the pre-submit baseline (reads behind a lagging node are rejected until
 *   `ledger >= minLedger`). A delta equal to the batch size means it landed; a
 *   zero delta means it is safe to resubmit; anything else is a partial/foreign
 *   write and throws rather than risking duplicate streams.
 */
export async function submitBatchWithRecovery(
  batchSize: number,
  deps: RecoveryDeps,
  options: RecoveryOptions = {},
): Promise<RecoveryResult> {
  if (!Number.isSafeInteger(batchSize) || batchSize <= 0 || batchSize > MAX_BATCH_SIZE) {
    throw new Error(`batchSize must be between 1 and MAX_BATCH_SIZE (${MAX_BATCH_SIZE})`);
  }
  const maxAttempts = options.maxAttempts ?? 3;
  const backoffMs = options.backoffMs ?? 1000;
  const maxSkewReads = options.maxSkewReads ?? 5;
  const sleep = deps.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));

  const baseline = (await deps.readStreamCount()).streamCount;
  let restored = false;

  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const outcome = await deps.submit();
    if (outcome.status === "SUCCESS") {
      return { attempts: attempt, restored, alreadyLanded: false };
    }
    if (outcome.status === "FAILED") {
      throw new Error("Batch transaction failed on chain; not retrying");
    }
    if (outcome.status === "ARCHIVED") {
      await deps.restore();
      restored = true;
      continue;
    }

    // TIMEOUT / TRY_AGAIN_LATER: determine whether the batch landed.
    const minLedger = outcome.ledger ?? 0;
    let read = await deps.readStreamCount();
    for (let i = 0; read.ledger < minLedger && i < maxSkewReads; i++) {
      await sleep(backoffMs);
      read = await deps.readStreamCount();
    }
    if (read.ledger < minLedger) {
      throw new Error(
        `RPC read skew: node at ledger ${read.ledger} is behind ledger ${minLedger}; cannot confirm batch state`,
      );
    }
    const delta = read.streamCount - baseline;
    if (delta === BigInt(batchSize)) {
      return { attempts: attempt, restored, alreadyLanded: true };
    }
    if (delta !== 0n) {
      throw new Error(
        `Unexpected stream_count delta ${delta} for batch of ${batchSize}; manual reconciliation required`,
      );
    }
    await sleep(backoffMs * attempt);
  }

  throw new Error(`Batch not confirmed after ${maxAttempts} attempts`);
}
