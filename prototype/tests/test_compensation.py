import numpy as np
import pytest
from pathlib import Path
from paraeq.measurement.compensation import load_compensation, apply_compensation

FIXTURE_DIR = Path(__file__).parent / "fixtures"


def test_load_compensation_csv():
    freqs, gains_db = load_compensation(FIXTURE_DIR / "test_compensation.csv")
    assert len(freqs) == 7
    assert len(gains_db) == 7
    assert freqs[0] == 20.0
    assert gains_db[0] == 0.5
    assert freqs[-1] == 20000.0
    assert gains_db[-1] == -2.0


def test_apply_compensation_subtracts():
    n_fft = 4096
    sample_rate = 48000
    freqs_fft = np.fft.rfftfreq(n_fft, 1.0 / sample_rate)
    magnitude_db = np.zeros_like(freqs_fft)
    comp_freqs = np.array([20.0, 1000.0, 20000.0])
    comp_gains = np.array([1.0, 0.0, -1.0])
    corrected = apply_compensation(magnitude_db, freqs_fft, comp_freqs, comp_gains)
    idx_1k = np.argmin(np.abs(freqs_fft - 1000.0))
    assert abs(corrected[idx_1k]) < 0.01
    idx_20 = np.argmin(np.abs(freqs_fft - 20.0))
    assert abs(corrected[idx_20] - (-1.0)) < 0.1


def test_apply_compensation_preserves_shape():
    n = 2049
    magnitude_db = np.zeros(n)
    freqs_fft = np.linspace(0, 24000, n)
    comp_freqs = np.array([20.0, 20000.0])
    comp_gains = np.array([0.0, 0.0])
    result = apply_compensation(magnitude_db, freqs_fft, comp_freqs, comp_gains)
    assert result.shape == (n,)


def test_load_compensation_minidsp_raw_format():
    """miniDSP REW-style: quoted-string headers, * comments, 3-col whitespace data.

    Neither header line begins with a number, nor does any * comment, so the
    REW rule loads exactly the seven (freq, SPL, phase) rows — taking only
    freq and SPL.
    """
    freqs, gains_db = load_compensation(FIXTURE_DIR / "test_compensation_minidsp.txt")
    assert len(freqs) == 7
    assert len(gains_db) == 7
    assert freqs[0] == 20.0
    assert gains_db[0] == 0.5
    assert freqs[-1] == 20000.0
    assert gains_db[-1] == -2.0


def test_load_compensation_minidsp_ignores_phase_column():
    """The third column (phase in degrees) must not leak into gains_db."""
    freqs, gains_db = load_compensation(FIXTURE_DIR / "test_compensation_minidsp.txt")
    # Phase is 0.0 everywhere in the fixture; verify gains carry the SPL values, not zeros.
    assert gains_db[3] == -0.2  # 1000 Hz row, SPL = -0.2 (not phase=0.0)


def test_load_compensation_csv_still_works():
    """The one REW rule must not regress the existing CSV path."""
    freqs, gains_db = load_compensation(FIXTURE_DIR / "test_compensation.csv")
    assert freqs[0] == 20.0
    assert gains_db[0] == 0.5


def test_load_compensation_single_header_keeps_first_row():
    """UMIK-1 0-degree: ONE quoted header, tab-separated rows.

    The regression this pins: the old parser sniffed the leading quote and
    then hardcoded skiprows=2, eating the 20 Hz row along with the single
    header — after which interpolation edge-held from 50 Hz downwards. Only
    lines beginning with a number are data, so all seven rows load.
    """
    freqs, gains_db = load_compensation(FIXTURE_DIR / "test_compensation_umik_1header.txt")
    assert len(freqs) == 7
    assert freqs[0] == 20.0
    assert gains_db[0] == 0.5
    assert freqs[-1] == 20000.0
    assert gains_db[-1] == -2.0


def test_load_compensation_rejects_header_only_file(tmp_path):
    """A file with no data rows is an error, not a silent empty curve."""
    path = tmp_path / "headers_only.txt"
    path.write_text('"Sens Factor =-0.4210dB, SERNO: 7103798"\n* no data here\n')
    with pytest.raises(ValueError):
        load_compensation(path)


def test_load_compensation_rejects_row_missing_gain(tmp_path):
    """First column parses, second does not: a malformed row, not a header."""
    path = tmp_path / "bad_row.txt"
    path.write_text("20.0\t0.5\n50.0\n")
    with pytest.raises(ValueError):
        load_compensation(path)
