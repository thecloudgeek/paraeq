//! The `fixtures/decide/` characterization harness — **pre-bless**.
//!
//! The spec's own words for what these are: "`fixtures/decide/<case>/bundle.json`
//! plus `expected.json`, committed … These are **not** oracle fixtures — they
//! are characterization fixtures whose expected values are reviewed by the
//! owner once and then frozen. A diff in `expected.json` is a policy change and
//! must be argued for in the PR, which is the entire point."
//!
//! **No `expected.json` exists yet, and that is the designed state of this
//! branch.** The bless is owner-gated and must not run until every open value
//! call it would photograph is settled — a freeze over values the owner has not
//! ruled on is the exact failure the mechanism exists to prevent, and a freeze
//! that re-blesses twice in its first week trains everyone to rubber-stamp the
//! diff. So the two assertions that compare against `expected.json`
//! **report and pass** when it is absent, and the eight inputs, the freeze
//! manifest and the bless mechanism are all asserted today.
//!
//! Four things hold the freeze together, and all four are here:
//!
//! 1. `decide_reproduces_expected_json_byte_for_byte` — the characterization
//!    assertion. Byte-exactness is available because `float_roundtrip` is on
//!    workspace-wide, enabled specifically so this comparison can be an
//!    `assert_eq!` on text rather than an epsilon walk.
//! 2. `expected_json_is_canonical` — reparse + reserialize == the file bytes,
//!    which catches a hand-edit, a key reorder or a float-format drift that (1)
//!    would miss whenever `decide()` happens to agree semantically.
//! 3. `the_freeze_manifest_matches_every_file_on_disk` — **the only thing
//!    protecting the INPUTS.** (1) does not: a silently edited `bundle.json`
//!    would simply bless to a different `expected.json` and look legitimate.
//! 4. `blessing_is_off_unless_the_env_var_is_set` — CI sets no such variable,
//!    so CI can never silently re-bless.

mod common;

use common::golden::{self, BlessMode, GoldenCase, CASES, REQUIRED_FILES};
use paraeq_decide::{decide, MeasurementBundle};
use std::ffi::OsString;

/// Every named case is present, with the two files a case is required to carry
/// before the bless.
///
/// The list is hard-coded in [`golden::CASES`] rather than read from the
/// directory, for the reason `crates/paraeq-dsp/tests/fixtures_smoke.rs` spells
/// its own list out: a dropped directory must fail loudly instead of silently
/// shrinking the set the owner reviewed.
#[test]
fn every_named_case_is_present_with_its_required_files() {
    for case in CASES {
        let dir = golden::case_dir(case);
        assert!(dir.is_dir(), "fixtures/decide/{case}/ is missing");
        for file in REQUIRED_FILES {
            assert!(
                dir.join(file).is_file(),
                "fixtures/decide/{case}/{file} is missing"
            );
        }
        let sidecars: Vec<String> = golden::case_files(case)
            .into_iter()
            .filter(|f| f.starts_with("ir/"))
            .collect();
        assert!(
            !sidecars.is_empty(),
            "fixtures/decide/{case}/ir/ holds no impulse-response sidecars"
        );
    }
}

