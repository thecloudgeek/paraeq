//! Tiers: the prototype-ported surface (parse/interpolate/anchor/
//! closest-match scoring) is Tier 1, fixture-pinned against
//! prototype/paraeq/correction/target_curves.py. The class filter, the
//! `# classes:` header rules, `compute_correction_multi`, and the room
//! target generator have NO prototype oracle and are Tier 3 (analytic
//! invariants), per docs/specs/2026-07-15-room-dsp-design.md
//! ("`targets.rs` — rework") and the decision-engine spec's Target selection.

mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::targets::{self, RoomTargetSpec, TargetCurve, TransducerClass, ANCHOR_FREQS};
use paraeq_dsp::PerChannel;
use std::path::PathBuf;

fn targets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../targets")
}

fn curve_from_case(c: &Case, fkey: &str, gkey: &str, name: &str) -> TargetCurve {
    TargetCurve {
        name: name.into(),
        frequencies: c.array(fkey),
        gains_db: c.array(gkey),
        category: None,
        classes: vec![],
        description: None,
        source: None,
    }
}

/// Two-point flat curve with the given class legality — for the custom-list
/// class-filter tests.
fn two_point(name: &str, classes: Vec<TransducerClass>) -> TargetCurve {
    TargetCurve {
        name: name.into(),
        frequencies: vec![20.0, 20000.0],
        gains_db: vec![0.0, 0.0],
        category: None,
        classes,
        description: None,
        source: None,
    }
}

const TWO_ROWS: &str = "100,0.0\n200,1.0\n";

// ---------------------------------------------------------------------------
// Tier 1 — fixture-pinned (oracle: prototype/paraeq/correction/target_curves.py)
// ---------------------------------------------------------------------------

#[test]
fn interpolation_matches_oracle_incl_clamp_edges() {
    let c = Case::load("targets", "harman_ie_interp");
    let curve = curve_from_case(&c, "curve_freqs", "curve_gains", "h");
    assert_allclose(
        &curve.interpolate(&c.array("dense")),
        &c.array("interp_dense"),
        1e-9,
        1e-9,
        "dense",
    );
    assert_allclose(
        &curve.interpolate(&c.array("edges")),
        &c.array("interp_edges"),
        1e-9,
        1e-9,
        "edges",
    );
}

#[test]
fn anchor_deviation_matches_oracle() {
    let c = Case::load("targets", "anchor_deviation");
    let base = targets::load_target_csv(&targets_dir().join("harman_ie_2019.csv")).unwrap();
    let out = targets::build_anchor_target(
        &base,
        &c.array("anchor_freqs"),
        &c.array("offsets_db"),
        "Custom",
    );
    assert_allclose(
        &out.frequencies,
        &c.array("result_freqs"),
        1e-12,
        1e-12,
        "grid",
    );
    assert_allclose(&out.gains_db, &c.array("result_gains"), 1e-9, 1e-9, "gains");
}

#[test]
fn match_closest_matches_oracle() {
    let c = Case::load("targets", "match_closest");
    let all = targets::list_targets(&targets_dir()).unwrap();
    assert_eq!(all.len(), 7, "builtin count");
    // The fixture's measured data is Harman-IE-shaped and the Python oracle
    // has no class filter, so passing InEar here keeps expected_name
    // reachable — luck, not design. The Tier-3 tests below pin the actual
    // class-filter contract the oracle cannot express.
    let best = targets::match_closest_target(
        TransducerClass::InEar,
        &c.array("measured_freqs"),
        &c.array("measured_db"),
        &all,
    )
    .unwrap();
    assert_eq!(best.name, c.scalar("expected_name").as_str().unwrap());
}

#[test]
fn csv_metadata_parsing_matches_prototype_fixtures() {
    let proto = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../prototype/tests/fixtures");
    let with_meta =
        targets::load_target_csv(&proto.join("target_curve_with_metadata.csv")).unwrap();
    assert!(with_meta.category.is_some(), "metadata parsed");
    let no_meta = targets::load_target_csv(&proto.join("target_curve_no_metadata.csv")).unwrap();
    assert_eq!(
        no_meta.name, "target_curve_no_metadata",
        "name falls back to file stem"
    );
    assert!(no_meta.category.is_none());
    // mixed/unknown metadata keys must not error:
    targets::load_target_csv(&proto.join("target_curve_unknown_and_mixed_case.csv")).unwrap();
}

