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
