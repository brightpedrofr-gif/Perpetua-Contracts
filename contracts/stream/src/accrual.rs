//! Accrual math — the stream's pure arithmetic core.
//!
//! Every function here is **pure**: it takes a [`Stream`] and a wall-clock
//! timestamp and returns a number. No `Env`, no storage, no host calls. That
//! makes the whole vesting model property-testable without a Soroban host, and
//! it keeps the interesting arithmetic in one auditable place.
//!
//! # The stream clock
//!
//! The central idea is that a stream has its own clock which stops while the
//! stream is paused. [`stream_time`] maps wall-clock time onto that clock:
//!
//! ```text
//! stream_time(now) = (paused_at.unwrap_or(now)) - paused_total
//! ```
//!
//! While paused, the numerator is frozen at the instant of the pause, so the
//! clock does not advance. After a resume, `paused_total` has absorbed the
//! paused interval, so the clock resumes exactly where it stopped. Every other
//! quantity — elapsed time, the cliff gate, vesting — is expressed against this
//! clock, which is why pausing stretches the schedule without ever changing the
//! total value delivered.
//!
//! Note that this differs from the naive formulation
//! `effective_now = min(now, end_time + paused_total)`, which is correct only
//! *after* a resume. During an in-progress pause that formula keeps accruing,
//! because the current pause has not yet been added to `paused_total`. Reading
//! `paused_at` is what makes the freeze actually freeze.
//!
//! # Stated invariants
//!
//! These hold for every stream at every instant, and every entry point is
//! responsible for preserving them. They are asserted after *every* operation
//! by the test suite (`Harness::assert_invariants`), exhaustively across
//! operation orderings by `test::monotonicity`, and over random schedules by
//! `test::props`.
//!
//! **I1 — Bounds.** `0 <= withdrawn <= vested(t) <= deposited`.
//!
//! **I2 — Monotonic in time.** For a fixed stream state and `t1 <= t2`,
//! `vested(t1) <= vested(t2)`.
//!
//! **I3 — Monotonic across calls.** For a *fixed* `t`, no entry point may
//! reduce `vested(t)`. Formally, if a call transforms stream state `S -> S'`,
//! then `vested(S', t) >= vested(S, t)`.
//!
//! **I4 — Conservation.** `vested(t) + refundable(t) == deposited`, exactly.
//!
//! **I5 — Pause coherence.** `paused_at.is_some()` if and only if
//! `status == Paused`, and while paused the clock does not advance.
//!
//! **I6 — Zero dust at settlement.** For every `t >= end_time`,
//! `vested(t) == deposited` and `refundable(t) == 0`. Any truncation residue
//! that exists strictly before maturity is therefore part of the sender's
//! refundable complement and cannot remain stranded after settlement.
//!
//! ## Explicit invariant enforcement for adversarial edge cases
//!
//! Four edge cases get explicit guards beyond what the type system already
//! enforces:
//!
//! **Zero-duration cliff.** `cliff_time == start_time` is valid (it means "no
//! cliff") and must not be confused with a post-schedule cliff. The guard in
//! [`cliff_reached`] treats `stream_time >= cliff_time` as the condition, so a
//! zero-offset cliff opens immediately at schedule start as documented.
//!
//! **`i128::MAX` deposits.** `vested` performs `deposited * elapsed`; at
//! `deposited = i128::MAX` even `elapsed = 1` would overflow. The
//! `checked_mul` guard maps this to `Err(Error::Overflow)` — a typed error,
//! never a host trap. `create_stream` front-loads the same guard, so a stream
//! in storage cannot overflow `vested` unless an external path bypassed
//! creation (which is not possible through the public ABI). The guard here is
//! defence in depth.
//!
//! **Same-ledger transitions.** `elapsed(stream, now)` is expressed against the
//! stream clock rather than wall-clock time, so a pause and resume that land on
//! the same ledger leave elapsed unchanged. A `cancel` at `now == start_time`
//! collapses `end_time = start_time`, making `duration = 0`; that is handled
//! by the zero-duration early return in [`vested`] which returns `deposited`
//! directly (matching the post-cancel rewrite) and avoids division by zero.
//!
//! **Saturating time arithmetic.** All `u64` subtractions in [`stream_time`]
//! and [`elapsed`] use `saturating_sub` so that a `paused_total` larger than
//! the current instant, or a wall-clock `now` before `start_time`, clamps to
//! zero elapsed time rather than wrapping.
//!
//! ## Why I3 is the dangerous one
//!
//! I2 is the obvious property and is easy to get right. **I3 is the one that
//! actually broke.** It is easy to violate by accident because it is a property
//! of *state transitions*, not of the accrual formula, so reading `vested` in
//! isolation never reveals it.
//!
//! `top_up` originally rounded its duration extension up, so the new duration
//! slightly overshot, the rate fell slightly, and `vested(t)` for the *same* `t`
//! came out lower after the call than before. That breaks I1 — a recipient who
//! had already withdrawn at the old rate now holds more than `vested` — and
//! from there `cancel`, which sets `deposited = vested`, drives the stream's
//! liability negative and refunds the sender tokens the recipient already has.
//!
//! The general rule this yields: **any operation that changes `deposited`,
//! `start_time`, `end_time`, `cliff_time` or `paused_total` must be checked
//! against I3**, because those are exactly the inputs to `vested`. Operations
//! that touch only `withdrawn`, `recipient` or `status` cannot violate it.
//! Today that means `top_up` and `cancel` need the check and the rest do not,
//! but the test suite verifies all of them so a future entry point cannot
//! quietly join the first group.
//!
//! ## Why I3 requires freezing the clock, and why that is not a detail
//!
//! I3 can only be observed by reading `vested` at one timestamp, performing
//! exactly one call, and reading `vested` again **at that same timestamp**.
//!
//! This is the reason the bug survived a suite that already had good coverage
//! of `top_up`. Every hand-written test advanced time around the operations it
//! exercised — deposit, wait, withdraw, wait, top up, wait, assert — because
//! that is how you write a readable test for a contract whose whole subject is
//! the passage of time. But `vested` is *supposed* to grow as the clock
//! advances. So a 93-stroop backwards step vanished into the accrual that
//! happened alongside it, and every assertion still passed. The regression was
//! real, deterministic, and reachable from a two-line test — and invisible to
//! roughly a hundred existing ones, because they all measured the wrong
//! difference.
//!
//! So the test design follows from the invariant rather than from convenience:
//! I2 (monotonic in time) is tested by holding *state* still and advancing the
//! clock, and I3 (monotonic across calls) is tested by holding the *clock*
//! still and advancing the state. Conflating the two hides exactly the class of
//! defect that matters most, because a violation of I3 is a fund-safety bug
//! while a violation of I2 is merely a wrong number.
//!
//! `test::monotonicity` implements the frozen-clock half across every entry
//! point and every ordering of them; `test::props` implements the
//! advancing-clock half over random schedules.
//!
//! # Accrual formula
//!
//! ```text
//! elapsed  = clamp(stream_time(now), start_time, end_time) - start_time
//! vested   = 0                                    if stream_time(now) < cliff_time
//!          = deposited * elapsed / duration       otherwise   (rounds down, integer division)
//!          = deposited                            if elapsed >= duration  (exact maturity)
//!          = deposited                            if duration == 0        (post-cancel instant)
//! ```
//!
//! **Rounding is always down.** Truncating in the recipient's disfavour is
//! correct: the residue stays in the pool and returns to the sender at
//! settlement, so the contract can never owe more than it holds.
//!
//! **Conservation is exact.** `refundable` is defined as `deposited - vested`,
//! so `vested + refundable == deposited` is an algebraic identity, never an
//! approximation. No dust term, no per-interval accumulation error.
//!
//! # Error handling
//!
//! The only error any function here returns is [`Error::Overflow`], signalling
//! that a checked arithmetic operation exceeded the `i128` range. Every other
//! degenerate case (zero duration, clock before start, saturating subtraction)
//! is handled silently with a well-defined result — never a panic, never an
//! opaque host trap.
//!
//! `create_stream` and `top_up` pre-validate that `deposited * duration` fits
//! in `i128`, so for any stream that reached persistent storage, [`vested`]
//! should never return `Err` in practice. The guard is present as defence in
//! depth against crafted streams or future code paths that might bypass the
//! creation validation.

