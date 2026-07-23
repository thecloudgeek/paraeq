import numpy as np
import pytest
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
    assert curve.name == "Flat"  # populated from "# name:" header
    assert curve.category == "reference"
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
    # Names come from "# name:" headers (display names), not file stems.
    assert "Flat" in names


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


def test_list_builtin_targets_returns_seven():
    targets = list_builtin_targets()
    assert len(targets) == 7
    names = {t.name for t in targets}
    assert names == {
        "B&K Room 1974",
        "Diffuse Field",
        "Flat",
        "Harman In-Ear 2019",
        "Harman In-Ear 2019 (No Bass Shelf)",
        "Harman Over-Ear 2018",
        "Harman Over-Ear 2018 (No Bass Shelf)",
    }


def test_builtin_curves_have_descriptions_and_categories():
    targets = list_builtin_targets()
    for t in targets:
        assert t.description, f"{t.name!r} missing description"
        assert t.category in ("in-ear", "over-ear", "reference", "room"), (
            f"{t.name!r} has unexpected category {t.category!r}"
        )
        assert t.source, f"{t.name!r} missing source"


def test_load_target_csv_raises_value_error_on_malformed_data_line(tmp_path):
    # A line with one value but no comma should raise ValueError per the
    # docstring contract (not IndexError from out-of-bounds access).
    bad_csv = tmp_path / "bad.csv"
    bad_csv.write_text("# name: Bad\n# frequency_hz,gain_db\n20.0,0.0\n1000.0\n")
    with pytest.raises(ValueError, match="Expected 'frequency,gain'"):
        load_target_csv(bad_csv)


# ---------------------------------------------------------------------------
# Interactive editor: anchor deviation model + auto-match
# ---------------------------------------------------------------------------

from paraeq.correction.target_curves import (  # noqa: E402
    ANCHOR_FREQS,
    build_anchor_target,
    match_closest_target,
)


def test_anchor_freqs_are_sorted_and_span_audio_band():
    assert ANCHOR_FREQS[0] == 20.0
    assert ANCHOR_FREQS[-1] == 20000.0
    assert np.all(np.diff(ANCHOR_FREQS) > 0)
    assert len(ANCHOR_FREQS) == 16


def test_build_anchor_target_zero_offsets_equals_base():
    base = TargetCurve(
        name="Harmanish",
        frequencies=np.array([20.0, 200.0, 1000.0, 3000.0, 20000.0]),
        gains_db=np.array([4.0, 0.0, -1.0, 6.0, -8.0]),
    )
    offsets = np.zeros(len(ANCHOR_FREQS))
    result = build_anchor_target(base, ANCHOR_FREQS, offsets)
    # Sampling at the base's own anchor frequencies must reproduce the base.
    np.testing.assert_allclose(
        result.interpolate(base.frequencies), base.gains_db, atol=1e-6
    )


def test_build_anchor_target_single_offset_bends_locally():
    base = TargetCurve(
        name="Flat",
        frequencies=np.array([20.0, 20000.0]),
        gains_db=np.array([0.0, 0.0]),
    )
    offsets = np.zeros(len(ANCHOR_FREQS))
    idx_1250 = int(np.argmin(np.abs(ANCHOR_FREQS - 1250.0)))
    offsets[idx_1250] = 6.0
    result = build_anchor_target(base, ANCHOR_FREQS, offsets)
    at = result.interpolate(np.array([1250.0, 20.0, 20000.0]))
    assert abs(at[0] - 6.0) < 0.5          # bent up at the dragged anchor
    assert abs(at[1] - 0.0) < 0.5          # boundary held near zero
    assert abs(at[2] - 0.0) < 0.5


def test_build_anchor_target_does_not_mutate_base():
    base = TargetCurve(
        name="Flat",
        frequencies=np.array([20.0, 20000.0]),
        gains_db=np.array([0.0, 0.0]),
    )
    original = base.gains_db.copy()
    offsets = np.zeros(len(ANCHOR_FREQS))
    offsets[5] = 3.0
    build_anchor_target(base, ANCHOR_FREQS, offsets)
    np.testing.assert_array_equal(base.gains_db, original)


def test_match_closest_target_picks_level_aligned_best():
    freqs = np.array([20.0, 200.0, 1000.0, 5000.0, 20000.0])
    measured = np.array([2.0, 0.0, -3.0, 4.0, -6.0])
    # near: same shape as measured but shifted +10 dB (level-aligned → ~0 residual)
    near = TargetCurve(name="near", frequencies=freqs, gains_db=measured + 10.0)
    # far: a different shape
    far = TargetCurve(name="far", frequencies=freqs, gains_db=np.array([-8.0, 0.0, 8.0, -8.0, 8.0]))
    chosen = match_closest_target(freqs, measured, [far, near])
    assert chosen.name == "near"


def test_match_closest_target_returns_a_member():
    freqs = np.array([20.0, 1000.0, 20000.0])
    measured = np.array([0.0, 0.0, 0.0])
    targets = list_builtin_targets()
    chosen = match_closest_target(freqs, measured, targets)
    assert chosen in targets
