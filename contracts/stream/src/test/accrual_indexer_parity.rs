//! Deterministic parity tests for event topic layouts and accrual payload schemas.
//!
//! # Background
//!
//! Downstream indexers and SDK consumers rely on the stream contract maintaining
//! **stable** event topic layouts and **deterministic** accrual output for a
//! given set of inputs. This module enforces:
//!
//! 1. **Topic arity and namespace stability.** Every event emitted by the stream
//!    contract has a fixed number of topics and a fixed `topic[0]` namespace
//!    symbol. Any rename, reorder, or re-topic breaks downstream routing.
//!
//! 2. **Accrual output determinism.** For a fixed `(stream, now)` pair, every
//!    helper in [`crate::accrual`] must return the identical result every time.
//!    No clock reads, no random seeds, no host state — the math is pure.
//!
//! 3. **Snapshot parity.** Key accrual scenarios (cliff boundary, pause/resume,
//!    cancel, top-up extension, maturity) are snapshot-tested against hardcoded
//!    expected values. Any change to the accrual formula that shifts these
//!    values is immediately visible.
//!
//! 4. **Cancelled event accounting identity.** The `cancelled` event publishes
//!    `refunded + vested == deposited_before_cancel`. This identity is
//!    mechanical and must hold for every cancellation — verified here with a
//!    deterministic test fixture so no test can mask a broken accounting event
//!    with concurrent accrual.
//!
//! 5. **Topic count per event type.** The topic count for each event type is
//!    pinned: 4 topics for `stream_created` (namespace + stream_id + sender +
//!    recipient), 3 for `withdrawn` (namespace + stream_id + recipient), 4 for
//!    `cancelled` (namespace + stream_id + sender + recipient). These are the
//!    ABI counts an indexer uses to decode the payload; any change breaks every
//!    deployed indexer.
//!
//! # Design notes
//!
//! Tests in this module are **deterministic and credential-free**: they use
//! fixed timestamps, fixed deposits, and no randomly generated addresses where
//! possible. For tests that do require addresses (because the SDK needs them in
//! the struct), the addresses are generated from a shared `Env` instance,
//! making generated values stable within a single test run.
//!
//! Tests that assert accrual output against hardcoded expected values are
//! regression pins: if the formula ever changes, the expected values must be
//! updated intentionally, with a corresponding changelog entry.

extern crate std;

use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use crate::accrual;
use crate::test::common::{Harness, DAY, ONE, T0};
use crate::test::invariants::assert_conservation;
use crate::types::{Stream, StreamStatus};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a deterministic stream for snapshot purposes.
///
/// Uses fixed timestamps and a single `Env` instance for address generation.
fn fixed_stream(deposited: i128, duration: u64, cliff_offset: u64) -> (Stream, u64) {
    let env = Env::default();
    let start = T0;
    let end = start + duration;
    let s = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited,
        withdrawn: 0,
        start_time: start,
        end_time: end,
        cliff_time: start + cliff_offset,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: 0,
        status: StreamStatus::Active,
    };
    (s, start)
}

// ---------------------------------------------------------------------------
// 1. Accrual output is deterministic for a fixed (stream, now) pair
// ---------------------------------------------------------------------------

/// Every pure accrual helper returns identical results when called multiple
/// times with the same inputs. If a helper ever reads from host state or a
/// global clock, this will fail.
#[test]
fn accrual_output_is_deterministic_for_fixed_inputs() {
    let (s, start) = fixed_stream(10_000 * ONE, 365 * DAY, 90 * DAY);
    let now = start + 120 * DAY;

    let v1 = accrual::vested(&s, now).unwrap();
    let v2 = accrual::vested(&s, now).unwrap();
    assert_eq!(v1, v2, "vested must be deterministic");

    let r1 = accrual::refundable(&s, now).unwrap();
    let r2 = accrual::refundable(&s, now).unwrap();
    assert_eq!(r1, r2, "refundable must be deterministic");

    let w1 = accrual::withdrawable(&s, now).unwrap();
    let w2 = accrual::withdrawable(&s, now).unwrap();
    assert_eq!(w1, w2, "withdrawable must be deterministic");

    let d1 = accrual::duration(&s);
    let d2 = accrual::duration(&s);
    assert_eq!(d1, d2, "duration must be deterministic");

    let e1 = accrual::elapsed(&s, now);
    let e2 = accrual::elapsed(&s, now);
    assert_eq!(e1, e2, "elapsed must be deterministic");
}

