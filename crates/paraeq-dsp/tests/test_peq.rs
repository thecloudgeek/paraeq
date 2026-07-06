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
