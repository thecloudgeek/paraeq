"""Biquad filter coefficient computation (Audio EQ Cookbook formulas)."""

import numpy as np
from scipy.signal import sosfreqz


def biquad_peaking(fc: float, gain_db: float, q: float, sample_rate: int) -> np.ndarray:
    a_lin = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * np.pi * fc / sample_rate
    alpha = np.sin(w0) / (2.0 * q)
    b0 = 1.0 + alpha * a_lin
    b1 = -2.0 * np.cos(w0)
    b2 = 1.0 - alpha * a_lin
    a0 = 1.0 + alpha / a_lin
    a1 = -2.0 * np.cos(w0)
    a2 = 1.0 - alpha / a_lin
    return np.array([[b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]])


def biquad_low_shelf(fc: float, gain_db: float, q: float, sample_rate: int) -> np.ndarray:
    a_lin = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * np.pi * fc / sample_rate
    alpha = np.sin(w0) / (2.0 * q)
    cos_w0 = np.cos(w0)
    two_sqrt_a_alpha = 2.0 * np.sqrt(a_lin) * alpha
    b0 = a_lin * ((a_lin + 1) - (a_lin - 1) * cos_w0 + two_sqrt_a_alpha)
    b1 = 2.0 * a_lin * ((a_lin - 1) - (a_lin + 1) * cos_w0)
    b2 = a_lin * ((a_lin + 1) - (a_lin - 1) * cos_w0 - two_sqrt_a_alpha)
    a0 = (a_lin + 1) + (a_lin - 1) * cos_w0 + two_sqrt_a_alpha
    a1 = -2.0 * ((a_lin - 1) + (a_lin + 1) * cos_w0)
    a2 = (a_lin + 1) + (a_lin - 1) * cos_w0 - two_sqrt_a_alpha
    return np.array([[b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]])


def biquad_high_shelf(fc: float, gain_db: float, q: float, sample_rate: int) -> np.ndarray:
    a_lin = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * np.pi * fc / sample_rate
    alpha = np.sin(w0) / (2.0 * q)
    cos_w0 = np.cos(w0)
    two_sqrt_a_alpha = 2.0 * np.sqrt(a_lin) * alpha
    b0 = a_lin * ((a_lin + 1) + (a_lin - 1) * cos_w0 + two_sqrt_a_alpha)
    b1 = -2.0 * a_lin * ((a_lin - 1) + (a_lin + 1) * cos_w0)
    b2 = a_lin * ((a_lin + 1) + (a_lin - 1) * cos_w0 - two_sqrt_a_alpha)
    a0 = (a_lin + 1) - (a_lin - 1) * cos_w0 + two_sqrt_a_alpha
    a1 = 2.0 * ((a_lin - 1) - (a_lin + 1) * cos_w0)
    a2 = (a_lin + 1) - (a_lin - 1) * cos_w0 - two_sqrt_a_alpha
    return np.array([[b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]])


def biquad_notch(fc: float, q: float, sample_rate: int) -> np.ndarray:
    w0 = 2.0 * np.pi * fc / sample_rate
    alpha = np.sin(w0) / (2.0 * q)
    b0 = 1.0
    b1 = -2.0 * np.cos(w0)
    b2 = 1.0
    a0 = 1.0 + alpha
    a1 = -2.0 * np.cos(w0)
    a2 = 1.0 - alpha
    return np.array([[b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]])


def biquad_frequency_response(sos: np.ndarray, freqs: np.ndarray, sample_rate: int) -> np.ndarray:
    worN = freqs * 2.0 * np.pi / sample_rate
    _, h = sosfreqz(sos, worN=worN)
    return 20.0 * np.log10(np.abs(h) + 1e-10)
