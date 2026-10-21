# Batch stream creation (`tools/sdk/batch_create.ts`)

Public API: `parseCsv`, `buildBatchCreateTransaction`, `submitBatchWithRecovery`, `MAX_BATCH_SIZE` (16).

## Usage

```ts
const rows = parseCsv(csvText);              // validates every row
const tx = buildBatchCreateTransaction(rows, { sender, token, startTime, cancellable: true, pausable: true, transferable: false });
await submitBatchWithRecovery(rows.length, { submit, readStreamCount, restore });
```

CSV columns (any order): `recipient,amount,duration_days,cliff_days`.

## Error catalogue (client-side, mirrors `docs/error.md`)

| Condition | Mirrors |
|---|---|
| Empty CSV / no data rows, missing or duplicate column, wrong cell count | n/a |
| More than 16 rows | `BatchTooLarge` (19) |
| Empty recipient, non-integer / non-positive / over-`i128::MAX` amount | `InvalidDeposit` (4) |
| `duration_days` not a positive safe integer | `InvalidTimeRange` (2) |
| `cliff_days` outside `[0, duration_days]` | `InvalidCliff` (3) |
| `amount < duration seconds` | `DepositRateTooLow` (5) |
| `amount * duration` over `i128::MAX` | `Overflow` (22) |

## Recovery procedure

`submitBatchWithRecovery` never blind-resubmits:

1. Reads `stream_count()` as a baseline.
2. `ARCHIVED` result: runs `restore` (RestoreFootprint, idempotent), then retries the identical transaction.
3. `TIMEOUT` / `TRY_AGAIN_LATER`: reads `stream_count()` again, waiting (bounded) until the RPC node's ledger is at or past the submit ledger (read skew, see `docs/soroban-rpc-read-skew.md`).
   - delta == batch size: the batch landed, do not resubmit.
   - delta == 0: safe to resubmit the same transaction.
   - anything else: throws; reconcile manually (another writer, or partial state).
4. `FAILED`: throws without retry.

Troubleshooting: "RPC read skew" error means the node stayed behind; retry against another node. "Unexpected stream_count delta" means concurrent creators; use a dedicated sender sequence or reconcile via `stream_created` events.