use crate::error::Error;
use crate::types::Stream;

/// Map wall-clock time to the stream's own clock.
///
/// The stream clock stops while the stream is paused:
///
/// ```text
/// stream_time(now) = paused_at.unwrap_or(now) - paused_total
/// ```
///
/// While the stream is paused, `paused_at` is `Some(t)` where `t` is the
/// instant the clock froze. After a resume, `paused_total` has absorbed that
/// interval, so the clock continues from where it stopped.
///
/// # Saturating behaviour
///
/// Saturates to zero rather than underflowing. A stream whose `paused_total`
/// exceeds the freeze instant (which cannot happen through the public ABI, but
/// is guarded here for defence in depth) simply reads as time zero, which
/// downstream clamps to "no elapsed time".
///
/// # Same-ledger invariant
///
/// A pause and resume on the same ledger yield `frozen_at - paused_total ==
/// frozen_at - 0 == frozen_at`, leaving the stream clock exactly where it was
/// before the pair. The elapsed time does not change.
///
/// # Arguments
///
/// * `stream` — The stream whose clock is to be read.
/// * `now` — The current ledger timestamp in Unix seconds.
///
/// # Returns
///
/// The stream's clock value at `now`, in the same units and origin as
/// `start_time` and `end_time`. Always `<= now` (or `<= paused_at` while
/// paused).
pub fn stream_time(stream: &Stream, now: u64) -> u64 {
    let frozen_at = match stream.paused_at {
        Some(paused_at) => paused_at,
        None => now,
    };
    frozen_at.saturating_sub(stream.paused_total)
}