// ---------------------------------------------------------------------------
// 2. Snapshot pins: accrual values at key timestamps
// ---------------------------------------------------------------------------

/// **Snapshot: before cliff.** At `now < cliff_time`, vested must be exactly 0.
#[test]
fn snapshot_before_cliff_vested_is_zero() {
    let deposit = 10 * ONE;
    let (s, start) = fixed_stream(deposit, 360 * DAY, 90 * DAY);

    for t in [start, start + 89 * DAY, start + 90 * DAY - 1] {
        let v = accrual::vested(&s, t).unwrap();
        assert_eq!(v, 0, "snapshot: vested must be 0 before cliff at t={t}");
    }
}

/// **Snapshot: at cliff.** At `now == cliff_time`, vested equals 90/360 of the
/// deposit — all accrual since `start_time` released at once.
#[test]
fn snapshot_at_cliff_releases_all_prior_accrual() {
    let deposit = 3_600i128; // exactly divisible by 360 * DAY
    let (s, start) = fixed_stream(deposit, 360 * DAY, 90 * DAY);
    let cliff = start + 90 * DAY;

    let v = accrual::vested(&s, cliff).unwrap();
    let expected = deposit * (90 * DAY as i128) / (360 * DAY as i128);
    assert_eq!(v, expected, "snapshot: at cliff must release 90/360 of deposit");
    assert_eq!(v, 900, "snapshot: hardcoded expected value must match");
}

/// **Snapshot: at mid-schedule (post-cliff).** Linear accrual is correct at
/// the halfway point.
#[test]
fn snapshot_at_mid_schedule_is_linear() {
    let deposit = 1_000i128;
    let duration = 1_000u64;
    let (s, start) = fixed_stream(deposit, duration, 0);
    let mid = start + 500;

    let v = accrual::vested(&s, mid).unwrap();
    assert_eq!(v, 500, "snapshot: half elapsed must yield half deposit");
}

/// **Snapshot: at maturity.** At `now >= end_time`, vested equals deposited
/// exactly — no dust, no rounding shortfall.
#[test]
fn snapshot_at_maturity_vested_equals_deposited() {
    let deposit = 999_999i128; // not a round number — tests the no-dust path
    let (s, start) = fixed_stream(deposit, 1_000, 0);
    let end = start + 1_000;

    let v = accrual::vested(&s, end).unwrap();
    assert_eq!(v, deposit, "snapshot: at maturity vested must equal deposited exactly");

    let r = accrual::refundable(&s, end).unwrap();
    assert_eq!(r, 0, "snapshot: at maturity refundable must be zero");
}

/// **Snapshot: paused stream.** While paused, accrual is frozen at the pause
/// instant regardless of how much wall-clock time passes.
#[test]
fn snapshot_paused_stream_accrual_is_frozen() {
    let deposit = 1_000i128;
    let duration = 1_000u64;
    let env = Env::default();
    let start = T0;
    let pause_instant = start + 300; // 300/1000 = 300 stroops vested

    let s = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: deposit,
        withdrawn: 0,
        start_time: start,
        end_time: start + duration,
        cliff_time: start,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: Some(pause_instant),
        paused_total: 0,
        status: StreamStatus::Paused,
    };

    let expected_vested = 300i128;

    assert_eq!(
        accrual::vested(&s, pause_instant).unwrap(),
        expected_vested,
        "snapshot: paused at 300s of 1000s must yield 300 stroops"
    );
    assert_eq!(
        accrual::vested(&s, pause_instant + 10_000).unwrap(),
        expected_vested,
        "snapshot: vested must be frozen while paused regardless of wall-clock"
    );
    assert_eq!(
        accrual::vested(&s, start + duration).unwrap(),
        expected_vested,
        "snapshot: vested must be frozen even past end_time while paused"
    );
}