/// `fixtures/decide/` holds the reviewed set and nothing else.
///
/// Without this a ninth directory can appear, be tested by nothing, and still
/// read as part of the frozen set — which is how "owner-reviewed once" quietly
/// becomes "owner-reviewed some of".
#[test]
fn no_case_directory_exists_that_the_case_list_does_not_name() {
    let mut found: Vec<String> = std::fs::read_dir(golden::decide_dir())
        .expect("fixtures/decide/ is readable")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.is_dir())
        .map(|path| {
            path.file_name()
                .expect("a directory has a name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    assert_eq!(found, CASES.to_vec(), "fixtures/decide/ directory listing");
}

/// Each `bundle.json` hydrates into a `MeasurementBundle`, and every sidecar's
/// declared `len` is its byte length divided by eight.
///
/// The length check is inside [`golden::read_f64_le`], so it runs on every load
/// everywhere, not only here — a truncated sidecar is a loud failure at the
/// point of reading rather than a short curve fifty lines later.
#[test]
fn bundle_json_deserializes_to_a_measurement_bundle() {
    for case in CASES {
        let loaded = GoldenCase::load(case);
        let bundle = loaded.bundle();
        assert!(
            !bundle.positions.is_empty(),
            "{case}: a bundle with no positions is not an input"
        );
        for position in &bundle.positions {
            assert!(
                !position.ir.samples.is_empty(),
                "{case}: position {} has no channels",
                position.index
            );
            for channel in &position.ir.samples {
                assert!(
                    position.ir.peak < channel.len(),
                    "{case}: position {} peak {} is outside its own samples",
                    position.index,
                    position.ir.peak
                );
                assert!(
                    channel.iter().all(|s| s.is_finite()),
                    "{case}: position {} carries a non-finite sample",
                    position.index
                );
            }
        }
    }
}

/// A `Position` without `routing` must fail to deserialize, and that is the
/// WANTED behaviour rather than an accident of the derive.
///
/// `Position::routing` is non-`Option` and carries no `#[serde(default)]`:
/// defaulting a missing routing to `Both` would, on a coupler run, difference a
/// per-ear baseline against an L+R sum — the exact failure the verification
/// routing fence exists to refuse. `MeasurementBundle` derives plain
/// `Deserialize` with no field defaults, so the omission is a hard error.
#[test]
fn a_bundle_missing_position_routing_fails_to_deserialize() {
    let loaded = GoldenCase::load(CASES[0]);
    let mut envelope = loaded.envelope.clone();
    golden::hydrate(&mut envelope, &loaded.dir);

    // The same bundle loads with `routing` present…
    serde_json::from_value::<MeasurementBundle>(envelope.clone())
        .expect("the unmodified envelope is a MeasurementBundle");

    // …and does not without it.
    envelope["positions"][0]
        .as_object_mut()
        .expect("a position is an object")
        .remove("routing")
        .expect("routing was present");
    let error = serde_json::from_value::<MeasurementBundle>(envelope)
        .expect_err("a position without routing must not deserialize");
    assert!(
        error.to_string().contains("routing"),
        "the failure must name the missing field, got: {error}"
    );
}

/// The characterization assertion, and the bless.
///
/// Compared as TEXT, byte for byte. `expected.json` absent ⇒ report and pass:
/// B10 owns the bless and it is owner-gated (see this file's header).
#[test]
fn decide_reproduces_expected_json_byte_for_byte() {
    let mode = golden::bless_mode_from_env();
    let root = golden::bless_root(&mode);
    let mut skipped = Vec::new();

    for case in CASES {
        let loaded = GoldenCase::load(case);
        let produced = golden::canonical_json(&decide(&loaded.bundle()));

        if let Some(root) = &root {
            let path = root.join(case).join("expected.json");
            std::fs::create_dir_all(path.parent().expect("a parent directory"))
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            std::fs::write(&path, &produced).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            eprintln!("blessed {}", path.display());
            continue;
        }

        match loaded.expected_text() {
            None => skipped.push(case),
            Some(expected) => assert_eq!(
                produced,
                expected,
                "{case}: decide() no longer reproduces expected.json. \
                 A diff here is a POLICY CHANGE and must be argued for in the PR. \
                 If the change is intended, re-bless with \
                 `{}=1 cargo test -p paraeq-decide --test test_golden_bundles`.",
                golden::BLESS_VAR
            ),
        }
    }

    if mode == BlessMode::Repo {
        // The bless rewrites expected.json AND the manifest in one pass, so the
        // PR shows the policy change and the re-freeze together instead of as
        // two commits a reviewer has to correlate.
        let manifest = golden::build_manifest(None, None);
        std::fs::write(golden::manifest_path(), golden::canonical_json(&manifest))
            .expect("fixtures/decide/manifest.json is writable");
        eprintln!("rebuilt {}", golden::manifest_path().display());
    }

    if !skipped.is_empty() {
        eprintln!(
            "SKIPPED the characterization comparison for {} case(s) with no expected.json: {}. \
             This is the designed pre-bless state — B10 runs the owner-gated bless.",
            skipped.len(),
            skipped.join(", ")
        );
    }
}

/// Reparsing `expected.json` and reserializing it must reproduce the file
/// bytes.
///
/// This is what makes "a diff in `expected.json` is a policy change" true of
/// the FILE and not only of `decide()`: a hand-edit, a re-indent or a key
/// reorder that the characterization test would tolerate whenever `decide()`
/// happens to agree semantically fails here instead. It also exercises
/// `AuthorityCurve`'s sealed `try_from` reload — a blessed curve that cannot
/// round-trip back into a valid curve fails at the parse.
#[test]
fn expected_json_is_canonical() {
    let mut skipped = Vec::new();
    for case in CASES {
        let loaded = GoldenCase::load(case);
        let Some(text) = loaded.expected_text() else {
            skipped.push(case);
            continue;
        };
        let parsed: paraeq_decide::DecisionSet = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{case}/expected.json is not a DecisionSet: {e}"));
        assert_eq!(
            golden::canonical_json(&parsed),
            text,
            "{case}/expected.json is not in the canonical style \
             (serde_json::to_string_pretty plus a trailing newline)"
        );
    }
    if !skipped.is_empty() {
        eprintln!(
            "SKIPPED the canonicalization check for {} case(s) with no expected.json — pre-bless.",
            skipped.len()
        );
    }
}

/// The freeze manifest agrees with every file on disk, in both directions.
///
/// The characterization test does not protect the inputs: a silently edited
/// `bundle.json` blesses to a different `expected.json` and looks legitimate.
/// This is the check that makes the INPUTS frozen too.
#[test]
fn the_freeze_manifest_matches_every_file_on_disk() {
    let manifest = golden::read_manifest();
    assert_eq!(manifest["algorithm"], "fnv1a64");
    assert_eq!(manifest["schema_version"], 1);

    let cases = manifest["cases"]
        .as_object()
        .expect("the manifest carries a `cases` map");
    let mut named: Vec<&str> = cases.keys().map(String::as_str).collect();
    named.sort();
    assert_eq!(named, CASES.to_vec(), "manifest case list");

    for case in CASES {
        let listed = cases[case]
            .as_object()
            .expect("each case maps file paths to digests");
        let dir = golden::case_dir(case);

        for (relative, entry) in listed {
            let bytes = std::fs::read(dir.join(relative))
                .unwrap_or_else(|e| panic!("{case}/{relative} is in the manifest but: {e}"));
            assert_eq!(
                golden::digest_entry(&bytes),
                *entry,
                "{case}/{relative} does not match its frozen digest. \
                 Regenerate the inputs rather than editing them: \
                 `cargo test -p paraeq-decide --test test_golden_bundles -- --ignored \
                 regenerating_the_inputs_rewrites_every_bundle_and_sidecar`."
            );
        }

        let unlisted: Vec<String> = golden::case_files(case)
            .into_iter()
            .filter(|f| !listed.contains_key(f))
            .collect();
        // `expected.json` is unlisted only until B10's owner-gated bless, which
        // writes the file and its digest in the same pass. Anything else
        // unlisted is a file that entered the frozen set without review.
        assert!(
            unlisted.is_empty() || unlisted == vec!["expected.json".to_string()],
            "{case}: files on disk that the freeze manifest does not cover: {unlisted:?}"
        );
    }
}

/// The bless is off unless one of its three named variables says otherwise, and
/// nothing else can turn it on.
///
/// Driven through [`golden::bless_mode`]'s injected lookup rather than the
/// process environment, which integration tests share across threads: this
/// asserts the gate itself instead of asserting the state of the machine it
/// happens to be running on. CI sets none of the three.
#[test]
fn blessing_is_off_unless_the_env_var_is_set() {
    /// One fake environment holding a single variable.
    fn only(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<OsString> {
        move |asked: &str| {
            if asked == name {
                Some(OsString::from(value))
            } else {
                None
            }
        }
    }

    assert_eq!(golden::bless_mode(&|_: &str| None), BlessMode::Off);
    assert_eq!(
        golden::bless_mode(&only(golden::BLESS_VAR, "1")),
        BlessMode::Repo
    );
    assert_eq!(
        golden::bless_mode(&only(golden::BLESS_ALIAS, "1")),
        BlessMode::Repo
    );
    // Set-but-empty and set-to-zero are OFF: an inherited empty value must not
    // re-bless a freeze.
    assert_eq!(
        golden::bless_mode(&only(golden::BLESS_VAR, "")),
        BlessMode::Off
    );
    assert_eq!(
        golden::bless_mode(&only(golden::BLESS_VAR, "0")),
        BlessMode::Off
    );
    // An unrelated variable cannot reach the gate.
    assert_eq!(
        golden::bless_mode(&only("PARAEQ_SOMETHING_ELSE", "1")),
        BlessMode::Off
    );
    // The out-directory form implies a bless, to that directory and nowhere
    // else — which is how a dry run proves the mechanism without touching the
    // repo.
    assert_eq!(
        golden::bless_mode(&only(golden::BLESS_OUT_VAR, "/tmp/paraeq-dry-bless")),
        BlessMode::To(std::path::PathBuf::from("/tmp/paraeq-dry-bless"))
    );
}

/// Regenerate every `bundle.json`, every `ir/*.f64` sidecar and the freeze
/// manifest from the seeded generator.
///
/// `#[ignore]`d because it WRITES: it is a maintenance command, not an
/// assertion, and the assertion that matters is that running it leaves
/// `git status` clean. The inputs are authored here rather than by hand, which
/// is what keeps `fixtures/decide/`'s carve-out from CLAUDE.md's
/// generated-only rule a change of GENERATOR rather than a change of policy.
///
/// `cargo test -p paraeq-decide --test test_golden_bundles -- --ignored \
///  regenerating_the_inputs_rewrites_every_bundle_and_sidecar`
#[test]
#[ignore = "writes fixtures/decide/; run deliberately and commit the diff"]
fn regenerating_the_inputs_rewrites_every_bundle_and_sidecar() {
    common::generator::write_all();
}

/// Every bundle carries the fields the freeze depends on, and the set as a
/// whole covers both branches of the ones that have two.
///
/// These are AUTHORING requirements, and they are asserted rather than
/// described for the reason the whole freeze exists: a field that ships
/// serde-visible and ungraded by any golden case is a field whose wire shape
/// nobody notices breaking. Every clause here names what it protects.
#[test]
fn every_bundle_carries_the_fields_the_freeze_depends_on() {
    let mut with_skew_estimate = 0usize;
    let mut without_skew_estimate = 0usize;
    let mut with_verification = 0usize;

    for case in CASES {
        let bundle = GoldenCase::load(case).bundle();

        // `Position.routing` and `Position.capture` are both non-`Option`
        // additions: hydrating at all proves they are present, and the
        // per-position walk proves they are present on EVERY position rather
        // than on the first.
        for (index, position) in bundle.positions.iter().enumerate() {
            assert_eq!(position.index, index, "{case}: position indices are dense");
            assert!(
                position.capture.peak_dbfs.is_finite() && position.capture.rms_dbfs.is_finite(),
                "{case}: position {index} carries a non-finite capture readout, which \
                 serde_json writes as null and cannot read back"
            );
        }

        // `CapturePlan`'s additions. The clock estimate has two branches and the
        // set must cover both, because the two-clock warning fires on exactly
        // one of them.
        assert!(bundle.capture.input_present, "{case}: input_present");
        assert!(bundle.capture.self_excluded, "{case}: self_excluded");
        assert!(
            bundle.capture.chain_sensitivity_spl_per_dbfs.is_some(),
            "{case}: a bundle with no solved sensitivity cannot run the class cross-check"
        );
        match bundle.capture.clock_skew_ppm {
            Some(ppm) => {
                assert!(ppm.is_finite(), "{case}: clock_skew_ppm");
                assert!(
                    bundle.capture.clock_adjusted,
                    "{case}: an estimate was formed but the resample did not run"
                );
                with_skew_estimate += 1;
            }
            None => {
                assert!(
                    !bundle.capture.clock_adjusted,
                    "{case}: no estimate was formed, so nothing could have been adjusted"
                );
                without_skew_estimate += 1;
            }
        }

        if let Some(verification) = &bundle.verification {
            with_verification += 1;
            assert_eq!(
                verification.gain_db, 0.0,
                "{case}: the gain stage must be pinned at unity"
            );
            assert!(
                verification.capture.peak_dbfs.is_finite(),
                "{case}: the verification capture carries MS-21's own metering"
            );
            assert!(
                verification.two_clock.is_some(),
                "{case}: the verification fit is evidence and the shape needs a golden case"
            );
            assert_eq!(
                verification.ir.samples.len(),
                verification.installed.bands.channels(),
                "{case}: the capture and the plan must be the same width, or the residual \
                 is not a residual"
            );
            assert!(
                verification.ir.samples.len() >= 2,
                "{case}: at least one golden verification must be two-channel, so the \
                 per-channel fold and the worst-channel gate are graded"
            );
            let rows: Vec<&Vec<paraeq_dsp::peq::EQBand>> =
                verification.installed.bands.iter().collect();
            assert!(
                rows.windows(2).all(|w| w[0] == w[1]),
                "{case}: `Both` routing over divergent per-channel bands has no defined \
                 residual; this case must take the branch where it does"
            );
            assert_eq!(
                verification.routing, bundle.positions[verification.position_index].routing,
                "{case}: the verification must have used the baseline's own routing"
            );
            assert_ne!(
                verification.running_rate_hz, verification.installed.design_rate,
                "{case}: a golden case must exercise a plan armed at a rate it was not \
                 fitted at, or the live-rate preamp is never distinguished from the plan's"
            );
            assert_ne!(
                verification.installed_preamp_db, verification.installed.preamp_db,
                "{case}: the engine's armed preamp must differ from the plan's here, or an \
                 implementation that copied one into the other would look correct"
            );
        }
    }

    assert!(
        with_skew_estimate > 0,
        "no case carries a formed clock-skew estimate"
    );
    assert!(
        without_skew_estimate > 0,
        "no case carries `clock_skew_ppm: None`, so the branch the two-clock warning \
         actually fires on is ungraded"
    );
    assert!(
        with_verification > 0,
        "no case carries a verification block, so the whole `Verification` shape is frozen \
         untested"
    );
}

/// The inputs carry the conditions their `notes.md` claims.
///
/// `notes.md` is what the owner signs at the bless, so a case whose paragraph
/// says "a clipped position" and whose bundle has none would be a signature
/// over nothing. These assertions are about the INPUT, not about any rule: they
/// do not say what `decide()` should answer, only that the condition is really
/// in the bundle.
#[test]
fn the_inputs_carry_the_conditions_their_notes_claim() {
    // bookshelf_clean_room — clean means clean: the seats agree well enough
    // that no variance rule could be right to refuse them.
    let clean = decide(&GoldenCase::load("bookshelf_clean_room").bundle());
    assert_eq!(clean.analysis.per_position_db.len(), 9, "nine positions");
    let modal: Vec<f64> = clean
        .analysis
        .freqs_hz
        .iter()
        .zip(&clean.analysis.sigma_db)
        .filter(|(f, _)| **f <= 200.0)
        .map(|(_, sigma)| *sigma)
        .collect();
    let mut sorted = modal.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("sigma carries no NaN"));
    let median = sorted[sorted.len() / 2];
    assert!(
        median < 6.0,
        "bookshelf_clean_room: median sigma below 200 Hz is {median:.2} dB, which is not a \
         clean room — the decision table refuses a run at 6 dB"
    );

    // room_clipped_position — exactly one railed position, and it is railed in
    // the meter AND in the samples.
    let clipped = GoldenCase::load("room_clipped_position").bundle();
    let railed: Vec<&paraeq_decide::Position> = clipped
        .positions
        .iter()
        .filter(|p| p.capture.clipped_samples > 0)
        .collect();
    assert_eq!(
        railed.len(),
        1,
        "room_clipped_position: exactly one railed position"
    );
    assert!(
        railed[0].capture.peak_dbfs > -0.3,
        "room_clipped_position: the railed position must be past the −0.3 dBFS threshold"
    );
    for position in &clipped.positions {
        let peak = position
            .ir
            .samples
            .iter()
            .flat_map(|c| c.iter())
            .fold(0.0_f64, |acc, v| acc.max(v.abs()));
        let railed_here = position.capture.clipped_samples > 0;
        assert_eq!(
            peak > 0.9661,
            railed_here,
            "room_clipped_position: position {} disagrees with its own meter (peak {peak})",
            position.index
        );
    }

    // room_noisy_snr_boundary — a floor well above every other room case's, and
    // the `None` clock branch.
    let noisy = GoldenCase::load("room_noisy_snr_boundary").bundle();
    let quiet = GoldenCase::load("bookshelf_clean_room").bundle();
    assert!(
        noisy.noise_floor.rms_dbfs[0] > quiet.noise_floor.rms_dbfs[0] + 20.0,
        "room_noisy_snr_boundary: its floor is {:.1} dBFS against the clean room's {:.1}",
        noisy.noise_floor.rms_dbfs[0],
        quiet.noise_floor.rms_dbfs[0]
    );
    assert!(noisy.capture.clock_skew_ppm.is_none());

    // over_ear_ears_heq_cal — the one variant that changes a decision.
    let heq = GoldenCase::load("over_ear_ears_heq_cal").bundle();
    assert_eq!(
        heq.cal.as_ref().expect("a cal").variant,
        paraeq_decide::CalVariant::EarsHeq
    );
    assert!(
        heq.targets.iter().any(|t| t.name == "flat"),
        "the target is FORCED to flat, so flat has to be in the candidate set"
    );

    // room_cal_neighbour_outlier — the vendor defect, verbatim.
    let outlier = GoldenCase::load("room_cal_neighbour_outlier").bundle();
    let content = &outlier.cal.as_ref().expect("a cal").content;
    assert!(
        content.contains("19.611,0.0000"),
        "room_cal_neighbour_outlier: the shipping 7005770_90deg.txt defect is missing"
    );
    let curve = &outlier.cal.as_ref().expect("a cal").curve;
    let index = curve
        .0
        .iter()
        .position(|f| (*f - 19.611).abs() < 1e-9)
        .expect("19.611 Hz is in the parsed curve as well as in the text");
    assert_eq!(
        curve.1[index], 0.0,
        "the parsed curve carries the same zero"
    );
    assert!(
        index > 0 && index + 1 < curve.0.len(),
        "the defect needs both neighbours, or the single-defect rule cannot fire"
    );

    // over_ear_lost_seal_reseat — one reseat lost its bass and the rest did not.
    let seal = GoldenCase::load("over_ear_lost_seal_reseat").bundle();
    assert_eq!(seal.positions.len(), 5, "five reseats");
    assert!(
        seal.verification.is_some(),
        "this is the case that carries the verification block"
    );
}
