//! R1-1's auto-preamp, for the two correction arms that do not carry bands.
//!
//! **Control plane only.** Every function here allocates and runs an FFT or a
//! ~700-point response sweep; none of it may ever be called from
//! [`crate::shared::RtProcessor::process_block`]. They are called once per
//! `build_correction`, off the realtime thread, and the result rides into the
//! chain as [`crate::chain::Correction::preamp_lin`].
//!
//! The `Peq` arm does NOT live here: it calls
//! `paraeq_dsp::peq::ParametricEQ::preamp_db()` verbatim, because that is the
//! same function the AutoEQ export calls and spec `:117` requires the two
//! numbers to agree exactly. What is here is the same idea applied to the two
//! arms that arrive with coefficients already baked, where there are no bands
//! to hand `ParametricEQ`:
//!
//! * `Iir` -- the realized SOS cascade's magnitude, over the same log grid
//!   `preamp_db()` uses, MINUS the per-band `fc` union: an SOS row has
//!   forgotten the band it came from, so there is no `fc` to union in. The
//!   grid is 1/48-octave (~1.5% spacing), which resolves a moderate-Q peak;
//!   a very high-Q baked row can therefore read slightly low. That is the
//!   honest cost of baked coefficients and one more reason `Peq` is the
//!   preferred config shape (R1-6 / Open Q1).
//! * `Fir` -- spec `:101`, verbatim: *"For the FIR arm, 'the realized
//!   cascade' is the FIR's own magnitude response -- `preamp_lin = 1.0 /
//!   max(1.0, max|H(f)|)` over the same grid, computed with one FFT of the
//!   tap vector on the control plane."*
//!
//! Both take the WORST channel: a multi-channel config must not clip on the
//! loudest channel because the quietest one needed less headroom.

use paraeq_dsp::biquad;
use realfft::RealFftPlanner;

/// Peaks at or below this many dB are the `+1e-10` magnitude floor
/// `biquad::sos_frequency_response_db` adds, not a real boost, and get a
/// preamp of exactly 0.0. Mirrors `ParametricEQ::preamp_db`'s constant of the
/// same name (both exist because the two arms cannot share one function --
/// one starts from bands, the other from rows).
const BIAS_CEILING_DB: f64 = 1e-8;

/// Zero-padding factor for the FIR magnitude FFT. A length-`n` FIR's
/// magnitude is a trigonometric polynomial of degree `n - 1`, so 8x
/// oversampling puts a grid point within a small fraction of any lobe's
/// width; with a power-of-two transform the grid also contains DC, Nyquist
/// and (when the length is a multiple of 4) `fs/4` exactly.
const FIR_OVERSAMPLE: usize = 8;

/// The auto-preamp in dB for a set of baked SOS cascades: `-max(0, peak of
/// the realized cascade)` over every channel, never positive.
///
/// "Realized" is load-bearing and mirrors both `ParametricEQ::preamp_db` and
/// [`crate::chain::build_iir`]'s R1-3 funnel: a row failing
/// [`biquad::is_stable`] is evaluated AS THE IDENTITY SECTION, because that is
/// what the funnel will install. Without the mirror one `q = 0`/NaN design
/// turns the response NaN, the max fold discards NaN, and the surviving
/// boosts get zero headroom -- fail-unsafe in exactly the direction the
/// preamp exists to prevent.
pub fn sos_preamp_db(sos_per_channel: &[Vec<[f64; 6]>], sample_rate: f64) -> f64 {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return 0.0;
    }
    let grid = log_grid(sample_rate);
    let mut peak = f64::NEG_INFINITY;
    for set in sos_per_channel {
        if set.is_empty() {
            continue;
        }
        let realized: Vec<[f64; 6]> = set
            .iter()
            .map(|row| {
                if biquad::is_stable(row) {
                    *row
                } else {
                    biquad::IDENTITY
                }
            })
            .collect();
        for db in biquad::sos_frequency_response_db(&realized, &grid, sample_rate) {
            if db > peak {
                peak = db;
            }
        }
    }
    if peak > BIAS_CEILING_DB {
        -peak
    } else {
        0.0
    }
}

/// The auto-preamp in dB for a set of FIR tap vectors: `-max(0, 20*log10
/// max|H(f)|)` over every channel, i.e. `preamp_lin = 1/max(1, max|H|)`
/// expressed in dB so every arm reports one kind of number.
///
/// One real FFT per channel of the zero-padded tap vector (spec `:101`), on
/// the control plane.
pub fn fir_preamp_db(firs: &[Vec<f64>]) -> f64 {
    let Some(longest) = firs.iter().map(Vec::len).max() else {
        return 0.0;
    };
    if longest == 0 {
        return 0.0;
    }
    let n_fft = (longest * FIR_OVERSAMPLE).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n_fft);
    let mut padded = vec![0.0f64; n_fft];
    let mut spectrum = fwd.make_output_vec();
    let mut peak = 0.0f64;
    for fir in firs {
        padded.fill(0.0);
        padded[..fir.len()].copy_from_slice(fir);
        if fwd.process(&mut padded, &mut spectrum).is_err() {
            // Length mismatch is impossible here (both come from `n_fft`);
            // refusing to guess beats an unwrap on the preamp path.
            return 0.0;
        }
        for bin in &spectrum {
            let mag = bin.norm();
            if mag > peak {
                peak = mag;
            }
        }
    }
    if !peak.is_finite() || peak <= 1.0 {
        // `max(1.0, max|H|)`: a FIR that only cuts gets no preamp, matching
        // DIVERGENCES.md #14 (ParaEQ's preamp is clamped at <= 0 while
        // AutoEQ's is signed). A non-finite tap set gets none either -- the
        // R1-2 output backstop, not an invented number, is what handles it.
        return 0.0;
    }
    -20.0 * peak.log10()
}

/// dB -> linear for a preamp: always in `(0, 1]` because `preamp_db <= 0`.
pub fn preamp_lin(preamp_db: f64) -> f32 {
    if !preamp_db.is_finite() || preamp_db >= 0.0 {
        return 1.0;
    }
    10f64.powf(preamp_db / 20.0) as f32
}

/// The 1/48-octave log grid `ParametricEQ::preamp_db` evaluates on, from
/// 1.0 Hz to `0.499 * sample_rate`, both endpoints included (a shelf's
/// maximum sits at DC or Nyquist).
fn log_grid(sample_rate: f64) -> Vec<f64> {
    let f_max = (0.499 * sample_rate).max(1.0);
    let mut grid: Vec<f64> = Vec::new();
    for k in 0.. {
        let f = 2f64.powf(f64::from(k) / 48.0);
        if f >= f_max {
            break;
        }
        grid.push(f);
    }
    grid.push(f_max);
    grid
}
