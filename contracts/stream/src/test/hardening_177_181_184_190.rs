//! Regression pins for archival recovery, event invariants and flag packing.

use crate::accrual::{classify_read, recovery_for, remaining_to_apply, ReadFreshness, Recovery};
use crate::error::Error;
use crate::events::{check_cancelled_invariants, EVENT_TOPIC_NAMES};
use crate::types::{flag, Stream};

#[test]
fn skew_is_classified_not_combined() {
    assert_eq!(classify_read(100, 100), ReadFreshness::Fresh);
    assert_eq!(classify_read(101, 100), ReadFreshness::Fresh);
    assert_eq!(classify_read(95, 100), ReadFreshness::Stale { behind_by: 5 });
    assert_eq!(classify_read(0, u32::MAX), ReadFreshness::Stale { behind_by: u32::MAX });
}

#[test]
fn archived_vs_never_existed() {
    assert_eq!(recovery_for(true, 3, 10), Recovery::None);
    assert_eq!(recovery_for(false, 3, 10), Recovery::RestoreFootprint);
    assert_eq!(recovery_for(false, 10, 10), Recovery::NeverExisted);
}

#[test]
fn retry_is_idempotent() {
    assert_eq!(remaining_to_apply(100, 40), 60);
    assert_eq!(remaining_to_apply(100, 100), 0);
    assert_eq!(remaining_to_apply(100, 150), 0);
    assert_eq!(remaining_to_apply(i128::MIN, i128::MAX), 0);
}

#[test]
fn cancel_conservation_guarded() {
    assert_eq!(check_cancelled_invariants(100, 60, 40, 10), Ok(()));
    assert_eq!(check_cancelled_invariants(100, 60, 41, 10), Err(Error::InvalidAmount));
    assert_eq!(check_cancelled_invariants(100, -1, 101, 0), Err(Error::InvalidAmount));
    assert_eq!(check_cancelled_invariants(0, i128::MAX, 1, 0), Err(Error::Overflow));
}

#[test]
fn flags_pack_and_validate() {
    assert_eq!(flag::ALL, 0b111);
    for bits in 0u8..8 {
        let f = Stream::flags_from_parts(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
        assert_eq!(f, bits as u32);
        assert!(Stream::flags_valid(f));
    }
    assert!(!Stream::flags_valid(1 << 3));
}

#[test]
fn topic_inventory_unique() {
    let mut v = EVENT_TOPIC_NAMES.to_vec();
    v.sort();
    v.dedup();
    assert_eq!(v.len(), EVENT_TOPIC_NAMES.len());
}
