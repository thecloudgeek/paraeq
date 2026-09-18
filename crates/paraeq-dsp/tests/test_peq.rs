mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::peq::{EQBand, FilterType, ParametricEQ};

fn bands_from_scalars(c: &Case) -> Vec<EQBand> {
    c.scalar("bands")
        .as_array()
        .unwrap()
        .iter()
        .map(|b| EQBand {
            filter_type: FilterType::from_str(b["filter_type"].as_str().unwrap()).unwrap(),
            fc: b["fc"].as_f64().unwrap(),
            gain_db: b["gain_db"].as_f64().unwrap(),
            q: b["q"].as_f64().unwrap(),
        })
        .collect()
}

#[test]
fn composite_response_matches_oracle() {
    let c = Case::load("peq", "two_band");
    let peq = ParametricEQ {
        bands: bands_from_scalars(&c),
        sample_rate: c.param_f64("sample_rate"),
    };
    assert_eq!(peq.combined_sos().len(), 2);
    let resp = peq.frequency_response(&c.array("freqs"));
    assert_allclose(
        &resp,
        &c.array("response_db"),
        1e-9,
        1e-9,
        "composite response",
    );
}

#[test]
fn autoeq_export_matches_oracle_exactly() {
    let c = Case::load("peq", "two_band");
    let peq = ParametricEQ {
        bands: bands_from_scalars(&c),
        sample_rate: c.param_f64("sample_rate"),
    };
    assert_eq!(
        peq.export_autoeq_format(),
        c.scalar("autoeq_export").as_str().unwrap()
    );
}

/// The EQBand wire format the UI hand-mirrors (desktop/ui/src/ipc/types.ts):
/// snake_case field names AND snake_case `filter_type` values, pinned in BOTH
/// directions — serialize to the golden JSON, and deserialize that same golden
/// JSON back. `serde` is an optional feature on this crate (off for the
/// realtime path); the workspace build turns it on through paraeq-decide and
/// desktop/src-tauri, so this test runs in `cargo test --workspace`.
#[cfg(feature = "serde")]
#[test]
fn band_serde_roundtrip_and_golden_json() {
    use serde_json::json;

    let band = EQBand {
        filter_type: FilterType::Peaking,
        fc: 1000.0,
        gain_db: 3.0,
        q: 1.41,
    };
    let golden = json!({"filter_type": "peaking", "fc": 1000.0, "gain_db": 3.0, "q": 1.41});
    assert_eq!(serde_json::to_value(&band).unwrap(), golden);
    // Deserialize direction: the golden JSON's "peaking" must map back, and a
    // PascalCase spelling must NOT be accepted (it would mean the rename was
    // dropped and the UI's contract silently re-pinned).
    let from_golden: EQBand = serde_json::from_value(golden).unwrap();
    assert_eq!(from_golden, band);
    assert!(serde_json::from_value::<EQBand>(
        json!({"filter_type": "Peaking", "fc": 1000.0, "gain_db": 3.0, "q": 1.41})
    )
    .is_err());
}

/// The caller-supplied export writes the number it is handed, unlike the
/// oracle-pinned `export_autoeq_format` (a `0.0` literal) and unlike
/// `export_autoeq_format_with_preamp` (the cascade-derived number, which R1-1
/// made the desktop's `eq_export_autoeq` use). No production caller hands it a
/// number today; the variant is kept for the one that will.
#[test]
fn export_with_preamp_db_writes_the_caller_s_preamp() {
    let peq = ParametricEQ {
        bands: Vec::new(),
        sample_rate: 48000.0,
    };
    let exported = peq.export_autoeq_format_with_preamp_db(-6.5);
    assert_eq!(exported.lines().next().unwrap(), "Preamp: -6.5 dB");
    // Shares the -0.0 guard with the other two exports...
    assert_eq!(
        peq.export_autoeq_format_with_preamp_db(-0.04)
            .lines()
            .next()
            .unwrap(),
        "Preamp: 0.0 dB"
    );
    // ...and only that. The guard is bounded above at 0, so the positive half
    // of the desktop's accepted range (eq.rs: PREAMP_MAX_DB = 10.0) is written
    // verbatim rather than swallowed to "0.0".
    for (preamp_db, want) in [
        (0.1, "Preamp: 0.1 dB"),
        (3.5, "Preamp: 3.5 dB"),
        (6.0, "Preamp: 6.0 dB"),
        (10.0, "Preamp: 10.0 dB"),
        (-30.0, "Preamp: -30.0 dB"),
    ] {
        assert_eq!(
            peq.export_autoeq_format_with_preamp_db(preamp_db)
                .lines()
                .next()
                .unwrap(),
            want,
            "preamp {preamp_db}"
        );
    }
}

