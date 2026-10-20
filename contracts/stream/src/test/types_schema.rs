//! Golden fixture parity for `types.rs`: bitmask values, status discriminants,
//! struct field order and storage-key discriminants must match
//! `tests/fixtures/event_snapshots/stream_types.json` (and `docs/ABI.md`).

use crate::types::flag;
use crate::{op, DataKey, Stream, StreamStatus};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/event_snapshots/stream_types.json");
const TYPES_SRC: &str = include_str!("../types.rs");

/// Value of `"name": <int>` inside the fixture (names are unique per section).
fn fixture_u32(name: &str) -> u32 {
    let needle = std::format!("\"{}\": ", name);
    let start = FIXTURE.find(&needle).expect("fixture key") + needle.len();
    let digits: std::string::String = FIXTURE[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().expect("fixture int")
}

#[test]
fn op_bits_match_fixture() {
    assert_eq!(op::WITHDRAW, fixture_u32("WITHDRAW"));
    assert_eq!(op::CANCEL, fixture_u32("CANCEL"));
    assert_eq!(op::PAUSE, fixture_u32("PAUSE"));
    assert_eq!(op::RESUME, fixture_u32("RESUME"));
    assert_eq!(op::TOP_UP, fixture_u32("TOP_UP"));
    assert_eq!(op::TRANSFER_RECIPIENT, fixture_u32("TRANSFER_RECIPIENT"));
    let all = [op::WITHDRAW, op::CANCEL, op::PAUSE, op::RESUME, op::TOP_UP, op::TRANSFER_RECIPIENT];
    assert_eq!(all.iter().fold(0, |a, b| a | b), 63, "op bits must be disjoint");
}

#[test]
fn flag_bits_and_packing_match_fixture() {
    assert_eq!(flag::CANCELLABLE, fixture_u32("CANCELLABLE"));
    assert_eq!(flag::PAUSABLE, fixture_u32("PAUSABLE"));
    assert_eq!(flag::TRANSFERABLE, fixture_u32("TRANSFERABLE"));
    for bits in 0..8u32 {
        let (c, p, t) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
        assert_eq!(Stream::flags_from_parts(c, p, t), bits);
    }
}

#[test]
fn status_discriminants_and_terminality() {
    assert_eq!(StreamStatus::Active as u32, fixture_u32("Active"));
    assert_eq!(StreamStatus::Paused as u32, fixture_u32("Paused"));
    assert_eq!(StreamStatus::Cancelled as u32, fixture_u32("Cancelled"));
    assert_eq!(StreamStatus::Depleted as u32, fixture_u32("Depleted"));
    assert!(!StreamStatus::Active.is_terminal());
    assert!(!StreamStatus::Paused.is_terminal());
    assert!(StreamStatus::Cancelled.is_terminal());
    assert!(StreamStatus::Depleted.is_terminal());
}

#[test]
fn data_key_discriminants_match_fixture() {
    for name in ["NextStreamId", "StreamCount", "Stream", "Delegate"] {
        let n = fixture_u32(name);
        assert!(
            TYPES_SRC.contains(&std::format!("{}", name)) && TYPES_SRC.contains(&std::format!(" = {},", n)),
            "{} = {}",
            name,
            n
        );
    }
    assert_eq!(DataKey::Stream(7), DataKey::Stream(7));
    assert_ne!(DataKey::StreamCount, DataKey::NextStreamId);
}

#[test]
fn stream_field_order_matches_fixture() {
    let body = TYPES_SRC
        .split("pub struct Stream {")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .expect("Stream body");
    let actual: std::vec::Vec<std::string::String> = body
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("pub ") && l.ends_with(','))
        .map(|l| {
            let l = l.trim_start_matches("pub ").trim_end_matches(',');
            let (n, t) = l.split_once(": ").unwrap();
            std::format!("{}:{}", n, t)
        })
        .collect();
    let start = FIXTURE.find("\"stream_fields\"").unwrap();
    let end = FIXTURE[start..].find(']').unwrap() + start;
    let expected: std::vec::Vec<std::string::String> = FIXTURE[start..end]
        .lines()
        .skip(1)
        .map(|l| std::string::String::from(l.trim().trim_matches(',').trim_matches('"')))
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(actual, expected);
}
