//! The verification gate: **residual vs prediction**, inside [`crate::decide`].
//!
//! `docs/specs/2026-07-15-wizard-design.md` § "The two residuals — and which
//! one is honest": `residual_vs_prediction` is rig-independent, because any
//! static rig error — mic cal error, EARS's per-headphone error, coupler load
//! mismatch, placement — appears in `measured_corrected` and
//! `measured_uncorrected` identically and **cancels in the difference**. What
//! survives is an assertion about ParaEQ: *the filter we designed is the filter
//! your system is actually applying, to within X dB*. It is the product's
//! "single most important refusal".
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**. The level book below is
//! arithmetic with a worked example; the threshold is product policy. There is
//! no prototype verification loop to port.
//!
//! # The level book (normative; do not re-derive it here)
//!
//! Every symbol is dB magnitude at frequency `f`. `S(f)` is the whole physical
//! rig — transducer, acoustic path, mic, cal — unknown, identical in both
//! captures, and it cancels.
//!
//! ```text
//! measured_uncorrected(f)  U = L_measure + S(f)
//! measured_corrected(f)    C = L_verify + G_user + preamp_db + H(f) + S(f)
//! ```
//!
//! ```text
//! step 0  raw curves, SAME treatment, no alignment, unsmoothed:
//!           U(f) from bundle.positions[position_index].ir
//!           C(f) from verification.ir
//! step 1  prediction:  D(f) = H(f) + verification.installed_preamp_db
//!                      H(f) = the REALIZED cascade of installed.bands,
//!                             evaluated at verification.running_rate_hz
//! step 2  level, EXACT: K = bundle.capture.sweep.level_dbfs
//!                           − verification.level_dbfs
//! step 3  residual:    r_c(f) = C_c(f) − [ U_c(f) + D(f) ] + K
//! step 4  smooth ONCE per channel, then RMS over the authority band ⇒ rms_c
//! step 5  gate: max_c rms_c > VERIFICATION_RESIDUAL_MULTIPLE · flatness_target
//!               ⇒ Refuse
//! ```
//!
//! Substituting the model into step 3 with `G_user = 0` (the verify gate pins
//! and reads back `EngineState::gain_db`) and no cross-position alignment:
//!
//! ```text
//! r = [L_verify + preamp + H + S] − [L_measure + S + H + preamp] + (L_measure − L_verify)
//!   = 0
//! ```
//!
//! identically, at every bin, for every `preamp_db`. That invariance is what
//! `residual_is_invariant_to_preamp_db` pins.
//!
//! ## Why `D` includes the preamp, and why `K` is forced
//!
//! **`designed_correction(f) ≜ H(f) + preamp_db`, bands PLUS preamp.** The
//! bands-only reading inverts the test: against a correct engine it leaves a
//! constant residual of `preamp_db` (−6 dB on the worked example) and refuses,
//! while against an engine that *drops* the preamp — the very bug the wizard
//! lists verification as existing to catch, "preamp applied to the export text
//! but not the gain stage" — it leaves zero and proceeds. Under the R1-1 ruling
//! `preamp_lin` is a field **inside** `Correction`, applied on the corrected
//! path only, so it *is* part of the filter the system applies.
//!
//! **`K` is forced, not preferred.** Re-running `align_spl` over the
//! verification pair would absorb any constant — including a real
//! `preamp_lin`-not-applied failure — into the alignment and disable the
//! refusal this module exists for. The exact constant is the only permitted
//! compensation, and the `ResidualMean` evidence is its cross-check, never its
//! substitute. Both terms of `K` are **sweep-span RMS** (see
//! [`crate::bundle::Verification::level_dbfs`]): a whole-file RMS on one side
//! would be 1.46 dB out against a 2.0 dB gate.
//!
//! **The MS-19 attenuation is a safety margin, not an accounting error.**
//! `L_verify = L_measure + preamp_db` makes "the verification sweep is never
//! louder than the baseline at any frequency" hold *unconditionally* — including
//! in the failure case where the chain does not apply the preamp at all. It is
//! deliberately doubled in the level book and must be cancelled **in full**
//! here. The MS-11 cal margin, by contrast, is applied exactly once upstream and
//! rides through the subtraction unchanged.
//!
//! # The channel rule (normative)
//!
//! Every symbol in the five steps except `K` is **per channel**, and nothing in
//! the recipe says which capture channel is predicted by which band set.
//! Inventing that rule wrong on a per-ear coupler produces a meaningless
//! residual **with no symptom**, which is the exact failure class the routing
//! fence exists to prevent.
//!
//! **INDEXING.** Capture channels are the rows of `verification.ir.samples` and
//! of `bundle.positions[p].ir.samples`. Correction channels are the rows of
//! `installed.bands`, whose index is documented as "the ENGINE's channel index
//! (0 = left, 1 = right for stereo)". This rule is written against the INDEX,
//! not the container.
//!
//! **PRECONDITION.** `U` and `C` must have the SAME channel count, else Refuse
//! ([`DiagnosticCode::VerificationRoutingMismatch`]). Differencing captures of
//! different width is not a residual.
//!
//! **THE MAP**, from `verification.routing`, which the capture layer has already
//! forced equal to `positions[position_index].routing`:
//!
//! - `Only(n)` — every capture channel is predicted by `installed.bands[n]`.
//!   Refuse when `n >= installed.bands.channels()`: a routing naming a channel
//!   the plan never corrected cannot be predicted at all.
//! - `Both` — the capture heard the SUM of every output channel. A sum of
//!   differently-EQ'd channels is NOT the response of any one band set, and no
//!   algebra makes it one. When every row of `installed.bands` is EQUAL (the
//!   common case: one correction applied as one, which is what `Both` means on a
//!   room run) `H(f)` is that row's realized response and the residual is well
//!   defined. Otherwise REFUSE.
//!
//! **SCALARS AND VECTORS.** `K` is scalar: both its terms are file-level
//! sweep-span RMS and the helper emits one mono WAV per run. `D(f)` is the SAME
//! for every capture channel under both branches above, by construction of those
//! branches — which is the whole point of refusing the third case rather than
//! modelling it. Steps 0, 3 and 4 run per capture channel, smoothing included:
//! smoothing across channels would average a real per-ear failure into a pass.
//!
//! **THE GATE IS THE WORST CHANNEL, NOT THE MEAN.** `residual_rms_db` is the max
//! over capture channels, and the gate compares that. One bad ear must not be
//! rescued by a good one — the same "worst channel wins" posture the engine
//! already takes for the preamp. The per-channel values are attached
//! individually as [`EvidenceLabel::ResidualVsPrediction`] scalars so the drawer
//! can say WHICH ear failed.
//!
//! # What this module does NOT check
//!
//! - **Self-exclusion.** § D-V: verification **inverts** MS-6 — the tap *must*
//!   see the helper, which is never excluded either way. The baseline's
//!   `self_excluded` witness is the refusal table's row, and if it is false the
//!   whole bundle is already refused there. Copying MS-6 to this side would
//!   refuse every correct verification.
//! - **`bands_dropped`.** The capture layer refuses before spawning when the
//!   engine could not install every band, so no bundle carrying that failure
//!   exists for `decide()` to see. Of the capture layer's new diagnostics,
//!   **only `VerificationPreampMismatch` has a twin here**, precisely because it
//!   is the one failure `decide()` can observe after the fact: this module
//!   re-checks a number the bundle carries rather than trusting it. Without this
//!   paragraph the next reader adds fifteen weaker duplicates.
//! - **`residual_vs_target`.** Computed and attached as evidence, **never**
//!   gated: "Report it, plot it, never gate on it."

