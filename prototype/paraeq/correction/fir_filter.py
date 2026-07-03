"""FIR correction filter design via frequency sampling."""

import numpy as np
from scipy.signal import minimum_phase


def design_fir_correction(
    correction_db: np.ndarray,
    freqs: np.ndarray,
    n_taps: int = 4096,
    phase: str = "minimum",
) -> np.ndarray:
    """Design a FIR correction filter from a correction curve.

    Args:
        correction_db: Desired correction in dB at each frequency bin.
        freqs: Frequency of each bin in Hz (from rfftfreq).
        n_taps: Desired FIR filter length.
        phase: "minimum" for minimum-phase or "linear" for linear-phase.

    Returns:
        FIR filter coefficients as a 1D float64 array of length n_taps.
    """
    magnitude = 10.0 ** (correction_db / 20.0)

    if phase == "linear":
        return _design_linear_phase(magnitude, n_taps)
    else:
        return _design_minimum_phase(magnitude, n_taps)


def _design_linear_phase(magnitude: np.ndarray, n_taps: int) -> np.ndarray:
    n_fft_bins = n_taps // 2 + 1
    target_mag = np.interp(
        np.linspace(0, 1, n_fft_bins),
        np.linspace(0, 1, len(magnitude)),
        magnitude,
    )
    spectrum = target_mag.astype(np.complex128)
    ir = np.fft.irfft(spectrum, n_taps)
    ir = np.roll(ir, n_taps // 2)
    window = np.hanning(n_taps)
    ir *= window
    ir /= np.max(np.abs(ir))
    ir = (ir + ir[::-1]) / 2.0
    return ir


def _design_minimum_phase(magnitude: np.ndarray, n_taps: int) -> np.ndarray:
    n_proto = n_taps * 2 - 1
    n_fft_bins = n_proto // 2 + 1
    target_mag = np.interp(
        np.linspace(0, 1, n_fft_bins),
        np.linspace(0, 1, len(magnitude)),
        magnitude,
    )
    # The homomorphic minimum_phase algorithm takes the square root of the input spectrum's
    # magnitude (via cepstrum halving). To obtain the correct output magnitude M, the
    # prototype must be built with magnitude M^2 so that sqrt(M^2) = M after conversion.
    target_mag_sq = target_mag ** 2
    spectrum = target_mag_sq.astype(np.complex128)
    proto = np.fft.irfft(spectrum, n_proto)
    proto = np.roll(proto, n_proto // 2)
    window = np.hanning(n_proto)
    proto *= window
    min_ph = minimum_phase(proto, method="homomorphic", n_fft=2 ** int(np.ceil(np.log2(n_proto * 4))))
    if len(min_ph) >= n_taps:
        result = min_ph[:n_taps]
    else:
        result = np.zeros(n_taps)
        result[: len(min_ph)] = min_ph
    return result