/// Total scheduled duration of the stream, in seconds.
///
/// Computed as `end_time - start_time` using saturating arithmetic. This is
/// the denominator in the linear accrual formula. Guaranteed non-zero at
/// creation ([`Error::InvalidTimeRange`] rejects `end_time <= start_time`),
/// but can legitimately become zero after a `cancel` that collapses the
/// schedule onto `start_time`. [`vested`] handles that zero-duration case
/// explicitly (returning `deposited` directly) to avoid a division by zero.
///
/// # Arguments
///
/// * `stream` — The stream whose duration is to be computed.
///
/// # Returns
///
/// Duration in seconds. Zero only for a post-cancel stream where
/// `end_time == start_time`.
pub fn duration(stream: &Stream) -> u64 {
    stream.end_time.saturating_sub(stream.start_time)
}

/// Seconds of the schedule that have been consumed, clamped to `[0, duration]`.
///
/// Expresses how far through the linear accrual curve the stream is at `now`.
/// Uses the stream clock rather than wall-clock time, so pausing freezes this
/// value.
///
/// Clamping to `[0, duration]` means:
///
/// * If `stream_time(now) < start_time`, elapsed is 0 — the schedule has not
///   started yet (backdated streams are valid but deliver nothing before
///   `start_time`).
/// * If `stream_time(now) > end_time`, elapsed is `duration` — maturity is an
///   exact terminal boundary; the schedule does not keep running past it.
///
/// # Arguments
///
/// * `stream` — The stream whose elapsed time is to be computed.
/// * `now` — The current ledger timestamp in Unix seconds.
///
/// # Returns
///
/// Elapsed seconds in `[0, duration(stream)]`.
pub fn elapsed(stream: &Stream, now: u64) -> u64 {
    let clock = stream_time(stream, now);
    let capped = if clock > stream.end_time {
        stream.end_time
    } else {
        clock
    };
    capped.saturating_sub(stream.start_time)
}