#[test]
fn empty_bands_returns_exact_zeros() {
    // parametric_eq.py:79-90 special-cases `not self.bands` to return exact
    // zeros, rather than the generic cascade's `20*log10(1 + 1e-10)`.
    let peq = ParametricEQ {
        bands: Vec::new(),
        sample_rate: 48000.0,
    };
    let freqs = vec![20.0, 100.0, 1000.0, 10000.0, 20000.0];
    let resp = peq.frequency_response(&freqs);
    assert_eq!(resp.len(), freqs.len());
    assert!(resp.iter().all(|&v| v == 0.0));
}

// --- preamp_db (R1-1, 2026-07-15 engine-hardening spec) --------------------
// No oracle counterpart: the prototype hardcodes "Preamp: 0.0 dB". These
// tests pin the decided convention instead — `-max(0, peak of the realized
// cascade)` over the union grid, no headroom constant (DIVERGENCES.md #14).

fn peaking_band(fc: f64, gain_db: f64, q: f64) -> EQBand {
    EQBand {
        filter_type: FilterType::Peaking,
        fc,
        gain_db,
        q,
    }
}

/// 1/48- (or denser) octave log grid over [1.0, f_max], both endpoints
/// included — the R1-1 base grid, rebuilt independently of the impl.
fn log_grid(f_max: f64, points_per_octave: f64) -> Vec<f64> {
    let mut grid = Vec::new();
    for k in 0.. {
        let f = 2f64.powf(f64::from(k) / points_per_octave);
        if f >= f_max {
            break;
        }
        grid.push(f);
    }
    grid.push(f_max);
    grid
}

fn max_response_db(peq: &ParametricEQ, freqs: &[f64]) -> f64 {
    peq.frequency_response(freqs)
        .into_iter()
        .fold(f64::NEG_INFINITY, f64::max)
}

#[test]
fn preamp_single_peaking_band_is_minus_gain() {
    // R1-1 test table row 1: a lone +10 dB Q=1 band's realized peak sits at
    // fc, so preamp_db is −10.0 (fc is unioned into the grid → exact).
    let peq = ParametricEQ {
        bands: vec![peaking_band(1000.0, 10.0, 1.0)],
        sample_rate: 48000.0,
    };
    assert!((peq.preamp_db() - (-10.0)).abs() < 0.02);
}

#[test]
fn preamp_overlapping_boosts_are_superadditive() {
    // R1-1 test table row 2: two overlapping +6 dB bands cascade to more
    // than +6 dB, so the per-band max would under-reserve headroom. The
    // realized value must also agree with a dense 1/384-octave sweep.
    let peq = ParametricEQ {
        bands: vec![peaking_band(100.0, 6.0, 1.0), peaking_band(120.0, 6.0, 1.0)],
        sample_rate: 48000.0,
    };
    let preamp = peq.preamp_db();
    assert!(preamp < -6.0, "overlap must exceed a single band: {preamp}");
    let dense_max = max_response_db(&peq, &log_grid(0.499 * 48000.0, 384.0));
    assert!((preamp + dense_max).abs() < 0.02);
}

#[test]
fn preamp_octave_apart_boosts_are_superadditive() {
    // Spec prose case: "two +6 dB bands an octave apart sum to more than
    // +6 dB where they overlap".
    let peq = ParametricEQ {
        bands: vec![peaking_band(100.0, 6.0, 1.0), peaking_band(200.0, 6.0, 1.0)],
        sample_rate: 48000.0,
    };
    assert!(peq.preamp_db() < -6.0);
}

