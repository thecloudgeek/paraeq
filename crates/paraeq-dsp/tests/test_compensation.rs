mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::compensation::{self, CalWarningKind};
use std::path::PathBuf;

fn proto_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../prototype/tests/fixtures")
        .join(name)
}

// ---- Tier 1: frozen prototype parity -------------------------------------

#[test]
fn apply_matches_oracle_with_edge_hold() {
    let c = Case::load("compensation", "edge_hold");
    let out = compensation::apply_compensation(
        &c.array("mag_db"),
        &c.array("grid"),
        &c.array("comp_freqs"),
        &c.array("comp_gains"),
    );
    assert_allclose(
        &out,
        &c.array("compensated"),
        1e-12,
        1e-12,
        "apply_compensation",
    );
}

// Oracle pin (.venv/bin/python, prototype.paraeq.measurement.compensation.load_compensation):
// test_compensation.csv          7 20.0 20000.0 0.5 -2.0
// test_compensation_minidsp.txt  7 20.0 20000.0 0.5 -2.0
// Both were invariant across the REW-rule rewrite, which is exactly why
// neither caught the dropped first row — see the Tier-2 cases below.
#[test]
#[allow(deprecated)] // the shim's signature is the thing under test
fn parses_paraeq_csv_format() {
    let (f, g) = compensation::load_compensation(&proto_fixture("test_compensation.csv")).unwrap();
    assert_eq!(f.len(), 7);
    assert_eq!(g.len(), 7);
    assert_eq!(f[0], 20.0);
    assert_eq!(f[f.len() - 1], 20000.0);
    assert_eq!(g[0], 0.5);
    assert_eq!(g[g.len() - 1], -2.0);
    assert!(f.windows(2).all(|w| w[0] < w[1]), "freqs sorted");
}

#[test]
#[allow(deprecated)] // the shim's signature is the thing under test
fn parses_minidsp_format() {
    let (f, g) =
        compensation::load_compensation(&proto_fixture("test_compensation_minidsp.txt")).unwrap();
    assert_eq!(f.len(), 7);
    assert_eq!(g.len(), 7);
    assert_eq!(f[0], 20.0);
    assert_eq!(f[f.len() - 1], 20000.0);
    assert_eq!(g[0], 0.5);
    assert_eq!(g[g.len() - 1], -2.0);
}

// ---- Tier 2: generated cal cases -----------------------------------------

/// Every generated case must parse to the curve its file was rendered from.
/// `single_header` and `tab_delimited` are the ones that fail under the old
/// quote-sniff + skip(2) rule: it ate the 20 Hz row with the lone header.
#[test]
fn parses_every_cal_layout_to_the_generated_curve() {
    for name in [
        "comma_delimited",
        "single_header",
        "tab_delimited",
        "two_header",
    ] {
        let c = Case::load("compensation", name);
        let cal = compensation::load_cal(&c.file(c.param_str("cal_file"))).unwrap();
        assert_allclose(&cal.freqs, &c.array("freqs"), 1e-12, 1e-12, name);
        assert_allclose(&cal.gains_db, &c.array("gains"), 1e-12, 1e-12, name);
    }
}

#[test]
fn single_header_file_keeps_its_first_data_row() {
    let c = Case::load("compensation", "single_header");
    let cal = compensation::load_cal(&c.file(c.param_str("cal_file"))).unwrap();
    assert_eq!(cal.freqs.len(), 7, "the 20 Hz row must survive the header");
    assert_eq!(cal.freqs[0], 20.0);
    assert_eq!(cal.gains_db[0], 0.5);
}

#[test]
fn phase_column_never_leaks_into_gains() {
    let c = Case::load("compensation", "single_header");
    let cal = compensation::load_cal(&c.file(c.param_str("cal_file"))).unwrap();
    // Phase is 0.0 in every row; gains must carry SPL, not the third column.
    assert_eq!(cal.gains_db[3], -0.2);
}

// ---- Tier 3: analytic invariants -----------------------------------------