use crate::analysis::{realized_preamp_db, realized_response_db};
use crate::bundle::{CaptureRouting, ImpulseResponse, MeasurementBundle, Verification};
use crate::decision::{Evidence, EvidenceLabel, Unit};
use crate::decisions::{Decisions, TargetChoice};
use crate::outcome::{Diagnostic, DiagnosticCode, Severity, VerificationReport};
use paraeq_dsp::authority::{authority_band_mask, AuthorityCurve};
use paraeq_dsp::fr::{complex_spectrum, derotate, smooth};
use paraeq_dsp::gating::{apply_gate, GateSpec, ImpulseResponse as GatedIr, SweepParams};
use paraeq_dsp::logf::{resample_complex_to_log_grid, LogGrid, Prefilter};
use paraeq_dsp::peq::EQBand;
use paraeq_dsp::targets::{build_room_target, RoomTargetSpec};
use paraeq_dsp::window::WindowSpec;

/// The multiple of `flatness_target_db` the worst channel's residual RMS must
/// stay under. `2.0` on every path, read off `decisions.flatness_target_db`
/// **so that an override moves the gate with it** — one named constant, one
/// line to change.
///
/// # `OPEN [OWNER + NEEDS DATA]` — § E1, and the collision is inside one spec
///
/// `2 · flatness_target_db` is **2.0 dB** on the coupler
/// (`flatness_target_db = 1.0`) and **6.0 dB** on the room (`3.0`). The wizard's
/// results screen renders its own example of a **passing** verification as "It's
/// doing what we designed, to within **1.8 dB RMS** across 20–240 Hz" — so the
/// coupler gate clears the spec's own success story by **0.2 dB**, while the
/// room gate is more than 3× looser than the same stated expectation. The
/// multiplier was almost certainly chosen with the *target* residual in mind,
/// where rig error dominates, and does not transfer to the *prediction*
/// residual unexamined.
///
/// Both directions are wrong invisibly until real hardware runs: too tight and
/// auto mode becomes a false-refuse machine; too loose and "the single most
/// important refusal in the product" never fires. **The retune signal is
/// stated: the first EARS run where a correction that A/Bs correctly still
/// refuses.** See `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md`
/// § D-G and § E1.
pub const VERIFICATION_RESIDUAL_MULTIPLE: f64 = 2.0;