/// Whether the cliff gate has opened, allowing withdrawals.
///
/// The gate is evaluated against the **stream clock** (not wall-clock time),
/// so a stream paused across its cliff does not silently pass the cliff while
/// frozen. The stream must be explicitly resumed before the cliff opens.
///
/// # Cliff semantics: gate, not delayed start
///
/// The cliff **gates** the payout; it does not delay the start of accrual:
///
/// * At `stream_time(now) < cliff_time` — gate closed; `vested` returns 0.
/// * At `stream_time(now) == cliff_time` — gate opens in a single discrete
///   step, immediately releasing all value accrued continuously since
///   `start_time` (not merely what would have accrued since `cliff_time`).
/// * After `cliff_time` — accrual continues linearly along the original
///   schedule until `end_time`.
///
/// # Zero-duration cliff (cliff_time == start_time)
///
/// When `cliff_time == start_time` there is effectively no cliff: the gate
/// opens at schedule start and the first second of accrual is immediately
/// withdrawable. This is the valid "no cliff" configuration and is handled
/// correctly here: `stream_time >= start_time` is true from the moment the
/// stream starts, so the cliff is open.
///
/// # Arguments
///
/// * `stream` — The stream whose cliff is to be checked.
/// * `now` — The current ledger timestamp in Unix seconds.
///
/// # Returns
///
/// `true` if the cliff has been reached and withdrawals are permitted.
pub fn cliff_reached(stream: &Stream, now: u64) -> bool {
    stream_time(stream, now) >= stream.cliff_time
}

/// Total amount vested at `now`: what the recipient has earned in total, ever.
///
/// This is the primary computation of the accrual engine. It is evaluated from
/// scratch on every call rather than incrementally, which is why conservation
/// is exact and truncation error never accumulates.
///
/// # Formula
///
/// ```text
/// vested = 0                                  if stream_time(now) < cliff_time
/// vested = deposited                          if duration == 0 (post-cancel instant)
/// vested = deposited                          if elapsed >= duration (maturity)
/// vested = deposited * elapsed / duration     otherwise (integer division, rounds down)
/// ```
///
/// # Rounding direction
///
/// Rounds **down** (truncates toward zero). Truncating in the recipient's
/// disfavour is correct: the residue stays in the pool and is returned to the
/// sender at settlement via [`refundable`], so the pool can never owe more than
/// it holds.
///
/// # Maturity is an exact terminal boundary
///
/// When `elapsed >= duration`, this function returns `deposited` directly
/// rather than evaluating the ratio. This makes the zero-dust consequence
/// explicit even when `duration` does not divide `deposited` cleanly: at
/// maturity, every stroop is vested, nothing is refundable, and the pool
/// balance is exactly zero.
///
/// # Zero-duration streams (post-cancel)
///
/// `cancel` can collapse the schedule by setting `end_time = start_time`,
/// making `duration = 0`. In that case `deposited` has already been rewritten
/// to the amount vested at the cancel instant, so returning it in full is
/// exactly right — and it avoids a division by zero.
///
/// # Overflow guard
///
/// The multiplication `deposited * elapsed` is performed with `checked_mul`.
/// At `deposited = i128::MAX`, even `elapsed = 1` would overflow. Returning
/// `Err(Error::Overflow)` (equivalently `Err(Error::ArithmeticOverflow)`) is
/// the correct response — a typed error, never a panic or opaque host trap.
/// `create_stream` front-loads the same guard so a well-formed stream in
/// storage cannot trigger this in practice; the guard here is defence in depth.
///
/// # Arguments
///
/// * `stream` — The stream whose vested amount is to be computed.
/// * `now` — The current ledger timestamp in Unix seconds.
///
/// # Returns
///
/// * `Ok(v)` where `0 <= v <= stream.deposited`.
/// * `Err(Error::Overflow)` if the multiplication overflows `i128`.
///
/// # Errors
///
/// Returns [`Error::Overflow`] (alias [`Error::ArithmeticOverflow`]) if the
/// `deposited * elapsed` multiplication exceeds the `i128` range. This only
/// occurs for streams whose `deposited * duration` product exceeds `i128::MAX`,
/// which `create_stream` rejects at creation time.
pub fn vested(stream: &Stream, now: u64) -> Result<i128, Error> {
    if !cliff_reached(stream, now) {
        return Ok(0);
    }

    let total_duration = duration(stream);

    // A zero duration means the schedule has collapsed onto a single instant,
    // which only happens after `cancel` lands at `start_time`. In that case
    // `deposited` has already been rewritten to the amount vested at the
    // cancel, so returning it in full is exactly right — and it avoids a
    // division by zero.
    //
    // Edge-case invariant: this also handles the same-ledger transition where
    // cancel is called at start_time in the same ledger the stream was created.
    if total_duration == 0 {
        return Ok(stream.deposited);
    }

    let consumed = elapsed(stream, now);
    // Maturity is an exact terminal boundary. Returning the original deposit
    // here, instead of evaluating the ratio at the boundary, makes the
    // zero-dust consequence explicit even when the duration does not divide
    // the deposit cleanly.
    if consumed >= total_duration {
        return Ok(stream.deposited);
    }

    // Overflow guard for adversarial inputs (e.g. i128::MAX deposits).
    // `create_stream` validates that `deposited * duration` fits in i128, so
    // for any well-formed stream in storage this checked_mul succeeds. The
    // guard is here as defence in depth against streams that bypass creation
    // validation, and for fuzz/property tests that synthesise extreme values.
    let numerator = stream
        .deposited
        .checked_mul(consumed as i128)
        .ok_or(Error::Overflow)?;
    let raw = numerator
        .checked_div(total_duration as i128)
        .ok_or(Error::Overflow)?;

    // Clamp explicitly rather than trusting the arithmetic. `consumed` is
    // already capped at `total_duration`, so this should be unreachable, but
    // the invariant is load-bearing enough to assert rather than assume.
    // Returning `deposited` here also ensures I6 (zero-dust at settlement)
    // holds even on the exact-boundary case where floating-point intuition
    // might suggest the ratio is strictly less than 1.
    Ok(if raw > stream.deposited {
        stream.deposited
    } else {
        raw
    })
}