#[test]
fn cal_curves_are_never_normalized() {
    // Two curves differing by a constant 2.1 dB must STILL differ by 2.1 dB
    // after parse + apply. Guards the EARS L/R capsule offset.
    const OFFSET_DB: f64 = 2.1;
    let left = "\"Sens Factor =-0.8dB\"\n20,1.0\n1000,3.0\n20000,-1.0\n";
    let right = "\"Sens Factor =-0.8dB\"\n20,3.1\n1000,5.1\n20000,1.1\n";

    let cal_l = compensation::parse_cal(left).unwrap();
    let cal_r = compensation::parse_cal(right).unwrap();

    // The curve reaches the caller carrying the offset -- no reference-band
    // normalization, so 1 kHz is emphatically NOT 0 dB.
    assert_eq!(cal_l.gains_db[1], 3.0);
    assert_eq!(cal_r.gains_db[1], 5.1);
    for (l, r) in cal_l.gains_db.iter().zip(&cal_r.gains_db) {
        assert!((r - l - OFFSET_DB).abs() < 1e-12, "parse moved the offset");
    }

    // ...and it survives application to a measurement, which is where a
    // normalizing parser would have silently made the imbalance permanent.
    let grid: Vec<f64> = (1..=200).map(|i| i as f64 * 100.0).collect();
    let mag = vec![0.0; grid.len()];
    let out_l = compensation::apply_compensation(&mag, &grid, &cal_l.freqs, &cal_l.gains_db);
    let out_r = compensation::apply_compensation(&mag, &grid, &cal_r.freqs, &cal_r.gains_db);
    for (l, r) in out_l.iter().zip(&out_r) {
        assert!(
            (l - r - OFFSET_DB).abs() < 1e-12,
            "apply moved the offset: {l} vs {r}"
        );
    }
}

#[test]
fn metadata_round_trips() {
    let cal = compensation::parse_cal(
        "\"Sens Factor =-0.421dB, AGain =18dB, SERNO: 7103798\"\n20,0.0\n20000,0.0\n",
    )
    .unwrap();
    assert_eq!(cal.sens_factor_db, Some(-0.421));
    assert_eq!(cal.again_db, Some(18.0));
    assert_eq!(cal.serial.as_deref(), Some("7103798"));
}

#[test]
fn metadata_is_absent_when_unstated() {
    let cal = compensation::parse_cal("# ParaEQ curve\n20,0.0\n20000,0.0\n").unwrap();
    assert_eq!(cal.sens_factor_db, None);
    assert_eq!(cal.again_db, None);
    assert_eq!(cal.serial, None);
}

#[test]
fn ignored_lines_are_preserved_verbatim_in_file_order() {
    let c = Case::load("compensation", "two_header");
    let cal = compensation::load_cal(&c.file(c.param_str("cal_file"))).unwrap();
    assert_eq!(
        cal.ignored_lines,
        vec![
            "\"Sens Factor =-0.8dB, EARS Serial 999-9999, compensation RAW V1\"",
            "\"Use this file on the LEFT channel. Your sensitive side is RIGHT.\"",
            "*",
            "* Freq(Hz) SPL(dB) Phase(degrees)",
            "*",
        ],
        "quotes, comments and order must all survive"
    );
}

#[test]
fn header_only_file_is_an_error() {
    let err = compensation::parse_cal("\"Sens Factor =-0.421dB\"\n* nothing follows\n");
    assert!(matches!(err, Err(paraeq_dsp::DspError::Parse(_))));
}

#[test]
fn row_with_unparseable_second_column_is_an_error() {
    // Column 1 parses, column 2 does not: malformed row, not a header.
    assert!(compensation::parse_cal("20,0.5\n50,\n").is_err());
    assert!(compensation::parse_cal("20\t0.5\n50\n").is_err());
    assert!(compensation::parse_cal("20,0.5\n50,bogus\n").is_err());
}

#[test]
fn non_finite_tokens_are_not_numbers() {
    // "NaN"/"inf" parse as f64 but do not BEGIN WITH A NUMBER under REW's
    // rule, so such a line is a header, not a data row.
    let cal = compensation::parse_cal("NaN 1.0\ninf 2.0\n20,0.5\n20000,-2.0\n").unwrap();
    assert_eq!(cal.freqs, vec![20.0, 20000.0]);
    assert_eq!(cal.ignored_lines, vec!["NaN 1.0", "inf 2.0"]);
    // A non-finite GAIN on a real data row is malformed, not ignorable.
    assert!(compensation::parse_cal("20,0.5\n50,NaN\n").is_err());
}

#[test]
fn validator_flags_the_7005770_vendor_zero() {
    // The shipping 7005770_90deg.txt pattern, exactly as the spec states it:
    // an exact 0.0000 at 19.611 Hz between -3.13 and -3.11, sitting right
    // where correction authority is highest.
    let cal = compensation::parse_cal("19.361,-3.13\n19.611,0.0000\n19.861,-3.11\n").unwrap();
    let warnings = compensation::validate_cal(&cal);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].freq_hz, 19.611);
    // SuspectZero, NOT Outlier: exact-zero is the vendor's own signature and
    // earns its own message even though it also fails the outlier threshold.
    assert!(
        matches!(
            warnings[0].kind,
            CalWarningKind::SuspectZero { neighbours_db } if neighbours_db == (-3.13, -3.11)
        ),
        "{:?}",
        warnings[0].kind
    );
}

