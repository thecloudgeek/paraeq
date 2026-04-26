import numpy as np
from pathlib import Path
from paraeq.correction.target_curves import (
    TargetCurve,
    load_target_csv,
    list_builtin_targets,
    compute_correction,
)


def test_load_target_csv():
    targets_dir = Path(__file__).parent.parent / "targets"
    curve = load_target_csv(targets_dir / "flat.csv")
    assert curve.name == "flat"
    assert len(curve.frequencies) == 2
    assert curve.frequencies[0] == 20.0
    assert curve.gains_db[0] == 0.0


def test_target_curve_interpolate():
    curve = TargetCurve(
        name="test",
        frequencies=np.array([20.0, 1000.0, 20000.0]),
        gains_db=np.array([0.0, -3.0, -6.0]),
    )
    query_freqs = np.array([20.0, 510.0, 1000.0, 10500.0, 20000.0])
    interpolated = curve.interpolate(query_freqs)
    assert len(interpolated) == 5
    assert interpolated[0] == 0.0
    assert interpolated[2] == -3.0
    assert interpolated[4] == -6.0


def test_list_builtin_targets():
    targets = list_builtin_targets()
    names = {t.name for t in targets}
    assert "diffuse_field" in names
    assert "flat" in names
    assert "harman_ie_2019" in names
    assert "harman_oe_2018" in names


def test_compute_correction():
    measured = np.array([0.0, 3.0, 6.0])
    target = np.array([0.0, 0.0, 0.0])
    correction = compute_correction(measured, target)
    np.testing.assert_array_almost_equal(correction, [0.0, -3.0, -6.0])


def test_target_curve_metadata_fields_default_to_none():
    curve = TargetCurve(
        name="x",
        frequencies=np.array([20.0, 20000.0]),
        gains_db=np.array([0.0, 0.0]),
    )
    assert curve.category is None
    assert curve.description is None
    assert curve.source is None


def test_target_curve_metadata_fields_can_be_set():
    curve = TargetCurve(
        name="x",
        frequencies=np.array([20.0, 20000.0]),
        gains_db=np.array([0.0, 0.0]),
        category="reference",
        description="A flat curve.",
        source="hand-authored",
    )
    assert curve.category == "reference"
    assert curve.description == "A flat curve."
    assert curve.source == "hand-authored"


def test_load_target_csv_parses_metadata_header():
    fixtures = Path(__file__).parent / "fixtures"
    curve = load_target_csv(fixtures / "target_curve_with_metadata.csv")
    assert curve.name == "Sample Target"
    assert curve.category == "reference"
    assert curve.description == "A two-point sample target for testing metadata parsing."
    assert curve.source == "hand-authored"
    assert len(curve.frequencies) == 2
    assert curve.gains_db[1] == -3.0


def test_load_target_csv_ignores_unknown_keys_and_is_case_insensitive():
    fixtures = Path(__file__).parent / "fixtures"
    curve = load_target_csv(fixtures / "target_curve_unknown_and_mixed_case.csv")
    assert curve.name == "Mixed Case Sample"
    assert curve.category == "reference"
    assert curve.description == "Surrounding whitespace is preserved-stripped."
    # source not in fixture → falls back to None
    assert curve.source is None
    # data still parses normally
    assert len(curve.frequencies) == 2


def test_load_target_csv_no_metadata_falls_back_to_stem():
    fixtures = Path(__file__).parent / "fixtures"
    curve = load_target_csv(fixtures / "target_curve_no_metadata.csv")
    assert curve.name == "target_curve_no_metadata"  # file stem
    assert curve.category is None
    assert curve.description is None
    assert curve.source is None
    assert len(curve.frequencies) == 3
    assert curve.gains_db[2] == -6.0