/// How far the bundle's carried `installed_preamp_db` may sit from `decide()`'s
/// own recomputation over the same bands at the same rate before the pass is
/// refused.
///
/// Two independent computations of one number, in two crates, is the point —
/// and they are the *same call* on the *same inputs*, so the tolerance is a
/// float-equality guard, not a modelling allowance. It is satisfiable precisely
/// because the capture layer already refused a pass whose engine dropped a band:
/// with `bands_dropped == 0` the engine's live-rate fold and this one see the
/// same cascade.
const PREAMP_MATCH_TOLERANCE_DB: f64 = 1e-6;

/// A verification capture whose peak reached this is railed, and a residual
/// computed over a railed microphone is meaningless.
///
/// MIRRORS the refusal table's own position-clip gate (`refusal.rs`'s
/// `CLIP_POSITION_DBFS`), which is private to that module. One physical event,
/// one threshold: a value here that drifted from that one would grade the
/// verification capture by a different rule than the baseline it is differenced
/// against.
const CLIP_PEAK_DBFS: f64 = -0.3;

/// The additive magnitude floor, matching `biquad::sos_frequency_response_db`'s
/// `+1e-10` and `analysis.rs`'s own constant, so a bin that came out exactly
/// zero maps to a finite −200 dB rather than to `-inf` — which `fr::smooth`
/// refuses and `serde_json` cannot round-trip.
const MAGNITUDE_FLOOR: f64 = 1e-10;

/// The anti-comb prefilter the log resample runs at, the same REW-finest
/// setting `analysis.rs` uses. Mirrored rather than shared because `analysis`
/// keeps it private and this module is deliberately NOT the analysis pipeline —
/// see [`curve_db`].
const RESAMPLE_PREFILTER: Prefilter = Prefilter::AntiComb { fraction: 48 };

/// What the gate produced: the rows it adds to `DecisionSet::diagnostics`, and
/// the report that hangs off `DecisionSet::verification`.
pub(crate) struct VerificationOutcome {
    /// Appended to the refusal table's vector, never interleaved with it: B7c's
    /// order is the frozen shape `fixtures/decide/<case>/expected.json` compares
    /// as a list.
    pub diagnostics: Vec<Diagnostic>,
    pub report: VerificationReport,
}