/// One bad sample is ONE warning, even though it also drags both neighbours
/// off their interpolants by ~1.6 dB. On the real 0.25 Hz-spaced vendor grid
/// the naive per-point rule reports three defects and invites the wizard to
/// interpolate over two perfectly good samples.
#[test]
fn a_single_defect_raises_a_single_warning_on_a_dense_grid() {
    let cal = compensation::parse_cal(
        "19.111,-3.15\n19.361,-3.13\n19.611,0.0000\n19.861,-3.11\n20.111,-3.09\n",
    )
    .unwrap();
    let warnings = compensation::validate_cal(&cal);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].freq_hz, 19.611);
    assert!(matches!(
        warnings[0].kind,
        CalWarningKind::SuspectZero { .. }
    ));
}

#[test]
fn validator_is_silent_on_a_smooth_curve() {
    let cal =
        compensation::parse_cal("20,-3.13\n50,-3.11\n100,-3.09\n1000,-2.5\n20000,-1.0\n").unwrap();
    assert_eq!(compensation::validate_cal(&cal), vec![]);
}

/// A curve that legitimately passes through 0 dB is not a vendor defect:
/// SuspectZero fires only for a zero that is ALSO an outlier. The two_header
/// fixture's 100 Hz row is exactly 0.0000 and must stay silent.
#[test]
fn an_exact_zero_on_a_smooth_curve_is_not_suspect() {
    let c = Case::load("compensation", "two_header");
    let cal = compensation::load_cal(&c.file(c.param_str("cal_file"))).unwrap();
    assert_eq!(cal.gains_db[2], 0.0);
    assert_eq!(compensation::validate_cal(&cal), vec![]);
}

#[test]
fn validator_flags_a_plain_outlier() {
    let cal = compensation::parse_cal("20,-3.0\n50,-3.0\n100,4.5\n1000,-3.0\n").unwrap();
    let warnings = compensation::validate_cal(&cal);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].freq_hz, 100.0);
    assert!(matches!(warnings[0].kind, CalWarningKind::Outlier { .. }));
}

#[test]
fn validator_flags_frequency_order_defects() {
    let dup = compensation::parse_cal("20,0.0\n50,0.0\n50,0.0\n100,0.0\n").unwrap();
    let kinds: Vec<_> = compensation::validate_cal(&dup)
        .iter()
        .map(|w| w.kind)
        .collect();
    assert!(kinds.contains(&CalWarningKind::DuplicateFreq), "{kinds:?}");

    let back = compensation::parse_cal("20,0.0\n100,0.0\n50,0.0\n200,0.0\n").unwrap();
    let kinds: Vec<_> = compensation::validate_cal(&back)
        .iter()
        .map(|w| w.kind)
        .collect();
    assert!(
        kinds.contains(&CalWarningKind::NonMonotonicFreq),
        "{kinds:?}"
    );
}

/// A UTF-8 BOM is an encoding marker, not content, and U+FEFF is not
/// White_Space -- so before the strip it rode into the first token, a
/// headerless file's first row parsed as a header, and the curve edge-held
/// from the SECOND row down. Silent, and worst below 50 Hz where correction
/// authority is highest. The oracle reads utf-8-sig for the same reason.
#[test]
fn a_utf8_bom_does_not_eat_the_first_data_row() {
    let body = "20,0.5\n50,0.3\n100,0.0\n1000,-0.2\n20000,-2.0\n";
    let bare = compensation::parse_cal(body).unwrap();
    let bommed = compensation::parse_cal(&format!("\u{feff}{body}")).unwrap();
    assert_eq!(bommed.freqs, bare.freqs, "the BOM must not drop a row");
    assert_eq!(bommed.freqs[0], 20.0);
    assert!(bommed.ignored_lines.is_empty());

    // A BOM on a header line was always harmless (the header is ignored
    // either way) and must stay so.
    let headered = compensation::parse_cal("\u{feff}\"Sens Factor =-0.421dB\"\n20,0.5\n").unwrap();
    assert_eq!(headered.freqs, vec![20.0]);
    assert_eq!(headered.sens_factor_db, Some(-0.421));
}