#[test]
fn preamp_pure_cut_is_exactly_zero() {
    // R1-1 test table row 3 — pins DIVERGENCES.md #14: AutoEQ would emit a
    // *positive* preamp here (its −max_gain is signed); ParaEQ clamps at 0.
    let peq = ParametricEQ {
        bands: vec![
            EQBand {
                filter_type: FilterType::LowShelf,
                fc: 80.0,
                gain_db: -6.0,
                q: 0.707,
            },
            peaking_band(300.0, -4.0, 1.0),
            EQBand {
                filter_type: FilterType::Notch,
                fc: 60.0,
                gain_db: 0.0,
                q: 5.0,
            },
            EQBand {
                filter_type: FilterType::HighShelf,
                fc: 8000.0,
                gain_db: -3.0,
                q: 0.707,
            },
        ],
        sample_rate: 48000.0,
    };
    assert_eq!(peq.preamp_db(), 0.0);
    // Degenerate flat EQ is also exactly 0.0, not −20·log10(1 + 1e-10).
    let flat = ParametricEQ {
        bands: Vec::new(),
        sample_rate: 48000.0,
    };
    assert_eq!(flat.preamp_db(), 0.0);
}

#[test]
fn preamp_high_q_band_off_grid_needs_fc_union() {
    // R1-1 test table row 4: Q=20 @ 8137 Hz — 8137 falls between 1/48-octave
    // grid points (~55 Hz away) and its bandwidth is ~407 Hz, so the log grid
    // alone clips the peak. The fc union makes it exact.
    let peq = ParametricEQ {
        bands: vec![peaking_band(8137.0, 6.0, 20.0)],
        sample_rate: 48000.0,
    };
    let preamp = peq.preamp_db();
    assert!((preamp - (-6.0)).abs() < 0.02);
    // The dense 1/384-octave sweep agrees...
    let dense_max = max_response_db(&peq, &log_grid(0.499 * 48000.0, 384.0));
    assert!((preamp + dense_max).abs() < 0.02);
    // ...but the bare 1/48-octave grid (no fc union) visibly misses the
    // peak — the union is load-bearing, not belt-and-braces.
    let bare_max = max_response_db(&peq, &log_grid(0.499 * 48000.0, 48.0));
    assert!(
        bare_max < 5.95,
        "bare log grid resolved the peak: {bare_max}"
    );
}

#[test]
fn preamp_shelf_peaks_caught_at_endpoints() {
    // Shelf maxima sit at DC/Nyquist, outside any interior log point; the
    // grid's 1.0 / 0.499·sample_rate endpoints must catch them.
    let low = ParametricEQ {
        bands: vec![EQBand {
            filter_type: FilterType::LowShelf,
            fc: 50.0,
            gain_db: 6.0,
            q: 0.707,
        }],
        sample_rate: 48000.0,
    };
    let low_end = low.frequency_response(&[1.0])[0];
    assert!((low.preamp_db() + low_end).abs() < 1e-12);
    assert!((low_end - 6.0).abs() < 0.1, "not at the shelf plateau");

    let high = ParametricEQ {
        bands: vec![EQBand {
            filter_type: FilterType::HighShelf,
            fc: 12000.0,
            gain_db: 5.0,
            q: 0.707,
        }],
        sample_rate: 48000.0,
    };
    let high_end = high.frequency_response(&[0.499 * 48000.0])[0];
    assert!((high.preamp_db() + high_end).abs() < 1e-12);
    assert!((high_end - 5.0).abs() < 0.1, "not at the shelf plateau");
}