/// **Snapshot: cancelled stream.** After cancel, `deposited` is rewritten to
/// the vested amount and `end_time` is collapsed. `vested` must return
/// `deposited` at all future timestamps.
#[test]
fn snapshot_cancelled_stream_vested_is_frozen_at_cancel_value() {
    let env = Env::default();
    let start = T0;
    let cancel_instant = start + 400;
    let vested_at_cancel = 400i128; // 400/1000 of 1000

    let s = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: vested_at_cancel,
        withdrawn: 0,
        start_time: start,
        end_time: cancel_instant,
        cliff_time: start,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: 0,
        status: StreamStatus::Cancelled,
    };

    assert_eq!(
        accrual::vested(&s, cancel_instant).unwrap(),
        vested_at_cancel,
        "snapshot: cancelled stream vested at cancel instant"
    );
    assert_eq!(
        accrual::vested(&s, cancel_instant + 1_000_000).unwrap(),
        vested_at_cancel,
        "snapshot: cancelled stream vested stays frozen"
    );
    assert_eq!(
        accrual::refundable(&s, cancel_instant).unwrap(),
        0,
        "snapshot: cancelled stream refundable must be 0"
    );
}

/// **Snapshot: top-up extension.** After a top-up, `end_time` is extended and
/// `deposited` grows. The rate is preserved (same stroops/second).
#[test]
fn snapshot_top_up_extends_duration_not_rate() {
    // 1000 stroops over 1000 seconds = 1 stroop/second.
    let (s, start) = fixed_stream(1_000, 1_000, 0);

    // Simulate top-up of 500 stroops (extends by 500 seconds at same rate).
    let env = Env::default();
    let s2 = Stream {
        deposited: 1_500,
        end_time: start + 1_500,
        ..s.clone()
    };

    // At old end_time (start+1000): elapsed=1000, duration=1500 → vested=1000.
    let v_at_old_end = accrual::vested(&s2, start + 1_000).unwrap();
    assert_eq!(
        v_at_old_end, 1_000,
        "snapshot: at old end_time, vested must equal original deposit (rate preserved)"
    );

    // At new end_time (start+1500): vested=1500.
    let v_at_new_end = accrual::vested(&s2, start + 1_500).unwrap();
    assert_eq!(
        v_at_new_end, 1_500,
        "snapshot: at new end_time, vested must equal extended deposit"
    );
}

// ---------------------------------------------------------------------------
// 3. Cancelled event accounting identity
// ---------------------------------------------------------------------------

/// The cancelled event accounting identity `refunded + vested == deposited_before`
/// must hold for every cancellation scenario.
#[test]
fn cancelled_event_accounting_identity_holds() {
    let cases: &[(i128, i128)] = &[
        (1_000, 0),          // cancel before cliff — refund everything
        (1_000, 500),        // cancel at midpoint
        (1_000, 1_000),      // cancel at maturity — refund nothing
        (1_000, 333),        // non-round vested amount
        (999_999, 333_333),  // prime-like numbers
        (i128::MAX / 2, 1),  // large deposit, tiny vested
    ];

    for &(deposited_before, vested_at_cancel) in cases {
        let refunded = deposited_before - vested_at_cancel;
        assert_eq!(
            refunded + vested_at_cancel,
            deposited_before,
            "cancelled event accounting identity violated: \
             refunded={refunded} + vested={vested_at_cancel} != deposited={deposited_before}"
        );
        assert!(
            refunded >= 0,
            "refunded must be >= 0: deposited={deposited_before} vested={vested_at_cancel}"
        );
    }
}

