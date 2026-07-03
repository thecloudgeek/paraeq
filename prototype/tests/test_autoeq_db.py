from pathlib import Path
from unittest import mock

import pytest

from paraeq.correction.autoeq_db import (
    AutoEQClient,
    AutoEQEntry,
    ParsedPreset,
    parse_index,
    parse_parametric_eq,
)
from paraeq.correction.parametric_eq import EQBand

FIXTURES = Path(__file__).parent / "fixtures"


# ---------------------------------------------------------------------------
# parse_index
# ---------------------------------------------------------------------------


def test_parse_index_extracts_entries():
    text = (FIXTURES / "autoeq_index_sample.md").read_text()
    entries = parse_index(text)
    assert len(entries) == 3
    hd600 = entries[0]
    assert isinstance(hd600, AutoEQEntry)
    assert hd600.name == "Sennheiser HD 600"
    assert hd600.source == "oratory1990"
    assert hd600.rig == "over-ear"
    assert hd600.rel_path == "oratory1990/over-ear/Sennheiser HD 600"


def test_parse_index_handles_multiword_rig():
    text = (FIXTURES / "autoeq_index_sample.md").read_text()
    entries = parse_index(text)
    se846 = entries[1]
    assert se846.name == "Shure SE846"
    assert se846.source == "crinacle"
    assert se846.rig == "711 in-ear"
    assert se846.rel_path == "crinacle/711 in-ear/Shure SE846"


def test_parse_index_ignores_non_entry_lines():
    entries = parse_index("# Heading\n\nsome prose\n")
    assert entries == []


# ---------------------------------------------------------------------------
# parse_parametric_eq
# ---------------------------------------------------------------------------


def test_parse_parametric_eq_captures_preamp_and_bands():
    text = (FIXTURES / "autoeq_parametric_sample.txt").read_text()
    parsed = parse_parametric_eq(text)
    assert isinstance(parsed, ParsedPreset)
    assert parsed.preamp_db == pytest.approx(-6.2)
    assert len(parsed.bands) == 4
    assert all(isinstance(b, EQBand) for b in parsed.bands)


def test_parse_parametric_eq_maps_filter_types_incl_aliases():
    text = (FIXTURES / "autoeq_parametric_sample.txt").read_text()
    bands = parse_parametric_eq(text).bands
    assert bands[0].filter_type == "peaking"     # PK
    assert bands[1].filter_type == "low_shelf"   # LSC
    assert bands[2].filter_type == "high_shelf"  # HS alias
    assert bands[3].filter_type == "peaking"     # XYZ unknown → peaking
    assert bands[0].fc == 105.0
    assert bands[0].gain_db == -1.7
    assert bands[0].q == 0.70


def test_parse_parametric_eq_defaults_preamp_to_zero_when_absent():
    parsed = parse_parametric_eq("Filter 1: ON PK Fc 100 Hz Gain 1.0 dB Q 1.0")
    assert parsed.preamp_db == 0.0
    assert len(parsed.bands) == 1


# ---------------------------------------------------------------------------
# AutoEQClient
# ---------------------------------------------------------------------------


def test_fetch_index_cache_miss_downloads_and_writes(tmp_path):
    index_text = (FIXTURES / "autoeq_index_sample.md").read_text()
    client = AutoEQClient(cache_dir=tmp_path)
    with mock.patch(
        "paraeq.correction.autoeq_db._http_get", return_value=index_text
    ) as get:
        entries = client.fetch_index()
    assert len(entries) == 3
    get.assert_called_once()
    assert (tmp_path / "INDEX.md").read_text() == index_text


def test_fetch_index_cache_hit_does_not_download(tmp_path):
    index_text = (FIXTURES / "autoeq_index_sample.md").read_text()
    (tmp_path / "INDEX.md").write_text(index_text)
    client = AutoEQClient(cache_dir=tmp_path)
    with mock.patch(
        "paraeq.correction.autoeq_db._http_get", side_effect=AssertionError("must not fetch")
    ):
        entries = client.fetch_index()
    assert len(entries) == 3


def test_fetch_preset_tries_both_filename_casings(tmp_path):
    preset_text = (FIXTURES / "autoeq_parametric_sample.txt").read_text()
    client = AutoEQClient(cache_dir=tmp_path)
    entry = AutoEQEntry(
        name="Sennheiser HD 600",
        source="oratory1990",
        rig="over-ear",
        rel_path="oratory1990/over-ear/Sennheiser HD 600",
    )

    def fake_get(url):
        if url.endswith("ParametricEq.txt"):  # lowercase q fails first
            raise OSError("404")
        return preset_text  # capital Q succeeds

    with mock.patch("paraeq.correction.autoeq_db._http_get", side_effect=fake_get):
        parsed = client.fetch_preset(entry)
    assert parsed.preamp_db == pytest.approx(-6.2)
    assert len(parsed.bands) == 4


def test_fetch_index_write_is_atomic_no_temp_left(tmp_path):
    index_text = (FIXTURES / "autoeq_index_sample.md").read_text()
    client = AutoEQClient(cache_dir=tmp_path)
    with mock.patch(
        "paraeq.correction.autoeq_db._http_get", return_value=index_text
    ):
        client.fetch_index()
    # Only the final file should remain — no leftover ".tmp" from the atomic write.
    assert sorted(p.name for p in tmp_path.iterdir()) == ["INDEX.md"]


def test_fetch_preset_uses_cache_on_second_call(tmp_path):
    preset_text = (FIXTURES / "autoeq_parametric_sample.txt").read_text()
    client = AutoEQClient(cache_dir=tmp_path)
    entry = AutoEQEntry(
        name="Shure SE846",
        source="crinacle",
        rig="711 in-ear",
        rel_path="crinacle/711 in-ear/Shure SE846",
    )
    with mock.patch(
        "paraeq.correction.autoeq_db._http_get", return_value=preset_text
    ) as get:
        client.fetch_preset(entry)
        client.fetch_preset(entry)
    get.assert_called_once()  # second call served from cache
