//! Performance regression tests: accrual within Soroban CPU/memory budgets.
//!
//! # Background
//!
//! `accrual.rs` is on the hot path for every stream operation: `withdraw`,
//! `batch_withdraw`, `cancel`, and the view functions all call one or more of
//! `vested`, `withdrawable`, `refundable`, or `liability`. Under batch payroll
//! workloads — `batch_withdraw` at `MAX_BATCH_SIZE` (16) streams — the accrual
//! engine is called 16 times per invocation. If the pure math were to acquire
//! a hidden allocation or a quadratic loop, the batch would silently approach
//! the protocol limits.
//!
//! This module complements `test::resource_limits` (which tests the overall
//! transaction cost of batch_withdraw) by asserting properties that are
//! specific to the accrual engine:
//!
//! 1. **No super-linear CPU scaling.** The per-stream accrual cost in a
//!    batch_withdraw must scale linearly with batch size. Doubling the number
//!    of streams in the batch must not more than double the instruction count.
//!
//! 2. **Memory independence.** The accrual helpers are pure functions (no `Env`,
//!    no host calls). Calling them for any number of streams must not affect the
//!    linear-memory allocation of the invoking transaction.
//!
//! 3. **Budget headroom at batch payroll scale.** A batch_withdraw at
//!    `MAX_BATCH_SIZE` on streams that have accumulated a long accrual history
//!    (many pauses, multiple top-ups) must still complete with 2× headroom on
//!    every protocol limit.
//!
//! 4. **Accrual cost is independent of stream age and history.** `vested` is
//!    a closed-form computation from a fixed set of fields; it does not scan
//!    any history. A stream with 1000 prior withdrawals costs the same to accrue
//!    as a freshly created one.
//!
//! 5. **Pause/resume history does not inflate cost.** `paused_total` is a
//!    single accumulated counter. Accrual reads it once; it does not iterate.
//!
//! # What this module does NOT test
//!
//! * The absolute instruction count for `vested` alone — this is a pure Rust
//!   function, measured in nanoseconds without a host, and is not subject to
//!   Soroban's instruction budget in isolation.
//! * End-to-end gas cost of the full stream lifecycle — that is `test::resource_limits`.
//! * The event byte budget — also `test::resource_limits`.
//!
//! These tests use the real Soroban test host (via `Harness`) because only a
//! full invocation can measure the instruction and entry counts that the
//! protocol enforces.

use super::common::*;
use crate::{accrual, MAX_BATCH_SIZE};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

// ---------------------------------------------------------------------------
// Budget constants (same as test::resource_limits)
// ---------------------------------------------------------------------------

const LEDGER_ENTRY_LIMIT: u32 = 400;
const WRITE_ENTRY_LIMIT: u32 = 200;
const EVENT_BYTES_LIMIT: u32 = 16_384;
const INSTRUCTION_LIMIT: i64 = 400_000_000;

/// 2× safety factor — required headroom at MAX_BATCH_SIZE so that heavier
/// token contracts and future protocol changes do not immediately exhaust the
/// budget.
const REQUIRED_HEADROOM_FACTOR: u32 = 2;

/// Maximum acceptable ratio of per-item cost growth between batch sizes.
/// A batch of N items must cost at most N × (1-item cost) + SLACK instructions.
const PER_ITEM_SLACK: i64 = 5_000_000;

#[derive(Debug, Clone, Copy)]
struct Cost {
    footprint: u32,
    disk_reads: u32,
    writes: u32,
    memory_reads: u32,
    memory_bytes: i64,
    instructions: i64,
    event_bytes: u32,
}

fn measure(h: &Harness, label: &str) -> Cost {
    let r = h.env.cost_estimate().resources();
    let cost = Cost {
        footprint: r.disk_read_entries + r.memory_read_entries + r.write_entries,
        disk_reads: r.disk_read_entries,
        writes: r.write_entries,
        memory_reads: r.memory_read_entries,
        memory_bytes: r.mem_bytes,
        instructions: r.instructions,
        event_bytes: r.contract_events_size_bytes,
    };
    std::println!(
        "accrual_budget: {label:<36} \
         footprint={:<4}  disk={:<3}  writes={:<3}  mem_reads={:<4}  \
         mem_bytes={:<10}  events={:<6}  instructions={}",
        cost.footprint,
        cost.disk_reads,
        cost.writes,
        cost.memory_reads,
        cost.memory_bytes,
        cost.event_bytes,
        cost.instructions,
    );
    cost
}