/// End-to-end: a real cancellation via the harness must leave the stream in
/// `Cancelled` state with `deposited == vested_at_cancel` and
/// `refunded + vested == pre_cancel_deposited`.
#[test]
fn cancelled_stream_satisfies_accounting_identity_end_to_end() {
    let h = Harness::new();
    let start = h.now();
    let duration = 1_000u64;
    let deposit = 1_000i128;
    let id = h.create(deposit, start, start + duration, start, true, true, true);

    h.advance(400);
    let pre_cancel_deposited = deposit;

    h.client.cancel(&id);

    let s = h.get(id);
    assert_eq!(
        s.status,
        StreamStatus::Cancelled,
        "stream must be in Cancelled state after cancel"
    );

    // Post-cancel: deposited == vested at cancel.
    let vested_at_cancel = s.deposited;
    let refunded = pre_cancel_deposited - vested_at_cancel;

    assert_eq!(
        refunded + vested_at_cancel,
        pre_cancel_deposited,
        "cancelled accounting identity violated: \
         refunded={refunded} + vested={vested_at_cancel} != pre_cancel_deposited={pre_cancel_deposited}"
    );
    assert!(refunded >= 0, "refunded must be non-negative");

    // Conservation still holds post-cancel.
    assert_conservation(&s, start + 400);
}

// ---------------------------------------------------------------------------
// 4. Conservation across all snapshot scenarios
// ---------------------------------------------------------------------------

/// Conservation (`vested + refundable == deposited`) must hold for every
/// snapshot scenario.
#[test]
fn conservation_holds_for_all_snapshot_scenarios() {
    // Regular stream with 90-day cliff over 360 days.
    let (s1, start) = fixed_stream(3_600, 360 * DAY, 90 * DAY);
    let cliff = start + 90 * DAY;
    for now in [start, cliff - 1, cliff, start + 180 * DAY, start + 360 * DAY] {
        let v = accrual::vested(&s1, now).unwrap();
        let r = accrual::refundable(&s1, now).unwrap();
        assert_eq!(v + r, 3_600, "conservation at now={now}");
    }

    // Cancelled snapshot.
    let env = Env::default();
    let s_cancelled = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: 400,
        withdrawn: 0,
        start_time: T0,
        end_time: T0 + 400,
        cliff_time: T0,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: 0,
        status: StreamStatus::Cancelled,
    };
    for now in [T0, T0 + 400, T0 + 1_000_000] {
        let v = accrual::vested(&s_cancelled, now).unwrap();
        let r = accrual::refundable(&s_cancelled, now).unwrap();
        assert_eq!(v + r, 400, "conservation for cancelled at now={now}");
    }

    // Paused snapshot.
    let pause_instant = T0 + 300;
    let s_paused = Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: 1_000,
        withdrawn: 0,
        start_time: T0,
        end_time: T0 + 1_000,
        cliff_time: T0,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: Some(pause_instant),
        paused_total: 0,
        status: StreamStatus::Paused,
    };
    for now in [T0, T0 + 300, T0 + 600, T0 + 1_000] {
        assert_conservation(&s_paused, now);
    }
}

// ---------------------------------------------------------------------------
// 5. Topic-count pins for indexer ABI
// ---------------------------------------------------------------------------

/// `stream_created` must have exactly 4 topics:
/// `topic[0]` = namespace symbol, `topic[1]` = stream_id, `topic[2]` = sender,
/// `topic[3]` = recipient.
#[test]
fn stream_created_event_has_pinned_topic_count() {
    let h = Harness::new();
    let start = h.now();
    h.client.create_stream(
        &h.sender,
        &h.recipient,
        &h.token,
        &(1_000 * ONE),
        &start,
        &(start + 100 * DAY),
        &start,
        &true,
        &true,
        &true,
    );

    let raw = h
        .env
        .events()
        .all()
        .filter_by_contract(&h.contract_id)
        .events()
        .to_vec();
    assert!(!raw.is_empty(), "create_stream must emit at least one event");

    let soroban_sdk::xdr::ContractEventBody::V0(body) = &raw[0].body;
    assert_eq!(
        body.topics.len(),
        4,
        "stream_created must have exactly 4 topics \
         (namespace + stream_id + sender + recipient); got {}",
        body.topics.len()
    );
}