/// Grade the bundle's verification pass, or answer `None` when it carries none.
///
/// `None` is the only reason this returns nothing: `DecisionSet::verification`
/// is documented "Present iff the bundle carried a `verification`", which is
/// what keeps every golden case that carries no verification byte-stable.
///
/// **A verification block only ever ADDS refusals.** Nothing here can turn a
/// `Refuse` into a `Proceed` or remove a row the refusal table earned; the
/// verdict is still the worst severity over the whole list.
pub(crate) fn verify(
    bundle: &MeasurementBundle,
    decisions: &Decisions,
    authority: &AuthorityCurve,
    grid: &LogGrid,
) -> Option<VerificationOutcome> {
    let verification = bundle.verification.as_ref()?;
    let gate_db = VERIFICATION_RESIDUAL_MULTIPLE * decisions.flatness_target_db.value;

    let mut diagnostics = Vec::new();
    let mut evidence = two_clock_evidence(verification);
    evidence.push(Evidence::Scalar {
        label: EvidenceLabel::SectionsSubstituted,
        unit: Unit::Count,
        // `decide()`'s own count, over the same cascade the prediction is taken
        // from, so the two cannot disagree. Evidence, never a gate: the
        // realized-cascade prediction already models the substitution.
        value: sections_substituted(verification) as f64,
    });

    // The checks run in the order they MUST: structure first (is there a
    // residual to compute at all?), then the numbers the bundle carries, then
    // the capture itself, then the residual. The emitted order is this order,
    // and it is fixed for the same reason the refusal table's is.
    let prediction = match predict(bundle, verification) {
        Ok(prediction) => prediction,
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            // No residual exists — the refusal above says why. `evidence`
            // carries no `ResidualVsPrediction`, which is the witness that
            // nothing was computed; `residual_rms_db` is not a measurement here.
            return Some(VerificationOutcome {
                diagnostics,
                report: VerificationReport {
                    evidence,
                    gate_db,
                    residual_rms_db: None,
                },
            });
        }
    };

    if let Some(diagnostic) = preamp_mismatch(verification, &prediction) {
        diagnostics.push(diagnostic);
    }
    // Two conditions leave the residual COMPUTABLE but MEANINGLESS: a capture
    // taken with the user's trim still applied is off by exactly that trim, and
    // a railed capture is off by whatever the ADC did. Both already refuse, with
    // a sentence that names the real cause — so the residual row is suppressed
    // rather than added beside them. R23 requires this in as many words for the
    // railed case: report the clipping, "**not** `VerificationResidual`", or the
    // user is sent to fix the correction for a microphone problem. The residual
    // is still computed and still attached as evidence, because "the level is
    // off by 6.0 dB" is exactly the sentence that confirms the diagnosis.
    let mut gradeable = true;
    for diagnostic in [
        level_book_violated(bundle, verification),
        railed_capture(verification),
    ]
    .into_iter()
    .flatten()
    {
        gradeable = false;
        diagnostics.push(diagnostic);
    }

    let residual = residual(
        bundle,
        verification,
        decisions,
        authority,
        grid,
        &prediction,
    );
    let Some(residual) = residual else {
        diagnostics.push(Diagnostic {
            code: DiagnosticCode::VerificationResidual,
            position: Some(verification.position_index),
            remedy: "We played the check sweep but could not analyse what came \
                     back, so we cannot confirm the correction landed. Re-run \
                     the measurement."
                .to_string(),
            severity: Severity::Refuse,
            value: None,
        });
        return Some(VerificationOutcome {
            diagnostics,
            report: VerificationReport {
                evidence,
                gate_db,
                residual_rms_db: None,
            },
        });
    };

    evidence.extend(residual.evidence);
    if let Some(vs_target) = residual_vs_target(bundle, decisions, grid, &residual.corrected_db) {
        evidence.push(Evidence::Scalar {
            label: EvidenceLabel::ResidualVsTarget,
            unit: Unit::Db,
            value: vs_target,
        });
    }

    // **Nothing was graded** — the authority band kept no bin on any capture
    // channel, so the correction claims nothing inside the corrected range that
    // this pass could check. Ruling R-A12 makes that a REFUSAL rather than the
    // silent pass it was: `worst` stayed at its `0.0` initializer, the gate
    // compared `0.0 > gate_db` and answered false, and a verification that
    // checked nothing reported "0.0 dB RMS, within the limit". A band with no
    // authority in it is a reason to refuse to confirm, not a reason to confirm.
    //
    // `VerificationResidual` with its own remedy rather than a new variant: the
    // code space is append-only and frozen, and this IS a residual failure —
    // one whose cause we happen to know, so the sentence names it. Same posture
    // as the two level-book rows.
    let Some(rms_db) = residual.rms_db else {
        diagnostics.push(Diagnostic {
            code: DiagnosticCode::VerificationResidual,
            position: Some(verification.position_index),
            remedy: "The correction does not claim anything inside the range we \
                     can check, so the check cannot confirm it landed. Nothing \
                     has been changed. Re-run the measurement."
                .to_string(),
            severity: Severity::Refuse,
            value: None,
        });
        return Some(VerificationOutcome {
            diagnostics,
            report: VerificationReport {
                evidence,
                gate_db,
                residual_rms_db: None,
            },
        });
    };

    // The gate is the WORST channel, never the mean: one bad ear must not be
    // rescued by a good one.
    if gradeable && rms_db > gate_db {
        diagnostics.push(Diagnostic {
            code: DiagnosticCode::VerificationResidual,
            position: Some(verification.position_index),
            remedy: format!(
                "We checked our work and it didn't land: the corrected \
                 measurement is {rms_db:.1} dB RMS away from what we designed, \
                 against a limit of {gate_db:.1} dB. Nothing has been changed. \
                 Try the guided path, which shows the residual curve."
            ),
            severity: Severity::Refuse,
            value: Some(rms_db),
        });
    }

    Some(VerificationOutcome {
        diagnostics,
        report: VerificationReport {
            evidence,
            gate_db,
            residual_rms_db: Some(rms_db),
        },
    })
}

// ---------------------------------------------------------------------------
// Step 1 — the prediction, and the structural preconditions it needs
// ---------------------------------------------------------------------------

/// `D(f)` and the facts the residual needs about the shape of the pass.
struct Prediction {
    /// The one band set every capture channel is predicted by, per the channel
    /// rule. Held so the preamp re-check and `D` cannot read different bands.
    bands: Vec<EQBand>,
    /// How many capture channels both sides carry — they agree, or there is no
    /// prediction.
    channels: usize,
    /// `min` over the plan's channels of the realized preamp at the running
    /// rate: the identical fold the engine makes ("**The worst channel wins**").
    recomputed_preamp_db: f64,
}

