//! Tier 3 (analytic invariants; no oracle): `PerChannel<T>` is a pure
//! container with no numerical behavior to pin against a fixture. The tests
//! pin its construction rules (non-empty by construction) and its
//! shape-preserving `map`/`try_map` behavior, per
//! docs/specs/2026-07-15-room-dsp-design.md ("`PerChannel<T>` — new, in `lib.rs`").

use paraeq_dsp::{DspError, PerChannel};

#[test]
fn new_on_empty_vec_errs() {
    let err = PerChannel::<f64>::new(vec![]).unwrap_err();
    assert!(matches!(err, DspError::InvalidInput(_)), "got {err:?}");
}

#[test]
fn new_preserves_order_and_accessors_agree() {
    let p = PerChannel::new(vec![1.0, 2.0]).unwrap();
    assert_eq!(p.channels(), 2);
    assert_eq!(p.get(0), Some(&1.0));
    assert_eq!(p.get(1), Some(&2.0));
    assert_eq!(p.get(2), None, "out-of-range channel is None, not a panic");
    assert_eq!(p.as_slice(), &[1.0, 2.0]);
    let collected: Vec<f64> = p.iter().copied().collect();
    assert_eq!(collected, vec![1.0, 2.0]);
}

#[test]
fn splat_zero_channels_errs() {
    let err = PerChannel::splat(0.0f64, 0).unwrap_err();
    assert!(matches!(err, DspError::InvalidInput(_)), "got {err:?}");
}

#[test]
fn splat_then_map_round_trips() {
    let p = PerChannel::splat(7u32, 3).unwrap();
    let q = p.map(|&x| x);
    assert_eq!(q.channels(), 3);
    assert_eq!(q.as_slice(), &[7, 7, 7]);
}

#[test]
fn map_applies_per_channel_in_order() {
    let p = PerChannel::new(vec![1.0, 2.0, 3.0]).unwrap();
    let doubled = p.map(|x| x * 2.0);
    assert_eq!(doubled.as_slice(), &[2.0, 4.0, 6.0]);
}

#[test]
fn try_map_ok_collects_and_err_propagates() {
    let p = PerChannel::new(vec![1i32, -2, 3]).unwrap();
    let ok: Result<PerChannel<i32>, String> = p.try_map(|&x| Ok(x + 1));
    assert_eq!(ok.unwrap().as_slice(), &[2, -1, 4]);
    let err: Result<PerChannel<i32>, String> = p.try_map(|&x| {
        if x < 0 {
            Err(format!("negative: {x}"))
        } else {
            Ok(x)
        }
    });
    assert_eq!(err.unwrap_err(), "negative: -2");
}

#[test]
fn ref_into_iterator_matches_iter() {
    let p = PerChannel::new(vec![1u32, 2, 3]).unwrap();
    let mut total = 0;
    for x in &p {
        total += *x;
    }
    assert_eq!(total, 6);
}
