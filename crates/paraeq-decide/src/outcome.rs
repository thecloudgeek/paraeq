//! `decide()`'s output: one serializable value the two front-ends and the
//! Advanced drawer all render, plus the typed refusals that earn auto mode the
//! right to hide everything.

use crate::decisions::Decisions;
use paraeq_dsp::peq::EQBand;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DecisionSet {
    /// The derived curves both front-ends plot. Not decisions — products.
    pub analysis: Analysis,
    /// `None` iff `verdict == Refuse`. Rate-independent by construction.
    pub correction: Option<CorrectionPlan>,
    pub decisions: Decisions,
    pub diagnostics: Vec<Diagnostic>,
    pub verdict: Verdict,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Verdict {
    Proceed,
    /// At least one Warn diagnostic. `correction` is present and installable.
    ProceedWithWarnings,
    /// At least one Refuse diagnostic. `correction` is None, but `decisions`,
    /// `analysis` and `evidence` are still populated as far as they got — a
    /// refusal must be able to explain itself and the guided path must be able
    /// to pick up where auto stopped. This is why `decide()` returns
    /// `DecisionSet`, not `Result<DecisionSet, E>`.
    Refuse,
}

/// The curves every decision was read off, retained so a refusal can plot its
/// own reasoning and the drawer can show the margin.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Analysis {
    /// Per channel, dB, on `freqs_hz`: the aligned, averaged, smoothed curve.
    pub averaged_db: Vec<Vec<f64>>,
    /// Measured phase minus the phase of its minimum-phase reconstruction,
    /// seconds, on `freqs_hz`. Flat over a region ⇒ boost is meaningful there.
    pub excess_group_delay_s: Vec<f64>,
    /// The log-f analysis grid every curve here is sampled on.
    pub freqs_hz: Vec<f64>,
    /// Aligned, smoothed, per-position curves, dB — index-parallel to
    /// `MeasurementBundle::positions`. σ(f) is one pass over these.
    pub per_position_db: Vec<Vec<f64>>,
    /// Per-bin standard deviation in dB across positions, on `freqs_hz`.
    pub sigma_db: Vec<f64>,
}

/// Bands and a design rate, never baked coefficients.
///
/// The engine rebuilds its chain on every format change, so a plan carrying
/// SOS would reinstall coefficients designed for the old rate: at 44.1→48 kHz
/// a 47 Hz mode filter lands at 51 Hz with the wrong Q. Coefficients are
/// derived at the LIVE rate, at install time, from these bands.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CorrectionPlan {
    /// Per channel.
    pub bands: Vec<Vec<EQBand>>,
    /// The rate the bands were fitted at. Provenance only — never the rate the
    /// engine designs at.
    pub design_rate: f64,
    /// `-max(0, peak of the REALIZED cascade)`. No headroom constant.
    ///
    /// Reaches the engine as `Correction.preamp_lin` — inside the correction,
    /// applied on the **corrected path only** — not as a separate
    /// `EngineCommand::SetGainDb`, which is applied on both chain paths and
    /// would leave the bypassed side of an A/B quieter by the whole preamp. It
    /// is not just the export text either. See
    /// `docs/specs/2026-07-15-engine-hardening-design.md` R1-1 and
    /// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` section D-1.
    pub preamp_db: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    /// Which position, if position-scoped.
    pub position: Option<usize>,
    /// Plain-language remedy. Rendered in Rust, same reason as Rationale.
    pub remedy: String,
    pub severity: Severity,
    /// The number that tripped it — so the drawer can show the margin.
    pub value: Option<f64>,
}

/// `Refuse` means *we do not know how to do this correctly and will not
/// guess*. `Warn` means *we did it, and here is what you should know*. The
/// distinction is not severity theatre: a `Refuse` produces no installable
/// correction, so the system stays as it was.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Severity {
    /// Cannot proceed. `verdict = Refuse`, `correction = None`.
    Refuse,
    /// Proceed with a caveat; may de-weight a position.
    Warn,
}

/// One per row of the spec's refusal table. Severity is carried on the
/// [`Diagnostic`], not implied by the code, because two of these fire at two
/// thresholds with two severities.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DiagnosticCode {
    /// Span > 40 dB or midband tilt > 20 dB/decade: not a loudspeaker or a
    /// headphone.
    AbsurdCurve,
    /// EARS HEQ/HPN/IDF: applying another target would apply it twice.
    CalHasTargetBakedIn,
    CalMalformed,
    CalMissing,
    /// `|g[i] − (g[i−1]+g[i+1])/2| > 1.5 dB`. Not hypothetical: a shipping
    /// vendor file has a bogus 0.0000 at 19.611 Hz between −3.13 and −3.11.
    CalNeighbourOutlier,
    ClippingPosition,
    ClippingSession,
    /// Multi-position data reached a vector/coherent routine. It collapses
    /// toward the incoherent floor `−10log₁₀(N)` once spread approaches a
    /// wavelength.
    CoherentAveragingRejected,
    /// Median σ(f) > 6 dB below the transition, where positions should agree.
    ExcessiveVariance,
    /// Below `positions_default` but at or above 3.
    FewPositions,
    LowSnrHard,
    LowSnrSoft,
    MicNotConnected,
    NoSignal,
    NoiseFloorTooHigh,
    PositionOutlierCouplerHf,
    /// A seal problem, not the headphone.
    PositionOutlierCouplerLf,
    PositionOutlierRoom,
    /// `capture.self_excluded == false`: the tap's fail-open path left our own
    /// audio tapped, so the stimulus topology is unvalidated.
    SelfExclusionUnavailable,
    /// Internal error — this is a bug, not a user condition.
    SweepRateMismatch,
    /// Mic and output on different clocks. Gating needs a trustworthy t=0 and
    /// the "Farina tolerates skew" result does not transfer to it.
    TwoClock,
    TooFewPositions,
    /// We checked our work and it didn't land.
    VerificationResidual,
    /// Solved chain sensitivity outside the declared class's envelope. A
    /// safety event, not a quality event.
    WrongTransducer,
}