/// Amount the recipient can withdraw right now: `vested(now) - withdrawn`.
///
/// Saturates at zero rather than returning a negative number. `withdrawn` can
/// momentarily exceed `vested` only if a `cancel` rewrote the schedule after a
/// partial withdrawal — and even then `cancel` guarantees
/// `deposited >= withdrawn`, so this saturation path is defence in depth, not
/// normal operation.
///
/// # Arguments
///
/// * `stream` — The stream whose withdrawable amount is to be computed.
/// * `now` — The current ledger timestamp in Unix seconds.
///
/// # Returns
///
/// * `Ok(a)` where `a >= 0`.
/// * `Err(Error::Overflow)` if the underlying [`vested`] call overflows.
///
/// # Errors
///
/// Propagates [`Error::Overflow`] from [`vested`] if the multiplication
/// overflows. See [`vested`] for details.
pub fn withdrawable(stream: &Stream, now: u64) -> Result<i128, Error> {
    let earned = vested(stream, now)?;
    let available = earned
        .checked_sub(stream.withdrawn)
        .ok_or(Error::Overflow)?;
    Ok(if available < 0 { 0 } else { available })
}

/// Amount still locked for the sender to recover if they cancel now.
///
/// Defined as `deposited - vested(now)`, the exact complement of [`vested`].
/// Because conservation is exact (`vested + refundable == deposited`
/// algebraically), there is no dust term and no rounding residue.
///
/// * Before maturity, truncation residue from integer division is part of
///   `refundable`, not part of `vested`. This keeps the pool solvent.
/// * At and after maturity, `refundable` is exactly zero — no dust is ever
///   stranded in the pool after settlement.
///
/// # Arguments
///
/// * `stream` — The stream whose refundable amount is to be computed.
/// * `now` — The current ledger timestamp in Unix seconds.
///
/// # Returns
///
/// * `Ok(r)` where `r >= 0`.
/// * `Err(Error::Overflow)` if the underlying [`vested`] call overflows.
///
/// # Errors
///
/// Propagates [`Error::Overflow`] from [`vested`] if the multiplication
/// overflows. The `checked_sub` here cannot overflow because `deposited` is
/// always `>= vested` by construction (I1), but is written as `checked_sub`
/// as an extra layer of defence.
pub fn refundable(stream: &Stream, now: u64) -> Result<i128, Error> {
    let earned = vested(stream, now)?;
    stream.deposited.checked_sub(earned).ok_or(Error::Overflow)
}