/// Resolve the channel rule and build `D(f)`, or answer the routing refusal
/// that says why no residual exists.
fn predict(
    bundle: &MeasurementBundle,
    verification: &Verification,
) -> Result<Prediction, Diagnostic> {
    let routing_refusal = |reason: &str| Diagnostic {
        code: DiagnosticCode::VerificationRoutingMismatch,
        position: Some(verification.position_index),
        remedy: format!(
            "We could not compare the check sweep against the original \
             measurement: {reason}. Nothing has been changed. Re-run the \
             measurement."
        ),
        severity: Severity::Refuse,
        value: None,
    };

    // Trigger 1, and the degenerate reading of it: a pass naming a position the
    // bundle does not contain has no baseline to be differenced against at all.
    let Some(position) = bundle.positions.get(verification.position_index) else {
        return Err(routing_refusal(
            "the check names a measurement position that is not in this session",
        ));
    };
    if position.routing != verification.routing {
        return Err(routing_refusal(
            "the check played to different speakers than the measurement did",
        ));
    }

    // Trigger 2 — the channel-count precondition. Differencing captures of
    // different width is not a residual.
    let channels = position.ir.samples.len();
    if channels == 0 || channels != verification.ir.samples.len() {
        return Err(routing_refusal(
            "the check and the measurement recorded different numbers of channels",
        ));
    }

    let plan = verification.installed.bands.as_slice();
    let bands = match verification.routing {
        // Trigger 3 — a routing naming a channel the plan never corrected
        // cannot be predicted at all.
        CaptureRouting::Only(n) => match plan.get(n as usize) {
            Some(bands) => bands.clone(),
            None => {
                return Err(routing_refusal(
                    "the check played to a channel the correction does not cover",
                ))
            }
        },
        // Trigger 4 — the capture heard the SUM of every output channel. A sum
        // of differently-EQ'd channels is not the response of any one band set,
        // and no algebra makes it one: refused rather than modelled.
        CaptureRouting::Both => {
            let first = plan
                .first()
                .expect("PerChannel is non-empty by construction");
            if plan.iter().any(|channel| channel != first) {
                return Err(routing_refusal(
                    "the check heard both speakers at once while they carry \
                     different corrections",
                ));
            }
            first.clone()
        }
    };

    let rate = verification.running_rate_hz;
    if !rate.is_finite() || rate <= 0.0 {
        return Err(routing_refusal(
            "the check did not record the sample rate it ran at",
        ));
    }

    let recomputed_preamp_db = plan
        .iter()
        .map(|channel| realized_preamp_db(channel, rate))
        .fold(f64::INFINITY, f64::min);

    Ok(Prediction {
        bands,
        channels,
        recomputed_preamp_db,
    })
}

/// Two independent computations of one number, in two crates.
///
/// The bundle carries what the **engine** armed; this re-derives it from the
/// plan's own bands at the running rate. They agree when the engine installed
/// the plan; when they do not, refusing is the only honest answer, because the
/// prediction would otherwise be built on a preamp nothing confirmed.
///
/// **One condition, two crates, two codes with the same name.**
/// `MeasurementDiagnostic::VerificationPreampMismatch` is what the capture layer
/// refuses with *before* the capture, at the verify gate. This is the
/// after-the-fact half, over a bundle that already exists.
fn preamp_mismatch(verification: &Verification, prediction: &Prediction) -> Option<Diagnostic> {
    let carried = verification.installed_preamp_db;
    let recomputed = prediction.recomputed_preamp_db;
    if !carried.is_finite()
        || !recomputed.is_finite()
        || (carried - recomputed).abs() > PREAMP_MATCH_TOLERANCE_DB
    {
        return Some(Diagnostic {
            code: DiagnosticCode::VerificationPreampMismatch,
            position: Some(verification.position_index),
            remedy: format!(
                "The correction that was running reported {carried:.2} dB of \
                 headroom, but its own filters need {recomputed:.2} dB at \
                 {:.0} Hz. We will not grade a correction we cannot reproduce. \
                 Nothing has been changed.",
                verification.running_rate_hz
            ),
            severity: Severity::Refuse,
            value: Some(carried - recomputed),
        });
    }
    None
}

/// The two facts the pass carries so `decide()` can **refuse rather than
/// trust**: the gain stage was pinned, and the verification sweep was not
/// louder than the baseline.
///
/// `G_user` (`EngineState::gain_db`) is applied on BOTH chain paths, so a pass
/// captured with a non-zero trim enters the residual as a constant `+G_user` —
/// a 10 dB trim is a 10 dB residual and a refusal with the wrong explanation.
/// The verify gate pins it to `0.0`, reads it back and RAII-restores it; this
/// is the after-the-fact half of that same check. `L_verify > L_measure` is the
/// MS-19 safety invariant read backwards: `preamp_db <= 0`, so a verification
/// sweep louder than the baseline means the level was not solved by MS-19 at
/// all and the residual would be compensating a constant nobody armed.
///
/// Both reuse [`DiagnosticCode::VerificationResidual`] with their own remedy
/// rather than minting variants: the code space is append-only and frozen, and
/// each of these IS a residual failure — one whose cause we happen to know, so
/// the sentence names it instead of sending someone re-measuring.
fn level_book_violated(
    bundle: &MeasurementBundle,
    verification: &Verification,
) -> Option<Diagnostic> {
    let refusal = |remedy: String, value: f64| {
        Some(Diagnostic {
            code: DiagnosticCode::VerificationResidual,
            position: Some(verification.position_index),
            remedy,
            severity: Severity::Refuse,
            value: Some(value),
        })
    };
    let gain_db = verification.gain_db;
    if !gain_db.is_finite() || gain_db != 0.0 {
        return refusal(
            format!(
                "The check ran with your volume trim still at {gain_db:+.1} dB, \
                 so we cannot tell the trim apart from the correction. Nothing \
                 has been changed. Re-run the measurement."
            ),
            gain_db,
        );
    }
    let level_dbfs = verification.level_dbfs;
    let baseline_dbfs = bundle.capture.sweep.level_dbfs;
    if !level_dbfs.is_finite() || level_dbfs > baseline_dbfs {
        return refusal(
            format!(
                "The check sweep played at {level_dbfs:.1} dBFS, above the \
                 {baseline_dbfs:.1} dBFS the measurement used. A check is never \
                 louder than the measurement it checks. Nothing has been \
                 changed. Re-run the measurement."
            ),
            level_dbfs - baseline_dbfs,
        );
    }
    None
}

