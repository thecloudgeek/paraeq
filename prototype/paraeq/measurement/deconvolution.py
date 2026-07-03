"""Impulse response extraction via Farina deconvolution."""

import logging

import numpy as np

logger = logging.getLogger(__name__)


def deconvolve(
    recorded: np.ndarray,
    sweep: np.ndarray,
    sample_rate: int,
    f_start: float = 20.0,
    f_end: float = 20000.0,
) -> np.ndarray:
    """Extract the impulse response from a recorded sweep measurement.

    Uses FFT-based spectral division (Wiener / regularized deconvolution) to
    extract the impulse response.  The sweep FFT is computed once and reused
    across all channels.

    Args:
        recorded: Recorded signal. 1D (mono) or 2D (samples x channels).
        sweep: The original log sweep that was played.
        sample_rate: Sample rate in Hz.
        f_start: Start frequency of the sweep (unused; reserved for future
            band-limited windowing).
        f_end: End frequency of the sweep (unused; reserved for future
            band-limited windowing).

    Returns:
        Impulse response array with the same length as *recorded* and the same
        channel count.  Mono recordings return a 1-D array; stereo/multi-channel
        recordings return a 2-D array of shape (samples, channels).
    """
    logger.debug(
        "deconvolve called",
        extra={
            "recorded_shape": recorded.shape,
            "sweep_len": len(sweep),
            "sample_rate": sample_rate,
            "f_start": f_start,
            "f_end": f_end,
        },
    )

    stereo = recorded.ndim == 2
    if stereo:
        n_channels = recorded.shape[1]
    else:
        n_channels = 1
        recorded = recorded[:, np.newaxis]

    n_rec = len(recorded)
    n_fft = int(2 ** np.ceil(np.log2(n_rec + len(sweep))))

    # Compute sweep spectrum once
    sweep_fft = np.fft.rfft(sweep, n_fft)
    sweep_power = np.abs(sweep_fft) ** 2
    # Regularisation: small fraction of peak power to avoid division by zero
    eps = 1e-10 * sweep_power.max()
    inv_sweep_fft = np.conj(sweep_fft) / (sweep_power + eps)

    ir_channels = []
    for ch in range(n_channels):
        rec_fft = np.fft.rfft(recorded[:, ch], n_fft)
        ir_full = np.fft.irfft(rec_fft * inv_sweep_fft, n_fft)
        ir_channels.append(ir_full[:n_rec])

    ir = np.column_stack(ir_channels)
    if not stereo:
        ir = ir[:, 0]

    logger.debug(
        "deconvolve complete",
        extra={"ir_shape": ir.shape},
    )

    return ir
