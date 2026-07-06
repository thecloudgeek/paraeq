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