#[test]
fn preamp_self_consistent_with_own_frequency_response() {
    // preamp_db must equal −max(0, max of the EQ's own frequency_response)
    // over the decided grid: sorted union of the 1/48-octave log grid, every
    // band fc clamped into [1.0, 0.499·sample_rate], and the endpoints.
    let sample_rate = 44100.0;
    let peq = ParametricEQ {
        bands: vec![
            peaking_band(45.0, 10.0, 4.0),
            peaking_band(120.0, -8.0, 2.0),
            EQBand {
                filter_type: FilterType::LowShelf,
                fc: 100.0,
                gain_db: 3.0,
                q: 0.707,
            },
            peaking_band(6100.0, 7.5, 12.0),
        ],
        sample_rate,
    };
    let f_max = 0.499 * sample_rate;
    let mut grid = log_grid(f_max, 48.0);
    for band in &peq.bands {
        grid.push(band.fc.clamp(1.0, f_max));
    }
    grid.sort_by(f64::total_cmp);
    grid.dedup();
    let expected = -max_response_db(&peq, &grid).max(0.0);
    assert!((peq.preamp_db() - expected).abs() < 1e-12);
}

#[test]
fn preamp_counts_surviving_boosts_when_a_band_designs_nan() {
    // A q = 0 design yields NaN coefficients; the engine's build_iir funnel
    // (R1-3) substitutes identity for that row alone and keeps the rest of
    // the cascade running. preamp_db mirrors the substitution: the +12 dB
    // survivor still needs its headroom. (Un-mirrored, the NaN response is
    // discarded by the max fold and the preamp collapses to 0.0 —
    // fail-unsafe.)
    let peq = ParametricEQ {
        bands: vec![
            peaking_band(1000.0, 12.0, 1.0),
            peaking_band(200.0, 3.0, 0.0),
        ],
        sample_rate: 48000.0,
    };
    assert!((peq.preamp_db() - (-12.0)).abs() < 0.02);

    let nan_fc = ParametricEQ {
        bands: vec![
            peaking_band(1000.0, 12.0, 1.0),
            peaking_band(f64::NAN, 3.0, 1.0),
        ],
        sample_rate: 48000.0,
    };
    assert!((nan_fc.preamp_db() - (-12.0)).abs() < 0.02);
}

#[test]
fn preamp_lone_pure_cut_is_exactly_zero() {
    // A single cut leaves |H| within ~1e-12 of unity at the top grid
    // endpoint, where the response's +1e-10 additive magnitude floor pushes
    // the dB value slightly positive (~8e-10). The bias ceiling must absorb
    // it: exactly 0.0, with no wideband cut around to mask the edge (the
    // multi-band pure-cut test's shelf pulls |H| below the bias there).
    let cut = ParametricEQ {
        bands: vec![peaking_band(100.0, -6.0, 2.0)],
        sample_rate: 48000.0,
    };
    assert_eq!(cut.preamp_db(), 0.0);

    let notch = ParametricEQ {
        bands: vec![EQBand {
            filter_type: FilterType::Notch,
            fc: 60.0,
            gain_db: 0.0,
            q: 5.0,
        }],
        sample_rate: 48000.0,
    };
    assert_eq!(notch.preamp_db(), 0.0);
}

// --- the computed-preamp export (Stage 5) ---------------------------------
// `export_autoeq_format` keeps the oracle's literal and stays fixture-pinned
// above; these pin the additive export that carries the real number.

#[test]
fn the_computed_export_carries_the_realized_preamp() {
    let peq = ParametricEQ {
        bands: vec![peaking_band(1000.0, 6.0, 1.0)],
        sample_rate: 48000.0,
    };
    let expected = peq.preamp_db();
    assert!(expected < -5.0, "sanity: a +6 dB boost needs real headroom");
    let text = peq.export_autoeq_format_with_preamp();
    assert_eq!(
        text.lines().next().unwrap(),
        format!("Preamp: {expected:.1} dB")
    );
}

#[test]
fn the_two_exports_differ_only_in_the_preamp_line() {
    let peq = ParametricEQ {
        bands: vec![
            peaking_band(1000.0, 6.0, 1.0),
            peaking_band(3000.0, -3.0, 2.0),
        ],
        sample_rate: 48000.0,
    };
    let oracle_text = peq.export_autoeq_format();
    let oracle: Vec<&str> = oracle_text.lines().collect();
    let computed_text = peq.export_autoeq_format_with_preamp();
    let computed: Vec<&str> = computed_text.lines().collect();
    assert_eq!(oracle[0], "Preamp: 0.0 dB");
    assert_ne!(computed[0], oracle[0]);
    assert_eq!(oracle[1..], computed[1..], "filter lines must be identical");
}