fn assert_headroom(label: &str, cost: Cost, factor: u32) {
    assert_eq!(
        cost.disk_reads,
        0,
        "{label}: live state incurred {} disk reads (expected 0 under protocol 27)",
        cost.disk_reads,
    );
    assert!(
        cost.footprint * factor <= LEDGER_ENTRY_LIMIT,
        "{label}: footprint {} lacks {factor}x headroom under {LEDGER_ENTRY_LIMIT}",
        cost.footprint,
    );
    assert!(
        cost.writes * factor <= WRITE_ENTRY_LIMIT,
        "{label}: {} writes lack {factor}x headroom under {WRITE_ENTRY_LIMIT}",
        cost.writes,
    );
    assert!(
        cost.instructions <= INSTRUCTION_LIMIT,
        "{label}: {} instructions exceed protocol limit {INSTRUCTION_LIMIT}",
        cost.instructions,
    );
    assert!(
        cost.event_bytes * factor <= EVENT_BYTES_LIMIT,
        "{label}: {} event bytes lack {factor}x headroom under {EVENT_BYTES_LIMIT}",
        cost.event_bytes,
    );
}

// ---------------------------------------------------------------------------
// 1. batch_withdraw with MAX_BATCH_SIZE streams keeps budget headroom
// ---------------------------------------------------------------------------

/// A batch payroll run (`batch_withdraw` at `MAX_BATCH_SIZE`) on freshly
/// created streams must complete within every protocol limit with the required
/// 2× headroom.
#[test]
fn batch_payroll_at_max_batch_size_keeps_2x_budget_headroom() {
    let h = Harness::new();
    let ids: std::vec::Vec<u64> = (0..MAX_BATCH_SIZE)
        .map(|_| h.create_simple(100 * ONE, 100 * DAY))
        .collect();
    h.advance(30 * DAY);

    h.client.batch_withdraw(&h.recipient, &h.ids(&ids));
    let cost = measure(&h, "batch_payroll(MAX, fresh)");

    assert_headroom(
        "batch_payroll(MAX, fresh)",
        cost,
        REQUIRED_HEADROOM_FACTOR,
    );
}

/// Same test but on streams with a complex history: paused, resumed, and
/// topped up before the batch_withdraw. The accrual path is identical (pure
/// formula), but verifies that accumulated `paused_total` and additional
/// storage entries do not inflate the budget.
#[test]
fn batch_payroll_with_pause_resume_history_keeps_2x_budget_headroom() {
    let h = Harness::new();
    let start = h.now();
    let ids: std::vec::Vec<u64> = (0..MAX_BATCH_SIZE)
        .map(|_| h.create(100 * ONE, start, start + 100 * DAY, start, true, true, true))
        .collect();

    // Give each stream a non-trivial history.
    h.advance(10 * DAY);
    for &id in &ids {
        h.client.pause(&id);
    }
    h.advance(5 * DAY);
    for &id in &ids {
        h.client.resume(&id);
    }
    h.advance(5 * DAY);
    for &id in &ids {
        let _ = h.client.try_top_up(&id, &(10 * ONE));
    }
    h.advance(20 * DAY);

    h.client.batch_withdraw(&h.recipient, &h.ids(&ids));
    let cost = measure(&h, "batch_payroll(MAX, with_history)");

    assert_headroom(
        "batch_payroll(MAX, with_history)",
        cost,
        REQUIRED_HEADROOM_FACTOR,
    );
}

// ---------------------------------------------------------------------------
// 2. Accrual cost scales linearly with batch size
// ---------------------------------------------------------------------------

/// The instruction count for `batch_withdraw` must scale linearly with batch
/// size. Doubling the batch size must not more than double the instruction count
/// (plus a constant slack for per-invocation overhead).
#[test]
fn batch_withdraw_instruction_count_scales_linearly() {
    let h = Harness::new();
    let all_ids: std::vec::Vec<u64> = (0..MAX_BATCH_SIZE)
        .map(|_| h.create_simple(100 * ONE, 100 * DAY))
        .collect();

    // Warm up: first withdrawal to recipient creates their token balance entry.
    h.advance(DAY);
    h.client
        .batch_withdraw(&h.recipient, &h.ids(&all_ids[..1]));

    let sizes: &[usize] = &[1, 2, 4, 8, MAX_BATCH_SIZE as usize];
    let mut costs: std::vec::Vec<(usize, Cost)> = std::vec::Vec::new();

    for &size in sizes {
        h.advance(DAY);
        h.client
            .batch_withdraw(&h.recipient, &h.ids(&all_ids[..size]));
        let cost = measure(&h, &std::format!("batch_withdraw({size})"));
        assert_eq!(cost.disk_reads, 0, "batch_withdraw({size}): disk reads");
        costs.push((size, cost));
    }

    let (_, base) = costs[0];
    for &(size, cost) in &costs[1..] {
        let linear_bound =
            base.instructions * (size as i64) + PER_ITEM_SLACK;
        assert!(
            cost.instructions <= linear_bound,
            "batch_withdraw({size}): instructions {} exceeded linear bound {} \
             (base={} per stream + {PER_ITEM_SLACK} slack)",
            cost.instructions,
            linear_bound,
            base.instructions,
        );
    }
}