/// MS-21's capture-side half, on the far side of the transducer.
///
/// A railed verification capture produces a garbage `measured_corrected`, a
/// garbage residual and **no symptom**: without this row `decide()` would refuse
/// with `VerificationResidual` and blame the correction for a clipped
/// microphone. It reuses `ClippingPosition` — the same physical event the
/// refusal table grades every baseline position on, seen on the verification
/// pass — rather than minting a variant: the code space is frozen, and
/// `VerificationResidual` is exactly the wrong sentence for it.
///
/// NOT the same fact as the capture layer's `VerificationChainClipped`, which
/// reads the engine's own ±1.0 output clamp on the NEAR side. Both can fire and
/// neither implies the other.
fn railed_capture(verification: &Verification) -> Option<Diagnostic> {
    let peak = verification.capture.peak_dbfs;
    if !peak.is_finite() || peak < CLIP_PEAK_DBFS {
        return None;
    }
    Some(Diagnostic {
        code: DiagnosticCode::ClippingPosition,
        position: Some(verification.position_index),
        remedy: format!(
            "The microphone was overloaded during the check sweep (peak \
             {peak:.1} dBFS). That makes the check meaningless, not the \
             correction wrong. Lower the microphone's input gain and re-run \
             the measurement."
        ),
        severity: Severity::Refuse,
        value: Some(peak),
    })
}

// ---------------------------------------------------------------------------
// Steps 0, 2, 3, 4 — the residual
// ---------------------------------------------------------------------------

/// The graded number, the per-channel evidence behind it, and the corrected
/// curve the never-gated target residual is read off.
struct Residual {
    corrected_db: Vec<f64>,
    evidence: Vec<Evidence>,
    /// The **max** over capture channels, or `None` when no channel was graded
    /// at all — an authority band that keeps no bin (ruling R-A12). `Some` iff
    /// [`Self::evidence`] carries at least one `ResidualVsPrediction`, so the
    /// number and the curve behind it appear and disappear together.
    rms_db: Option<f64>,
}

/// Steps 0–4, per capture channel.
///
/// `None` when the pass cannot be turned into curves at all — a peak outside its
/// own samples, a non-finite sample, a gate the recording cannot support. The
/// caller refuses; fabricating a flat curve would be the "second, lying source
/// of truth about what the app did" this design exists to prevent.
fn residual(
    bundle: &MeasurementBundle,
    verification: &Verification,
    decisions: &Decisions,
    authority: &AuthorityCurve,
    grid: &LogGrid,
    prediction: &Prediction,
) -> Option<Residual> {
    let position = bundle.positions.get(verification.position_index)?;
    let baseline = &position.ir;
    let corrected = &verification.ir;

    // Step 2, EXACT and never re-aligned. Both terms are sweep-span RMS.
    let k_db = bundle.capture.sweep.level_dbfs - verification.level_dbfs;
    if !k_db.is_finite() {
        return None;
    }

    // ONE window for both curves — "through the *same* function". The decided
    // right window, bounded by whichever recording holds less post-peak data, so
    // `apply_gate` never refuses and neither side is gated longer than the other.
    let right_ms = decisions
        .right_window_ms
        .value
        .min(post_peak_ms(baseline))
        .min(post_peak_ms(corrected));

    // Step 1. `H(f)` is the REALIZED cascade at the RUNNING rate — not
    // `frequency_response`, and never at `installed.design_rate`.
    let designed_correction_db: Vec<f64> = realized_response_db(
        &prediction.bands,
        grid.freqs(),
        verification.running_rate_hz,
    )
    .into_iter()
    .map(|h| h + verification.installed_preamp_db)
    .collect();
    if designed_correction_db.iter().any(|d| !d.is_finite()) {
        return None;
    }

    let mask = authority_band_mask(grid.freqs(), authority, decisions.correction_range.value);

    let mut evidence = Vec::new();
    let mut worst: Option<f64> = None;
    let mut corrected_db_first = Vec::new();
    for channel in 0..prediction.channels {
        let uncorrected = curve_db(baseline, channel, bundle, decisions, grid, right_ms)?;
        let corrected_curve = curve_db(corrected, channel, bundle, decisions, grid, right_ms)?;
        if channel == 0 {
            corrected_db_first.clone_from(&corrected_curve);
        }

        // Step 3, on the raw grid.
        let raw: Vec<f64> = corrected_curve
            .iter()
            .zip(&uncorrected)
            .zip(&designed_correction_db)
            .map(|((c, u), d)| c - (u + d) + k_db)
            .collect();
        // Step 4 — smooth ONCE, per channel, with the smoothing the analysis
        // path used. `fr::smooth` is a dB-domain, unit-gain-on-constants linear
        // operator, so the level constants pass through it unchanged and the
        // ordering is free; subtracting first is the order that cannot be got
        // wrong. Smoothing across channels would average a real per-ear failure
        // into a pass.
        let smoothed = smooth(&raw, grid, decisions.smoothing.value.into()).ok()?;

        let Some(rms) = masked_rms(&smoothed, &mask) else {
            // The mask keeps no bin: nothing in the correction range carries
            // authority, so there is nothing the correction claims and nothing
            // this channel can be graded on. `worst` stays `None`, and the
            // CALLER turns that into a refusal (ruling R-A12) rather than into a
            // pass with a residual of zero.
            continue;
        };
        let mean = masked_mean(&smoothed, &mask).unwrap_or(0.0);
        let scatter = masked_rms(
            &smoothed.iter().map(|r| r - mean).collect::<Vec<f64>>(),
            &mask,
        )
        .unwrap_or(0.0);

        // Attached once PER CAPTURE CHANNEL, in channel order, so the drawer can
        // say WHICH ear failed. The mean and the scatter ride alongside because
        // every mis-accounting in the level book produces a CONSTANT residual:
        // "the level is off by 6.0 dB" names the bug, "the residual is 6.0 dB"
        // sends someone re-measuring.
        evidence.push(Evidence::Scalar {
            label: EvidenceLabel::ResidualVsPrediction,
            unit: Unit::Db,
            value: rms,
        });
        evidence.push(Evidence::Scalar {
            label: EvidenceLabel::ResidualMean,
            unit: Unit::Db,
            value: mean,
        });
        evidence.push(Evidence::Scalar {
            label: EvidenceLabel::ResidualScatter,
            unit: Unit::Db,
            value: scatter,
        });
        worst = Some(worst.map_or(rms, |w: f64| w.max(rms)));
    }

    Some(Residual {
        corrected_db: corrected_db_first,
        evidence,
        rms_db: worst,
    })
}