#[test]
fn a_pure_cut_export_never_prints_negative_zero() {
    // preamp_db() clamps at 0 for a pure cut, and a preamp inside the display
    // rounding step must print as "0.0", never "-0.0" — which reads as a bug.
    let peq = ParametricEQ {
        bands: vec![peaking_band(1000.0, -6.0, 1.0)],
        sample_rate: 48000.0,
    };
    assert_eq!(peq.preamp_db(), 0.0);
    assert_eq!(
        peq.export_autoeq_format_with_preamp()
            .lines()
            .next()
            .unwrap(),
        "Preamp: 0.0 dB"
    );
}

#[test]
fn an_empty_band_set_exports_a_zero_preamp_either_way() {
    let peq = ParametricEQ {
        bands: Vec::new(),
        sample_rate: 48000.0,
    };
    assert_eq!(peq.export_autoeq_format(), "Preamp: 0.0 dB");
    assert_eq!(peq.export_autoeq_format_with_preamp(), "Preamp: 0.0 dB");
}

// --- the realized cascade: sosfilt / realized_sos / realized_response /
// --- apply_offline / preamp_grid (build-plan B2 item 6) -------------------
//
// Why these exist: `combined_sos` (and therefore the Tier-1 fixture-pinned
// `frequency_response`) evaluates the cascade AS DESIGNED, at `self.sample_rate`.
// `preamp_db` already evaluates the cascade AS INSTALLED — `biquad::IDENTITY`
// substituted for any row failing `biquad::is_stable`, mirroring `build_iir`'s
// R1-3 funnel. With one unstable design row those two disagree, so a
// verification residual computed from `frequency_response` reports an engine
// fault that is really a PREDICTION fault. `realized_response` is the shared
// answer, and `realized_sos` is the single fold both it and `preamp_db` run so
// they cannot drift.

#[test]
fn sosfilt_matches_the_scipy_sosfilt_fixture() {
    // Tier 2. The fixture's SOS array is supplied EXPLICITLY, which is the whole
    // reason `sosfilt` is a free function with no `self`: `apply_offline` takes
    // its sections from `realized_sos(rate_hz)`, so an arbitrary array could
    // never be pushed through that door and a fixture hung there would have
    // graded nothing.
    let c = Case::load("peq", "sosfilt_offline");
    let sos_flat = c.array2("sos");
    let sos: Vec<[f64; 6]> = (0..sos_flat.rows)
        .map(|r| {
            let row = &sos_flat.data[r * sos_flat.cols..(r + 1) * sos_flat.cols];
            [row[0], row[1], row[2], row[3], row[4], row[5]]
        })
        .collect();
    let y = paraeq_dsp::peq::sosfilt(&sos, &c.array("x"));
    assert_allclose(&y, &c.array("y"), 1e-9, 1e-12, "sosfilt");
}

/// A band set with exactly one row that fails `biquad::is_stable` (q = 0 makes
/// every coefficient NaN) and a realized peak comfortably above
/// `preamp_db`'s BIAS_CEILING_DB, so the boost — not the bias — decides.
fn one_unstable_row_boost_dominant() -> ParametricEQ {
    ParametricEQ {
        bands: vec![
            peaking_band(1000.0, 9.0, 1.0),
            peaking_band(200.0, 4.0, 0.0), // q = 0 ⇒ NaN coefficients ⇒ unstable
        ],
        sample_rate: 48000.0,
    }
}