/// Outstanding liability of this stream against the pooled contract balance.
///
/// Defined as `deposited - withdrawn`: the portion of the deposit that has not
/// yet left the contract via a withdrawal. This quantity must be backed by
/// actual tokens in the contract's pool at all times.
///
/// The pool invariant is:
/// ```text
/// sum(liability(s) for all live streams s) <= contract_token_balance
/// ```
///
/// A violation means some stream's claim is unbacked and a recipient cannot be
/// paid. The test harness asserts this after every operation via
/// `assert_pool_invariant`.
///
/// Note: `liability` can be negative if `withdrawn > deposited`. This is
/// impossible through the public ABI but is representable by the type; callers
/// that sum liabilities should check for this. The test suite covers it in
/// `accrual_overflow.rs`.
///
/// # Arguments
///
/// * `stream` — The stream whose outstanding liability is to be computed.
///
/// # Returns
///
/// * `Ok(l)` where normally `0 <= l <= deposited`.
/// * `Err(Error::Overflow)` if the subtraction overflows (impossible in
///   practice for streams created through the public ABI).
///
/// # Errors
///
/// Returns [`Error::Overflow`] if `deposited - withdrawn` overflows `i128`.
/// This cannot occur through the public ABI because `deposited` and
/// `withdrawn` are both non-negative and `withdrawn <= deposited` is enforced
/// by invariant I1.
pub fn liability(stream: &Stream) -> Result<i128, Error> {
    stream
        .deposited
        .checked_sub(stream.withdrawn)
        .ok_or(Error::Overflow)
}

/// Verdict on a read taken from an RPC node that may lag the ledger the
/// caller expects. Pure so it can be tested without a host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum ReadFreshness {
    /// Observed ledger is at or past the required ledger: safe to derive from.
    Fresh,
    /// Observed ledger is behind: discard and re-read, never combine.
    Stale { behind_by: u32 },
}

/// Classify a read against the ledger the caller requires (e.g. the ledger of
/// the confirmed write it follows). Deterministic and side-effect free, so a
/// retry after skew can never corrupt accounting: the caller simply re-reads.
#[allow(dead_code)]
pub fn classify_read(observed_ledger: u32, required_ledger: u32) -> ReadFreshness {
    if observed_ledger >= required_ledger {
        ReadFreshness::Fresh
    } else {
        ReadFreshness::Stale {
            behind_by: required_ledger - observed_ledger,
        }
    }
}

/// What an operator or client must do when a stream entry cannot be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum Recovery {
    /// Entry is present; nothing to do.
    None,
    /// Id was issued but the entry is gone: restore the archived
    /// `DataKey::Stream(id)` footprint (`RestoreFootprint`), then retry.
    RestoreFootprint,
    /// Id was never issued: retrying cannot help.
    NeverExisted,
}

/// Decide the recovery path from presence and the id counter. An id below
/// `next_stream_id` was issued, so a missing entry means archived, not absent.
#[allow(dead_code)]
pub fn recovery_for(present: bool, stream_id: u64, next_stream_id: u64) -> Recovery {
    if present {
        Recovery::None
    } else if stream_id < next_stream_id {
        Recovery::RestoreFootprint
    } else {
        Recovery::NeverExisted
    }
}

/// Amount a retried withdraw may still pull given the amount already applied.
/// Idempotent: replaying a request whose effect is already in `withdrawn`
/// yields zero rather than a double payout.
#[allow(dead_code)]
pub fn remaining_to_apply(requested_total: i128, withdrawn: i128) -> i128 {
    requested_total.saturating_sub(withdrawn).max(0)
}