/// Step 0 — one capture channel's raw dB curve on the analysis grid.
///
/// This is `analysis::curve_on_grid` minus **three** stages, and each omission
/// is load-bearing rather than an economy:
///
/// - **No `align_spl`.** The verification capture is not a member of the
///   position cohort and has had no cross-position offset removed, so aligning
///   one side would leave that offset in the residual — and re-aligning the pair
///   would absorb a real `preamp_lin`-not-applied failure into the alignment.
/// - **No smoothing.** Applied ONCE, to the residual, in [`residual`].
/// - **No FDW.** Frequency-dependent windowing is a smoothing of the COMPLEX
///   spectrum, so it does not commute with the multiplication by `H` that the
///   prediction models: applying it would smear the two sides differently and
///   manufacture a residual. It is a resolution control for decisions, and there
///   are no decisions here.
///
/// The cal compensation is omitted for a fourth reason: it is additive and
/// identical on both sides, so it cancels exactly in `C − U`. Applying it would
/// change no digit of the residual.
fn curve_db(
    ir: &ImpulseResponse,
    channel: usize,
    bundle: &MeasurementBundle,
    decisions: &Decisions,
    grid: &LogGrid,
    right_ms: f64,
) -> Option<Vec<f64>> {
    let samples = ir.samples.get(channel)?;
    if ir.peak >= samples.len() || samples.iter().any(|s| !s.is_finite()) {
        return None;
    }
    let sweep = &bundle.capture.sweep;
    let (gated, report) = apply_gate(
        &GatedIr {
            peak: ir.peak as f64,
            sample_rate: ir.sample_rate,
            samples: samples.clone(),
        },
        &GateSpec {
            left_ms: decisions.left_window_ms.value,
            right_ms,
            sweep: Some(SweepParams {
                duration_s: sweep.duration_s,
                f1: sweep.f_start_hz,
                f2: sweep.f_end_hz,
            }),
            window: WindowSpec {
                left: crate::analysis::window_kind(decisions.window_type.value),
                right: crate::analysis::window_kind(decisions.window_type.value),
            },
        },
    )
    .ok()?;

    let n_fft = gated.len().next_power_of_two();
    let (freqs_linear, spectrum) = complex_spectrum(&gated, ir.sample_rate, Some(n_fft)).ok()?;
    // MANDATORY before the log resample: at 20 kHz a 50 ms delay winds ~1000
    // full turns and no log grid could sample it. The bulk delay is where
    // `apply_gate` put the peak in its own output.
    let derotated = derotate(&spectrum, &freqs_linear, report.applied_left_ms / 1000.0);
    let resampled =
        resample_complex_to_log_grid(&freqs_linear, &derotated, grid, RESAMPLE_PREFILTER).ok()?;
    let curve: Vec<f64> = resampled
        .iter()
        .map(|c| 20.0 * (c.norm() + MAGNITUDE_FLOOR).log10())
        .collect();
    curve.iter().all(|v| v.is_finite()).then_some(curve)
}