#[test]
fn realized_response_on_the_preamp_grid_peaks_at_minus_preamp_db_for_a_boost_dominant_set() {
    // The item's whole point, stated as an equality that only holds for the
    // REALIZED cascade. Two things make the naive version of this test false and
    // both are avoided here deliberately:
    //
    //   1. `preamp_db` evaluates on ITS OWN grid, not on a caller's log-f
    //      analysis grid, so the peaks would differ by construction. Hence
    //      `preamp_grid()`.
    //   2. `preamp_db` returns exactly 0.0 below BIAS_CEILING_DB, so a
    //      cut-dominant set would compare 0.0 against a negative peak. Hence
    //      "boost dominant". That branch gets its own test below, pinned rather
    //      than "fixed".
    let peq = one_unstable_row_boost_dominant();
    let grid = peq.preamp_grid();
    let realized = peq.realized_response(&grid, peq.sample_rate);
    let peak = realized.into_iter().fold(f64::NEG_INFINITY, f64::max);
    assert!(peak > 1e-8, "the set must be boost dominant; peak {peak}");
    assert!(
        (peq.preamp_db() - (-peak)).abs() < 1e-12,
        "preamp_db {} vs -peak {}",
        peq.preamp_db(),
        -peak
    );

    // And the difference from the as-designed curve is the reason the function
    // exists: `frequency_response` does NOT substitute, so the NaN row poisons
    // the whole product and its peak is not a number at all.
    let designed_peak = peq
        .frequency_response(&grid)
        .into_iter()
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        designed_peak.is_nan() || (designed_peak - peak).abs() > 1e-6,
        "frequency_response must not agree here: {designed_peak} vs {peak}"
    );
}

#[test]
fn preamp_db_is_exactly_zero_for_a_cut_dominant_set_while_realized_response_peaks_negative() {
    // The BIAS_CEILING_DB branch (peq.rs `preamp_db`), PINNED, not "fixed".
    // Shipped reason, verbatim from the source: "sos_frequency_response_db's
    // +1e-10 ADDITIVE magnitude floor lifts a near-unity response by up to
    // ~8.7e-10 dB, which would turn a pure-cut cascade's preamp into a tiny
    // negative number at the top grid endpoint. Peaks at or below this ceiling
    // are the bias, not a boost: exactly 0.0 by the DIVERGENCES.md #14
    // convention."
    //
    // So for a cut-dominant set `-preamp_db` is 0.0 while the realized peak is
    // strictly negative, and an implementer who "repaired" this into an equality
    // would be editing R1-1 safety code that has no oracle.
    let peq = ParametricEQ {
        bands: vec![
            peaking_band(1000.0, -8.0, 1.0),
            peaking_band(200.0, -4.0, 0.0), // unstable ⇒ identity ⇒ still cut-only
        ],
        sample_rate: 48000.0,
    };
    let grid = peq.preamp_grid();
    let peak = peq
        .realized_response(&grid, peq.sample_rate)
        .into_iter()
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(peak < 0.0, "expected a cut-dominant peak, got {peak}");
    assert_eq!(peq.preamp_db(), 0.0);
}

#[test]
fn realized_sos_at_a_rate_other_than_self_sample_rate_redesigns_every_row() {
    // The falsifier for the `rate_hz` ARGUMENT, which is A4's live-rate point
    // made mechanical: `combined_sos` hardcodes `self.sample_rate`, so an
    // implementation that ignored `rate_hz` and delegated to it would pass every
    // other test in this file.
    let peq = ParametricEQ {
        bands: vec![
            peaking_band(1000.0, 6.0, 1.0),
            peaking_band(80.0, -3.0, 2.0),
            EQBand {
                filter_type: FilterType::HighShelf,
                fc: 8000.0,
                gain_db: 3.0,
                q: 0.707,
            },
        ],
        sample_rate: 44100.0,
    };
    let at_44k = peq.realized_sos(44100.0);
    let at_48k = peq.realized_sos(48000.0);
    assert_eq!(
        at_44k,
        peq.combined_sos(),
        "self-rate must reproduce the design"
    );
    assert_eq!(at_48k.len(), at_44k.len());
    for (i, (a, b)) in at_48k.iter().zip(&at_44k).enumerate() {
        assert_ne!(a, b, "row {i} was not redesigned at the new rate");
    }
}