#[test]
fn non_monotonic_target_csv_is_a_parse_error() {
    assert!(targets::parse_target_csv("100,0.0\n50,1.0\n", "bad").is_err());
    assert!(targets::parse_target_csv("100,0.0\n100,1.0\n", "dup").is_err());
}

#[test]
fn single_row_target_csv_is_a_parse_error() {
    assert!(targets::parse_target_csv("100,0.0\n", "one").is_err());
}

#[test]
fn non_finite_target_csv_is_a_parse_error() {
    // f64::parse accepts "NaN"/"inf", and NaN slips through the
    // strictly-increasing windows(2) check (NaN comparisons are false).
    assert!(targets::parse_target_csv("100,0.0\nNaN,1.0\n300,2.0\n", "nan").is_err());
    assert!(targets::parse_target_csv("100,0.0\n200,NaN\n300,2.0\n", "nan-gain").is_err());
    assert!(targets::parse_target_csv("100,0.0\n200,inf\n300,2.0\n", "inf-gain").is_err());
    assert!(targets::parse_target_csv("100,0.0\n200,1.0\ninf,2.0\n", "inf-freq").is_err());
}

#[test]
fn anchor_freqs_constant_matches_oracle() {
    assert_eq!(ANCHOR_FREQS.len(), 16);
    assert_eq!(ANCHOR_FREQS[0], 20.0);
    assert_eq!(ANCHOR_FREQS[15], 20000.0);
}

// ---------------------------------------------------------------------------
// Tier 3 — class filter (the category-error fix; no oracle CAN express these:
// generate_fixtures.py has no class filter at all)
// ---------------------------------------------------------------------------

#[test]
fn bookshelf_on_harman_shaped_data_never_returns_a_harman_curve() {
    // The actual contract: over the SAME Harman-IE-shaped fixture data, a
    // Bookshelf query must return the best room-legal curve — never a
    // headphone target whose synthesized ear gain (+8.3 dB at 3 kHz for
    // harman_oe_2018) a loudspeaker already delivers acoustically.
    let c = Case::load("targets", "match_closest");
    let all = targets::list_targets(&targets_dir()).unwrap();
    let best = targets::match_closest_target(
        TransducerClass::Bookshelf,
        &c.array("measured_freqs"),
        &c.array("measured_db"),
        &all,
    )
    .unwrap();
    assert!(
        !best.name.to_lowercase().contains("harman"),
        "class filter must exclude headphone targets for a room query, got {}",
        best.name
    );
    assert!(
        best.classes.contains(&TransducerClass::Bookshelf),
        "returned curve must be legal for the requested class"
    );
    assert!(
        ["B&K Room 1974", "Flat"].contains(&best.name.as_str()),
        "only the two Bookshelf-legal bundled curves are eligible, got {}",
        best.name
    );
}

#[test]
fn class_served_by_no_curve_is_err_not_a_wrong_curve() {
    let only_overear = vec![
        two_point("oe_a", vec![TransducerClass::OverEar]),
        two_point("oe_b", vec![TransducerClass::OverEar]),
    ];
    let res = targets::match_closest_target(
        TransducerClass::Bookshelf,
        &[100.0, 1000.0],
        &[0.0, 0.0],
        &only_overear,
    );
    assert!(
        res.is_err(),
        "no Bookshelf-legal candidate: must Err, never fall back to a wrong curve"
    );
}