/// Post-peak data the recording holds, in milliseconds.
fn post_peak_ms(ir: &ImpulseResponse) -> f64 {
    let rate = f64::from(ir.sample_rate.max(1));
    (ir.samples
        .first()
        .map_or(0, Vec::len)
        .saturating_sub(ir.peak + 1)) as f64
        / rate
        * 1000.0
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// The marker-fit provenance, when the pass carried one. **Evidence only, never
/// gated.**
///
/// Two of `TwoClockFit`'s four numbers have labels: the RMS residual and the
/// skew. `intercept_samples` and `residual_peak_samples` have none and are not
/// invented here — a label is serde-visible and belongs in the pre-freeze
/// window, not in a gate.
fn two_clock_evidence(verification: &Verification) -> Vec<Evidence> {
    let Some(fit) = verification.two_clock else {
        return Vec::new();
    };
    vec![
        Evidence::Scalar {
            label: EvidenceLabel::TwoClockResidual,
            unit: Unit::Samples,
            value: fit.residual_rms_samples,
        },
        Evidence::Scalar {
            label: EvidenceLabel::TwoClockSkewPpm,
            unit: Unit::PartsPerMillion,
            value: fit.skew_ppm,
        },
    ]
}

/// How many biquad rows this prediction evaluated as identity because they fail
/// the Jury stability test at the running rate.
///
/// `decide()`'s own count over the same cascade `H(f)` is taken from, so the two
/// cannot disagree. Evidence, never a gate: the realized-cascade prediction
/// already models the substitution, which is exactly why a nonzero value here is
/// visible rather than silent.
fn sections_substituted(verification: &Verification) -> usize {
    let rate = verification.running_rate_hz;
    if !rate.is_finite() || rate <= 0.0 {
        return 0;
    }
    verification
        .installed
        .bands
        .as_slice()
        .iter()
        .flatten()
        .filter(|band| !paraeq_dsp::biquad::is_stable(&band.to_sos(rate)))
        .count()
}

/// `residual_vs_target` — **reported, plotted, never gated.**
///
/// Rig-dependent by construction: on a coupler with `flatness_target_db = 1.0`
/// it would refuse a correct correction whenever the rig's own error exceeds the
/// gate, which EARS routinely does. It is level-matched over the same band
/// before the RMS, because the corrected curve is in dBFS and a target is a
/// relative shape — an unmatched difference would report the playback level.
///
/// The target is resolved the same way the fit resolved it; `None` when the
/// named curve is not in the bundle or the parametric spec is refused, in which
/// case nothing is attached rather than a number read off a curve nobody named.
fn residual_vs_target(
    bundle: &MeasurementBundle,
    decisions: &Decisions,
    grid: &LogGrid,
    corrected_db: &[f64],
) -> Option<f64> {
    let target = match &decisions.target.value {
        TargetChoice::Curve { name } => bundle
            .targets
            .iter()
            .find(|t| &t.name == name)
            .map(|t| t.interpolate(grid.freqs()))?,
        TargetChoice::Parametric {
            shelf_db,
            shelf_fc,
            shelf_q,
            tilt_db_per_oct,
        } => {
            build_room_target(
                &RoomTargetSpec {
                    pivot_hz: RoomTargetSpec::default().pivot_hz,
                    shelf_gain_db: *shelf_db,
                    shelf_hz: *shelf_fc,
                    shelf_q: *shelf_q,
                    tilt_db_per_oct: *tilt_db_per_oct,
                },
                grid.freqs(),
            )
            .ok()?
            .gains_db
        }
    };
    if corrected_db.len() != target.len() {
        return None;
    }
    let difference: Vec<f64> = corrected_db
        .iter()
        .zip(&target)
        .map(|(c, t)| c - t)
        .collect();
    let mean = difference.iter().sum::<f64>() / difference.len() as f64;
    let matched: Vec<f64> = difference.iter().map(|d| d - mean).collect();
    Some((matched.iter().map(|d| d * d).sum::<f64>() / matched.len() as f64).sqrt())
}

// ---------------------------------------------------------------------------
// Band statistics
// ---------------------------------------------------------------------------

/// RMS over the bins the mask keeps. `None` when it keeps none.
fn masked_rms(values: &[f64], mask: &[bool]) -> Option<f64> {
    let (sum, count) = values
        .iter()
        .zip(mask)
        .filter(|(_, &keep)| keep)
        .fold((0.0, 0usize), |(sum, count), (v, _)| {
            (sum + v * v, count + 1)
        });
    (count > 0).then(|| (sum / count as f64).sqrt())
}

/// Mean over the bins the mask keeps. `None` when it keeps none.
fn masked_mean(values: &[f64], mask: &[bool]) -> Option<f64> {
    let (sum, count) = values
        .iter()
        .zip(mask)
        .filter(|(_, &keep)| keep)
        .fold((0.0, 0usize), |(sum, count), (v, _)| (sum + v, count + 1));
    (count > 0).then(|| sum / count as f64)
}