#[test]
fn realized_response_equals_frequency_response_when_no_row_is_substituted_at_the_same_rate() {
    // The anti-drift device. `frequency_response` is Tier-1 fixture-pinned, so
    // it is NOT refactored to share a kernel with `realized_response` —
    // extracting its body would edit a frozen function for no behavioural gain.
    // A test is the guard instead, exactly as it is elsewhere in the tree.
    //
    // Stable rows and `rate_hz == self.sample_rate` is the ONLY regime in which
    // the two are supposed to agree.
    let peq = ParametricEQ {
        bands: vec![
            peaking_band(1000.0, 6.0, 1.0),
            peaking_band(80.0, -3.0, 2.0),
            EQBand {
                filter_type: FilterType::LowShelf,
                fc: 120.0,
                gain_db: 2.5,
                q: 0.707,
            },
        ],
        sample_rate: 48000.0,
    };
    // 1e-12 is the CONTRACT. In practice the two are bit-identical today —
    // `realized_sos(self.sample_rate)` reproduces `combined_sos()` exactly when
    // nothing is substituted — but pinning bit equality would turn a harmless
    // future reassociation into a red test for no reason.
    let freqs = log_grid(20000.0, 48.0);
    assert_allclose(
        &peq.realized_response(&freqs, peq.sample_rate),
        &peq.frequency_response(&freqs),
        1e-12,
        1e-12,
        "realized vs designed, all rows stable",
    );
}

#[test]
fn realized_response_differs_from_frequency_response_when_a_row_is_substituted() {
    // The other half, and the reason the function exists at all. Without this,
    // the test above is satisfiable by an implementation that just calls
    // `frequency_response`.
    let peq = one_unstable_row_boost_dominant();
    let freqs = log_grid(20000.0, 48.0);
    let realized = peq.realized_response(&freqs, peq.sample_rate);
    let designed = peq.frequency_response(&freqs);
    assert!(
        realized.iter().all(|v| v.is_finite()),
        "the realized curve must survive an unstable row"
    );
    assert!(
        designed.iter().any(|v| !v.is_finite()) || {
            realized
                .iter()
                .zip(&designed)
                .any(|(a, b)| (a - b).abs() > 1e-6)
        },
        "the two curves must not agree once a row is substituted"
    );
}

#[test]
fn preamp_grid_is_the_grid_preamp_db_evaluates_on() {
    // The falsifier for the factoring. Without it, a `preamp_grid` that returned
    // some other reasonable log grid would still make the peak test above pass
    // by coincidence on a dense enough axis.
    //
    // The band's fc is chosen to fall BETWEEN two 1/48-octave points — that
    // union is what makes a high-Q peak exact regardless of grid density, and it
    // is the one feature a "rebuild a log grid" implementation would drop.
    let fc = 1234.567;
    let peq = ParametricEQ {
        bands: vec![peaking_band(fc, 6.0, 12.0)],
        sample_rate: 48000.0,
    };
    let grid = peq.preamp_grid();
    assert!(
        grid.contains(&fc),
        "the band's fc must be unioned into the grid"
    );
    // Sorted, deduplicated, and spanning [1.0, 0.499 * sample_rate].
    assert!(
        grid.windows(2).all(|w| w[0] < w[1]),
        "grid must be sorted and deduped"
    );
    assert_eq!(grid[0], 1.0);
    assert_eq!(*grid.last().unwrap(), 0.499 * 48000.0);
    // fc is genuinely off-grid: neither of its 1/48-octave neighbours is it.
    let base = log_grid(0.499 * 48000.0, 48.0);
    assert!(
        base.iter().all(|g| (g - fc).abs() > 1e-9),
        "fc must not coincidentally sit on a 1/48-octave point"
    );
    // And the grid is the one preamp_db actually used: its peak reproduces it.
    let peak = peq
        .realized_response(&grid, peq.sample_rate)
        .into_iter()
        .fold(f64::NEG_INFINITY, f64::max);
    assert!((peq.preamp_db() - (-peak)).abs() < 1e-12);
}