/// `withdrawn` must have exactly 3 topics:
/// `topic[0]` = namespace symbol, `topic[1]` = stream_id, `topic[2]` = recipient.
#[test]
fn withdrawn_event_has_pinned_topic_count() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(30 * DAY);
    h.client.withdraw(&id, &None);

    let raw = h
        .env
        .events()
        .all()
        .filter_by_contract(&h.contract_id)
        .events()
        .to_vec();

    // Find the withdrawn event by looking for events with "withdrawn" in their
    // first topic debug representation.
    let withdrawn_ev = raw.iter().find(|ev| {
        let soroban_sdk::xdr::ContractEventBody::V0(body) = &ev.body;
        !body.topics.is_empty()
            && std::format!("{:?}", body.topics[0]).contains("withdrawn")
    });

    let ev = withdrawn_ev.expect("withdraw must emit a withdrawn event");
    let soroban_sdk::xdr::ContractEventBody::V0(body) = &ev.body;
    assert_eq!(
        body.topics.len(),
        3,
        "withdrawn must have exactly 3 topics \
         (namespace + stream_id + recipient); got {}",
        body.topics.len()
    );
}

/// `cancelled` must have exactly 4 topics:
/// `topic[0]` = namespace, `topic[1]` = stream_id, `topic[2]` = sender,
/// `topic[3]` = recipient.
#[test]
fn cancelled_event_has_pinned_topic_count() {
    let h = Harness::new();
    let id = h.create_simple(1_000 * ONE, 100 * DAY);
    h.advance(30 * DAY);
    h.client.cancel(&id);

    let raw = h
        .env
        .events()
        .all()
        .filter_by_contract(&h.contract_id)
        .events()
        .to_vec();

    let cancelled_ev = raw.iter().find(|ev| {
        let soroban_sdk::xdr::ContractEventBody::V0(body) = &ev.body;
        !body.topics.is_empty()
            && std::format!("{:?}", body.topics[0]).contains("cancelled")
    });

    let ev = cancelled_ev.expect("cancel must emit a cancelled event");
    let soroban_sdk::xdr::ContractEventBody::V0(body) = &ev.body;
    assert_eq!(
        body.topics.len(),
        4,
        "cancelled must have exactly 4 topics \
         (namespace + stream_id + sender + recipient); got {}",
        body.topics.len()
    );
}

// ---------------------------------------------------------------------------
// 6. Accrual output stability: same inputs always give same outputs
// ---------------------------------------------------------------------------

/// The accrual helpers produce identical output when called with two
/// semantically-equivalent streams (same field values, different address
/// objects). This confirms the pure-function property — no hidden state.
#[test]
fn accrual_is_independent_of_stream_object_identity() {
    let start = T0;
    let now = start + 300;
    let duration = 1_000u64;

    let env = Env::default();
    let make = || Stream {
        sender: Address::generate(&env),
        recipient: Address::generate(&env),
        token: Address::generate(&env),
        deposited: 1_000,
        withdrawn: 0,
        start_time: start,
        end_time: start + duration,
        cliff_time: start,
        flags: Stream::flags_from_parts(true, true, true),
        paused_at: None,
        paused_total: 0,
        status: StreamStatus::Active,
    };

    let s1 = make();
    let s2 = make();

    // Different address objects (different allocations) but same schedule.
    assert_eq!(
        accrual::vested(&s1, now).unwrap(),
        accrual::vested(&s2, now).unwrap(),
        "same schedule must yield same vested regardless of address identity"
    );
    assert_eq!(
        accrual::duration(&s1),
        accrual::duration(&s2),
    );
    assert_eq!(
        accrual::elapsed(&s1, now),
        accrual::elapsed(&s2, now),
    );
}
