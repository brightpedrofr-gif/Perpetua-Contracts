# Error reference

Generated from `contracts/stream/src/error.rs`. Discriminants are frozen ABI: never renumber, only append. `script/validate-doc-alignment.py` fails if any variant or number here drifts from the source.

| # | Variant | Meaning |
|---|---|---|
| 1 | `StreamNotFound` | No stream exists with the given id. |
| 2 | `InvalidTimeRange` | `end_time <= start_time`. |
| 3 | `InvalidCliff` | `cliff_time` is outside [start_time, end_time]. |
| 4 | `InvalidDeposit` | Deposit is zero or negative. |
| 5 | `DepositRateTooLow` | `deposited < duration`, so the per-second rate truncates to zero and the recipient would accrue nothing. |
| 6 | `SelfStream` | Sender and recipient are the same address. |
| 7 | `Unauthorized` | Caller is not the party allowed to perform this action. |
| 8 | `NotCancellable` | `cancel` called on a stream created with `cancellable == false`. |
| 9 | `NotPausable` | `pause` called on a stream created with `pausable == false`. |
| 10 | `NotTransferable` | `transfer_recipient` called on a stream created with `transferable == false`. |
| 11 | `StreamNotActive` | Action requires an `Active` stream. |
| 12 | `StreamNotPaused` | `resume` called on a stream that is not `Paused`. |
| 13 | `StreamAlreadyPaused` | `pause` called on a stream that is already `Paused`. |
| 14 | `StreamTerminated` | Action attempted on a `Cancelled` or `Depleted` stream. |
| 15 | `StreamMatured` | `top_up` on a stream whose accrual clock has already reached `end_time`, Topping up a matured stream would make the new funds instantly withdrawable; create a new stream instead. |
| 16 | `InsufficientWithdrawable` | Requested amount exceeds the currently withdrawable balance. |
| 17 | `NothingToWithdraw` | Withdrawable balance is zero. |
| 18 | `InvalidAmount` | Explicit withdraw amount was zero or negative. |
| 19 | `BatchTooLarge` | Batch size exceeds `MAX_BATCH_SIZE`. |
| 20 | `EmptyBatch` | Batch contained no stream ids. |
| 21 | `DuplicateStreamId` | A Batch referenced the same stream id more than once. |
| 22 | `Overflow` | A Checked arithmetic operation overflowed or underflowed. |
| 23 | `TopUpTooSmall` | A positive `top_up` amount is smaller than one second of streaming at the current rate, so it cannot extend the duration at all and would instead vest retroactively. |
| 24 | `StreamIdExhausted` | The stream-id counter has reached `u64::MAX`; no further ids can be handed out. |
| 25 | `TokenTransferFailed` | The token contract rejected the transfer (e.g. |
| 26 | `TokenMissing` | The address stored as the stream's token does not resolve to a deployed contract. |
| 27 | `DelegateNotPermitted` | The delegate grant does not permit this operation on this stream. |
| 28 | `DelegateExpired` | The delegate grant has passed its `expires_at` timestamp. |
| 29 | `MalformedStreamId` | A serialized vector element of a batch is not a `u64`. |
| 30 | `RepeatedTransfer` | `transfer_recipient` to the current recipient. |
| 31 | `InvalidTopUp` | Zero or negative `top_up` amount. |
| 32 | `DepositTooSmall` | `deposited < (end_time - start_time)`, i.e. |
| 33 | `StreamIdOverflow` | The `next_stream_id` counter would overflow `u64` on increment. |
| 34 | `BatchSizeExceeded` | `batch_withdraw` received more than `MAX_BATCH_SIZE` (16) stream ids. |

## Usage

```ts
import { parseStreamError } from "../tools/sdk/errors.ts";
// A failed simulation surfaces `Error(Contract, #N)`; N is the first column above.
```

Match on the numeric discriminant, never on message text. `ArithmeticOverflow` is an alias of `Overflow` (22).

## Troubleshooting

- **`DepositRateTooLow` (5) / `DepositTooSmall` (32)**: deposit must be at least one stroop per second of duration (`deposited >= end - start`). `tools/sdk/batch_create.ts` rejects such rows before submission.
- **`BatchTooLarge` (19) / `BatchSizeExceeded` (34)**: chunk client-side to `MAX_BATCH_SIZE` (16).
- **`StreamNotFound` (1) for an id below `stream_count()`**: the entry is likely archived, not missing. Submit a `RestoreFootprint` (see `docs/soroban-rpc-read-skew.md`).
- **`TokenTransferFailed` (25)**: the token's own error is intentionally discarded; read `diagnosticEvents` of the failed transaction.
- **`StreamTerminated` (14)**: the stream is `Cancelled` or `Depleted`; no further transitions are possible.
- **`StreamMatured` (15)**: create a new stream instead of topping up.