#[test]
fn apply_offline_is_sosfilt_of_realized_sos() {
    // The composition, asserted literally against its two graded halves:
    // `sosfilt` is Tier-2 (scipy) and `realized_sos` is Tier-3 (our IDENTITY
    // policy). `apply_offline` has no independent delegate of its own, so this
    // — and the two tests after it — are what pin it.
    let peq = one_unstable_row_boost_dominant();
    let x: Vec<f64> = (0..2048)
        .map(|n| (n as f64 * 0.017).sin() + 0.3 * (n as f64 * 0.31).cos())
        .collect();
    for rate in [44100.0, 48000.0] {
        assert_eq!(
            peq.apply_offline(&x, rate),
            paraeq_dsp::peq::sosfilt(&peq.realized_sos(rate), &x),
            "rate {rate}"
        );
    }
}

#[test]
fn apply_offline_is_the_inverse_fft_of_the_realized_response() {
    // A unit impulse through `apply_offline`, transformed, must reproduce
    // `realized_response` — the frequency-domain and time-domain halves of the
    // same object. This is what makes the correction-aware marker template
    // (B13) and the prediction (B8) the same cascade rather than two.
    let peq = one_unstable_row_boost_dominant();
    let rate = 48000.0;
    let n = 8192;
    let mut impulse = vec![0.0; n];
    impulse[0] = 1.0;
    let ir = peq.apply_offline(&impulse, rate);

    // A log grid inside the IIR's settled band. 20 Hz at 48 kHz has a ~1 s tail
    // for a Q = 1 peaking section, so the low edge is set where an 8192-sample
    // (171 ms) window holds the response.
    let freqs: Vec<f64> = (0..64)
        .map(|k| 200.0 * (20000.0f64 / 200.0).powf(k as f64 / 63.0))
        .collect();
    let want = peq.realized_response(&freqs, rate);
    for (i, &f) in freqs.iter().enumerate() {
        let w = 2.0 * std::f64::consts::PI * f / rate;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (k, v) in ir.iter().enumerate() {
            re += v * (w * k as f64).cos();
            im -= v * (w * k as f64).sin();
        }
        // sos_frequency_response_db's +1e-10 is ADDITIVE on |H|, so mirror it
        // rather than comparing against a bare 20*log10.
        let got = 20.0 * ((re * re + im * im).sqrt() + 1e-10).log10();
        // Measured worst bin: 1.5e-13 dB. The budget is set at 1e-10 dB — still
        // nine orders below anything audible, and loose enough that the IIR's
        // settling tail cannot make it flaky.
        assert!(
            (got - want[i]).abs() < 1e-10,
            "{f:.1} Hz: {got} dB from the IR vs {} dB from realized_response",
            want[i]
        );
    }
}

#[test]
fn apply_offline_is_zero_state_and_deterministic() {
    // No filter state survives a call: the same input twice gives BIT-identical
    // output. An `apply_offline` that cached `zi` between calls would make the
    // verification marker depend on what was rendered before it.
    let peq = one_unstable_row_boost_dominant();
    let x: Vec<f64> = (0..1024)
        .map(|n| ((n * 37 % 101) as f64 / 50.0) - 1.0)
        .collect();
    let first = peq.apply_offline(&x, 48000.0);
    let second = peq.apply_offline(&x, 48000.0);
    assert_eq!(first, second);
    // Zero state also means a leading zero block comes out as zeros.
    let mut padded = vec![0.0; 64];
    padded.extend_from_slice(&x);
    let from_padded = peq.apply_offline(&padded, 48000.0);
    assert!(from_padded[..64].iter().all(|v| *v == 0.0));
    assert_eq!(&from_padded[64..], &first[..]);
}

#[test]
fn realized_helpers_are_well_defined_for_an_empty_band_set() {
    // No bands is a valid state everywhere else in this file, so it is here too:
    // an identity cascade, a flat 0 dB response (exactly, matching
    // `frequency_response`'s own special case), and a pass-through filter.
    let peq = ParametricEQ {
        bands: Vec::new(),
        sample_rate: 48000.0,
    };
    assert!(peq.realized_sos(48000.0).is_empty());
    let freqs = [20.0, 1000.0, 20000.0];
    assert_eq!(peq.realized_response(&freqs, 48000.0), vec![0.0; 3]);
    let x = vec![1.0, -0.5, 0.25, 0.0];
    assert_eq!(peq.apply_offline(&x, 48000.0), x);
    assert_eq!(peq.preamp_db(), 0.0);
}