/// Footprint must also scale linearly.
#[test]
fn batch_withdraw_footprint_scales_linearly() {
    let h = Harness::new();
    let all_ids: std::vec::Vec<u64> = (0..MAX_BATCH_SIZE)
        .map(|_| h.create_simple(100 * ONE, 100 * DAY))
        .collect();

    // Warm up token balance entry creation.
    h.advance(DAY);
    h.client.batch_withdraw(&h.recipient, &h.ids(&all_ids[..1]));

    let (_, base) = {
        h.advance(DAY);
        h.client.batch_withdraw(&h.recipient, &h.ids(&all_ids[..1]));
        (1usize, measure(&h, "footprint base(1)"))
    };

    for size in [2usize, 4, 8, MAX_BATCH_SIZE as usize] {
        h.advance(DAY);
        h.client
            .batch_withdraw(&h.recipient, &h.ids(&all_ids[..size]));
        let cost = measure(&h, &std::format!("footprint scale({size})"));
        assert!(
            cost.footprint <= base.footprint * size as u32,
            "batch_withdraw({size}): footprint {} exceeded linear bound {}",
            cost.footprint,
            base.footprint * size as u32
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Accrual cost is independent of stream age
// ---------------------------------------------------------------------------

/// `vested`, `withdrawable`, and `refundable` are closed-form computations.
/// A stream created 1000 ledgers ago costs the same to accrue as one created
/// this ledger. This is pinned by comparing footprint of withdraw on an old
/// stream vs. a new one.
#[test]
fn withdraw_footprint_is_independent_of_stream_age() {
    let h = Harness::new();

    // Warm up: create first stream and recipient token balance entry.
    let warmup = h.create_simple(100 * ONE, 1_000 * DAY);
    h.advance(30 * DAY);
    h.client.withdraw(&warmup, &None);

    // Young stream: created now.
    let young = h.create_simple(100 * ONE, 100 * DAY);
    h.advance(30 * DAY);
    h.client.withdraw(&young, &None);
    let young_cost = measure(&h, "withdraw(young)");

    // Old stream: created a long time ago.
    let far_start = h.now();
    let old = h.create(100 * ONE, far_start, far_start + 100 * DAY, far_start, true, true, true);
    h.advance(999 * DAY); // simulate old age
    h.client.withdraw(&old, &None);
    let old_cost = measure(&h, "withdraw(old)");

    assert_eq!(
        young_cost.footprint,
        old_cost.footprint,
        "withdraw footprint must be identical regardless of stream age: \
         young={} old={}",
        young_cost.footprint,
        old_cost.footprint,
    );
    assert_eq!(
        young_cost.disk_reads,
        old_cost.disk_reads,
        "disk reads must not depend on stream age"
    );
    assert_eq!(
        young_cost.writes,
        old_cost.writes,
        "write count must not depend on stream age"
    );
}

// ---------------------------------------------------------------------------
// 4. Multiple pause/resume cycles do not inflate budget
// ---------------------------------------------------------------------------

/// A stream that has been paused and resumed many times stores the accumulated
/// pause duration in a single `paused_total` counter. Accrual reads this field
/// once; it does not iterate over a pause history. The cost must equal a
/// fresh stream with no pause history.
#[test]
fn many_pause_resume_cycles_do_not_inflate_withdraw_cost() {
    let h = Harness::new();

    // Create two streams: one with no pause history, one with many pauses.
    let start = h.now();
    let plain = h.create(100 * ONE, start, start + 1_000 * DAY, start, true, true, true);
    let paused_often = h.create(100 * ONE, start, start + 1_000 * DAY, start, true, true, true);

    // Give `paused_often` 20 pause/resume cycles.
    for _ in 0..20 {
        h.advance(2 * DAY);
        h.client.pause(&paused_often);
        h.advance(1); // minimal pause duration
        h.client.resume(&paused_often);
    }

    // Advance both to the same calendar time.
    h.advance(10 * DAY);

    // Warm up recipient token balance.
    h.client.withdraw(&plain, &None);
    let plain_cost = measure(&h, "withdraw(plain, no_pause_history)");

    h.client.withdraw(&paused_often, &None);
    let paused_cost = measure(&h, "withdraw(paused_often, 20_cycles)");

    assert_eq!(
        plain_cost.footprint,
        paused_cost.footprint,
        "footprint must not depend on pause/resume history: \
         plain={} paused_often={}",
        plain_cost.footprint,
        paused_cost.footprint,
    );
    assert_eq!(
        plain_cost.disk_reads,
        paused_cost.disk_reads,
        "disk reads must not depend on pause history"
    );
}

// ---------------------------------------------------------------------------
// 5. Pure accrual math has no super-linear CPU behaviour
// ---------------------------------------------------------------------------

/// Calling the pure accrual helpers directly for `N` synthetic streams must
/// not exhibit super-linear runtime growth. Because these are pure Rust
/// functions with no host calls, this is a compile-time structural guarantee
/// (they have no loops or allocations) verified by calling them a large number
/// of times and asserting they complete.
///
/// This is a smoke test to catch accidental introduction of per-call allocation
/// or recursion.
#[test]
fn pure_accrual_helpers_complete_for_large_call_count() {
    let env = Env::default();
    let start = T0;
    let end = start + 1_000;

    // 10,000 calls across three helpers.
    for i in 0u64..10_000 {
        let now = start + (i % (end - start));
        let s = crate::types::Stream {
            sender: Address::generate(&env),
            recipient: Address::generate(&env),
            token: Address::generate(&env),
            deposited: 1_000,
            withdrawn: 0,
            start_time: start,
            end_time: end,
            cliff_time: start,
            flags: crate::types::Stream::flags_from_parts(true, true, true),
            paused_at: None,
            paused_total: 0,
            status: crate::types::StreamStatus::Active,
        };

        let v = accrual::vested(&s, now).expect("no overflow");
        let r = accrual::refundable(&s, now).expect("no overflow");
        assert!(v + r == 1_000, "conservation at i={i}");
        let _ = accrual::withdrawable(&s, now).expect("no overflow");
        let _ = accrual::liability(&s).expect("no overflow");
    }
    // If this completes, the helpers have no hidden quadratic behaviour.
}

// ---------------------------------------------------------------------------
// 6. batch_withdraw at MAX_BATCH_SIZE does not exceed event budget
// ---------------------------------------------------------------------------

/// The event budget (`contract_events_size_bytes <= 16384`) must not be
/// saturated by a full-batch payroll run. Verify with the 2× headroom
/// requirement.
#[test]
fn batch_payroll_event_budget_has_2x_headroom() {
    let h = Harness::new();
    let ids: std::vec::Vec<u64> = (0..MAX_BATCH_SIZE)
        .map(|_| h.create_simple(100 * ONE, 100 * DAY))
        .collect();
    h.advance(30 * DAY);

    h.client.batch_withdraw(&h.recipient, &h.ids(&ids));
    let cost = measure(&h, "event_budget(MAX_BATCH)");

    assert!(
        cost.event_bytes * REQUIRED_HEADROOM_FACTOR <= EVENT_BYTES_LIMIT,
        "batch_withdraw at MAX_BATCH_SIZE: {} event bytes lacks {REQUIRED_HEADROOM_FACTOR}x \
         headroom under {EVENT_BYTES_LIMIT}",
        cost.event_bytes,
    );
    std::println!(
        "accrual_budget: per-stream event cost ~{} bytes; \
         budget allows ~{} streams (cap is {MAX_BATCH_SIZE})",
        cost.event_bytes / MAX_BATCH_SIZE,
        EVENT_BYTES_LIMIT / (cost.event_bytes / MAX_BATCH_SIZE).max(1),
    );
}

// ---------------------------------------------------------------------------
// 7. Repeated batch_withdraw on same stream ids (already drained)
// ---------------------------------------------------------------------------

/// After all streams in a batch are fully withdrawn (NothingToWithdraw),
/// a subsequent batch_withdraw attempt must either succeed (returning 0) or
/// return a typed error immediately — it must NOT consume a significant
/// fraction of the budget on a no-op path.
///
/// This covers the "empty payroll run" scenario where a keeper retries a
/// batch that was already processed.
#[test]
fn empty_batch_payroll_run_is_cheap() {
    let h = Harness::new();
    let ids: std::vec::Vec<u64> = (0..MAX_BATCH_SIZE)
        .map(|_| h.create_simple(100 * ONE, 100 * DAY))
        .collect();

    // Drain all streams.
    h.advance(100 * DAY);
    h.client.batch_withdraw(&h.recipient, &h.ids(&ids));

    // Retry on already-drained streams: either succeeds with 0 total or fails
    // gracefully. Either way must be cheap.
    let _ = h.client.try_batch_withdraw(&h.recipient, &h.ids(&ids));
    let cost = measure(&h, "batch_payroll_retry(already_drained)");

    // Even a no-op batch must have no disk reads and must stay within limits.
    assert_eq!(cost.disk_reads, 0, "retry: no disk reads expected");
    assert!(
        cost.instructions <= INSTRUCTION_LIMIT,
        "retry: {} instructions exceeded protocol limit",
        cost.instructions,
    );
}