/// `\r\n`, `\r` and `\n` all end a line. `str::lines` splits on `\n` only, so
/// a CR-only file reached `columns` as ONE line and -- `'\r'` being
/// whitespace -- tokenized into a single silently truncated row, while the
/// oracle's text-mode read (universal newlines) returned every row.
#[test]
fn every_line_ending_ends_a_line() {
    let rows = vec![20.0, 50.0, 20000.0];
    for (name, content) in [
        ("LF", "20,0.5\n50,0.3\n20000,-2.0\n"),
        ("CRLF", "20,0.5\r\n50,0.3\r\n20000,-2.0\r\n"),
        ("CR", "20,0.5\r50,0.3\r20000,-2.0\r"),
    ] {
        let cal = compensation::parse_cal(content).unwrap();
        assert_eq!(cal.freqs, rows, "{name}: a row was lost");
        assert!(
            cal.ignored_lines.is_empty(),
            "{name}: {:?}",
            cal.ignored_lines
        );
    }
}

/// The number grammar is the C-locale float, pinned on both sides: CPython's
/// `float()` is the looser of the two (it takes `1_000` and Arabic-Indic
/// digits; Rust takes neither), so the oracle gates on the ASCII float
/// alphabet before calling it. A token Rust rejects must be a header on both.
#[test]
fn the_number_grammar_is_the_c_locale_float() {
    // Underscored and non-ASCII digits are not numbers -> the line is a header.
    let cal = compensation::parse_cal("1_000,0.5\n٢٠,0.5\n20,0.5\n20000,-2.0\n").unwrap();
    assert_eq!(cal.freqs, vec![20.0, 20000.0]);
    assert_eq!(cal.ignored_lines, vec!["1_000,0.5", "٢٠,0.5"]);
    // Overflow to inf is not a number either (f64::from_str returns inf).
    assert!(compensation::parse_cal("1e400,0.5\n").is_err());
}

/// The validator predicts each point from its neighbours on the LOG-frequency
/// axis, because cal grids are log-spaced. `neighbour_deviation_db`'s own
/// comment says a linear-in-f interpolant "would skew every prediction toward
/// the upper neighbour and manufacture outliers on a smooth curve" -- this
/// pins that claim. (`apply_compensation` interpolates in linear f, which is
/// the oracle-pinned Tier-1 behaviour and a separate contract.)
///
/// 100/200/400 Hz is a symmetric octave straddle: 200 Hz is the log-f
/// midpoint, so a curve linear in log-f is predicted EXACTLY and is silent.
/// A linear-f interpolant puts the midpoint at t = 1/3 instead of 1/2 and
/// mispredicts by (1/2 - 1/3) x 12 dB = 2.0 dB -- twice DEFAULT_OUTLIER_DB,
/// so the mutant warns where the real interpolant does not.
#[test]
fn the_validator_interpolates_in_log_frequency() {
    let smooth = compensation::parse_cal("100,0.0\n200,6.0\n400,12.0\n").unwrap();
    assert_eq!(
        compensation::validate_cal(&smooth),
        vec![],
        "a curve linear in log-f is smooth: a linear-f interpolant would \
         manufacture a 2.0 dB outlier at 200 Hz"
    );

    // The mirror image: a curve linear in LINEAR f is genuinely kinked in
    // log-f, and must be flagged. Pins the direction of the axis, so the test
    // cannot pass by simply never warning.
    let kinked = compensation::parse_cal("100,0.0\n200,4.0\n400,12.0\n").unwrap();
    let warnings = compensation::validate_cal(&kinked);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].freq_hz, 200.0);
}

/// SuspectZero is the vendor's specific exact-0.0000 signature, and both
/// neighbour gates are load-bearing: an outlying zero whose neighbour is
/// itself near 0 dB is a curve crossing zero steeply, not a dropped sample,
/// and must report as a plain Outlier.
///
/// Note both cases must be genuine OUTLIERS: the kind is chosen only inside
/// an over-threshold run, so a silent curve exercises neither gate and would
/// pass no matter what they said.
#[test]
fn suspect_zero_needs_both_neighbours_far_from_zero() {
    // Zero between two neighbours far from 0 dB: the vendor defect.
    let defect = compensation::parse_cal("20,-3.0\n50,-3.0\n100,0.0\n1000,-3.0\n").unwrap();
    let warnings = compensation::validate_cal(&defect);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].freq_hz, 100.0);
    assert!(matches!(
        warnings[0].kind,
        CalWarningKind::SuspectZero { .. }
    ));

    // An outlying zero whose LOWER neighbour is only 0.5 dB from zero: still
    // a defect worth flagging, but not the vendor's signature -- Outlier.
    let crossing = compensation::parse_cal("100,-0.5\n200,0.0\n400,8.0\n").unwrap();
    let warnings = compensation::validate_cal(&crossing);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].freq_hz, 200.0);
    assert!(
        matches!(warnings[0].kind, CalWarningKind::Outlier { .. }),
        "a zero beside a near-zero neighbour is a steep crossing, not the \
         vendor's dropped-sample signature: {:?}",
        warnings[0].kind
    );
}