#[test]
fn curve_with_no_classes_is_never_matched() {
    let bare = targets::parse_target_csv(TWO_ROWS, "bare").unwrap();
    assert!(
        bare.classes.is_empty(),
        "no `# classes:` and no known `# category:` means legal for NO class"
    );
    for class in [
        TransducerClass::Bookshelf,
        TransducerClass::Floorstander,
        TransducerClass::InEar,
        TransducerClass::OverEar,
    ] {
        assert!(
            targets::match_closest_target(
                class,
                &[100.0, 200.0],
                &[0.0, 0.0],
                std::slice::from_ref(&bare),
            )
            .is_err(),
            "class-less curve must not be matched for {class:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Tier 3 — `# classes:` header parsing (decision-engine spec, Target selection)
// ---------------------------------------------------------------------------

#[test]
fn classes_header_any_expands_to_all_four() {
    let curve = targets::parse_target_csv(&format!("# classes: any\n{TWO_ROWS}"), "t").unwrap();
    assert_eq!(
        curve.classes,
        vec![
            TransducerClass::Bookshelf,
            TransducerClass::Floorstander,
            TransducerClass::InEar,
            TransducerClass::OverEar,
        ]
    );
}

#[test]
fn classes_header_is_case_insensitive_and_deduped() {
    let curve = targets::parse_target_csv(
        &format!("# classes: inear, OVEREAR, InEar\n{TWO_ROWS}"),
        "t",
    )
    .unwrap();
    assert_eq!(
        curve.classes,
        vec![TransducerClass::InEar, TransducerClass::OverEar]
    );
}

#[test]
fn unknown_class_token_is_a_parse_error_not_a_silent_drop() {
    // A typo'd header must not quietly change which devices a curve is
    // legal for.
    assert!(
        targets::parse_target_csv(&format!("# classes: InEar, Speeker\n{TWO_ROWS}"), "typo")
            .is_err()
    );
    // Category spellings are not class tokens; only variant names are.
    assert!(
        targets::parse_target_csv(&format!("# classes: in-ear\n{TWO_ROWS}"), "hyphen").is_err()
    );
    // An empty token (stray comma) is malformed, not ignorable.
    assert!(
        targets::parse_target_csv(&format!("# classes: InEar,,OverEar\n{TWO_ROWS}"), "empty")
            .is_err()
    );
}

#[test]
fn missing_classes_header_backcompat_maps_category_never_any() {
    // Decision-engine spec back-compat map: over-ear -> {OverEar},
    // in-ear -> {InEar}, reference -> {InEar, OverEar} — deliberately NOT
    // room (`reference` curves like diffuse_field are head-measured and
    // carry ear gain). Anything else: legal for NO class, never `any`.
    let oe = targets::parse_target_csv(&format!("# category: over-ear\n{TWO_ROWS}"), "oe").unwrap();
    assert_eq!(oe.classes, vec![TransducerClass::OverEar]);
    let ie = targets::parse_target_csv(&format!("# category: in-ear\n{TWO_ROWS}"), "ie").unwrap();
    assert_eq!(ie.classes, vec![TransducerClass::InEar]);
    let rf =
        targets::parse_target_csv(&format!("# category: reference\n{TWO_ROWS}"), "rf").unwrap();
    assert_eq!(
        rf.classes,
        vec![TransducerClass::InEar, TransducerClass::OverEar]
    );
    let unknown =
        targets::parse_target_csv(&format!("# category: room\n{TWO_ROWS}"), "room").unwrap();
    assert!(
        unknown.classes.is_empty(),
        "an unmapped category must not silently become legal anywhere"
    );
    assert_eq!(
        unknown.category.as_deref(),
        Some("room"),
        "raw category is retained for round-tripping"
    );
}

#[test]
fn classes_header_supersedes_category() {
    let curve = targets::parse_target_csv(
        &format!("# category: over-ear\n# classes: Bookshelf, Floorstander\n{TWO_ROWS}"),
        "t",
    )
    .unwrap();
    assert_eq!(
        curve.classes,
        vec![TransducerClass::Bookshelf, TransducerClass::Floorstander]
    );
}

#[test]
fn flat_csv_is_legal_for_all_four_classes() {
    let flat = targets::load_target_csv(&targets_dir().join("flat.csv")).unwrap();
    assert_eq!(
        flat.classes,
        vec![
            TransducerClass::Bookshelf,
            TransducerClass::Floorstander,
            TransducerClass::InEar,
            TransducerClass::OverEar,
        ]
    );
}

#[test]
fn diffuse_field_csv_is_exactly_the_two_headphone_classes() {
    let df = targets::load_target_csv(&targets_dir().join("diffuse_field.csv")).unwrap();
    assert_eq!(
        df.classes,
        vec![TransducerClass::InEar, TransducerClass::OverEar],
        "5128 head-measured curve carries ear gain: never legal for rooms"
    );
}

// ---------------------------------------------------------------------------
// Tier 3 — room target generator
// ---------------------------------------------------------------------------

#[test]
fn room_target_zero_tilt_zero_shelf_is_flat() {
    let spec = RoomTargetSpec {
        shelf_gain_db: 0.0,
        tilt_db_per_oct: 0.0,
        ..Default::default()
    };
    let freqs = [20.0, 105.0, 500.0, 1000.0, 3000.0, 20000.0];
    let t = targets::build_room_target(&spec, &freqs).unwrap();
    for (f, g) in freqs.iter().zip(&t.gains_db) {
        assert!(g.abs() <= 1e-9, "expected flat at {f} Hz, got {g} dB");
    }
}

#[test]
fn room_target_tilt_between_1k_and_2k_is_exactly_the_tilt() {
    // shelf_gain_db = 0 removes the shelf's (constant-in-f) contribution
    // exactly, so g(2 kHz) - g(1 kHz) = tilt * log2(2) = -0.9 dB.
    let spec = RoomTargetSpec {
        shelf_gain_db: 0.0,
        ..Default::default()
    };
    assert_eq!(spec.tilt_db_per_oct, -0.9, "default tilt");
    let t = targets::build_room_target(&spec, &[20.0, 1000.0, 2000.0, 20000.0]).unwrap();
    let delta = t.gains_db[2] - t.gains_db[1];
    assert!(
        (delta + 0.9).abs() <= 1e-9,
        "expected -0.9 dB across 1k..2k, got {delta}"
    );
}

#[test]
fn room_target_rejects_out_of_domain_tilt() {
    let too_steep = RoomTargetSpec {
        tilt_db_per_oct: -2.0,
        ..Default::default()
    };
    assert!(
        targets::build_room_target(&too_steep, &[20.0, 20000.0]).is_err(),
        "-2.0 dB/oct is outside the -1.5..=0.0 domestic consensus domain"
    );
    let upward = RoomTargetSpec {
        tilt_db_per_oct: 0.5,
        ..Default::default()
    };
    assert!(
        targets::build_room_target(&upward, &[20.0, 20000.0]).is_err(),
        "an upward tilt is outside the domain too"
    );
}

#[test]
fn room_target_defaults_and_room_classes() {
    let spec = RoomTargetSpec::default();
    assert_eq!(spec.shelf_hz, 105.0);
    assert_eq!(spec.shelf_gain_db, 4.0);
    assert_eq!(spec.shelf_q, 0.71);
    assert_eq!(spec.tilt_db_per_oct, -0.9);
    assert_eq!(spec.pivot_hz, 1000.0);
    let t = targets::build_room_target(&spec, &[20.0, 1000.0, 20000.0]).unwrap();
    assert_eq!(
        t.classes,
        vec![TransducerClass::Bookshelf, TransducerClass::Floorstander],
        "a generated room target is legal for BOTH room kinds, nothing else"
    );
    assert!(!t.name.is_empty());
}

// ---------------------------------------------------------------------------
// Tier 3 — per-channel correction against ONE shared mono target
// ---------------------------------------------------------------------------

#[test]
fn compute_correction_multi_matches_mono_per_channel() {
    let left = vec![1.0, 2.0, 3.0];
    let right = vec![0.5, -1.0, 2.5];
    let target = vec![0.0, 1.0, -1.0];
    let measured = PerChannel::new(vec![left.clone(), right.clone()]).unwrap();
    let out = targets::compute_correction_multi(&measured, &target).unwrap();
    assert_eq!(out.channels(), 2);
    assert_eq!(
        out.get(0).unwrap(),
        &targets::compute_correction(&left, &target)
    );
    assert_eq!(
        out.get(1).unwrap(),
        &targets::compute_correction(&right, &target)
    );
    assert_ne!(
        out.get(0),
        out.get(1),
        "the SHARED target corrects each channel independently — that is what removes L/R imbalance"
    );
}

#[test]
fn compute_correction_multi_rejects_ragged_channels() {
    let ragged = PerChannel::new(vec![vec![1.0, 2.0, 3.0], vec![1.0, 2.0]]).unwrap();
    assert!(targets::compute_correction_multi(&ragged, &[0.0, 0.0, 0.0]).is_err());
    let wrong_len = PerChannel::new(vec![vec![1.0, 2.0]]).unwrap();
    assert!(
        targets::compute_correction_multi(&wrong_len, &[0.0, 0.0, 0.0]).is_err(),
        "every channel must match the target length exactly"
    );
}

// ---------------------------------------------------------------------------
// Tier 3 — display names (owner decision 2026-07-22)
// ---------------------------------------------------------------------------

#[test]
fn display_names_match_owner_decision() {
    assert_eq!(
        TransducerClass::Bookshelf.display_name(),
        "Bookshelf speakers"
    );
    assert_eq!(
        TransducerClass::Floorstander.display_name(),
        "Floorstanding speakers"
    );
    assert_eq!(TransducerClass::InEar.display_name(), "In-ear monitors");
    assert_eq!(
        TransducerClass::OverEar.display_name(),
        "Over-ear headphones"
    );
}
