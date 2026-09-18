# Decision-Engine Open Questions — Recommended Resolutions

**Date:** 2026-07-21
**Source:** the eight Open Questions in
`docs/specs/2026-07-15-decision-engine-design.md` §"Open Questions".
**Status:** recommendations, awaiting owner sign-off. Each is either
*pinnable now* (decide and unblock) or *ship-a-starting-value* (a defensible
default that only real measurements can finalize, with the retune signal
stated).

Every data-dependent number here is grounded in the acoustics literature and
in what shipping room-correction products actually do; the load-bearing
figures were independently verified (citations per question). Where a value
can only be a starting point, that is said plainly rather than dressed up as
final.

## Summary

| # | Question | Recommendation | Confidence | Kind |
|---|----------|----------------|-----------|------|
| Q1 | Rate-independence fix (1) or (2) | **Fix (1)** — bands + `design_rate` in the config, re-derive at the live rate; R1-6 refuse-and-fail-open is the safety net | High | Pin now (unblocks R1-6) |
| Q2 | Make `biquad` designers fallible? | **No** — Stage 1's Jury guard + `Q_max` cap already cover it at the one boundary that matters | High | Pin now |
| Q3 | Does the FDW pre/post asymmetry earn its keep? | **Retain, default `n_c_pre = 3.0`, lowest-confidence knob** — REW ships symmetric and is fine; simplify to symmetric if real rooms show no effect | Medium-low | Ship + validate |
| Q4 | EGD flatness threshold | **`1/(4·f_c)` peak-to-peak over a 1/3-oct window** — a 90° excess-phase cap, the defensible middle of `1/(2·f_c)`…`1/(8·f_c)` | Medium | Ship starting value |
| Q5 | σ authority thresholds | **σ_full = 1.0 dB, σ_none = 6.0 dB** — 6.0 sits just above the primary-sourced 5.571 dB diffuse-field asymptote; keep the wide gap | High (anchor); Medium (exact) | Ship starting value |
| Q6 | Two-clock resolution | **Adopt REW's bracketed-timing-marker skew estimate + resample, default on**; `Warn(TwoClock)` fallback | High | Pin now (port, not research) |
| Q7 | Room positions ceiling | **Default 9, ceiling 15, floor 5, hard-min 3** | High | Pin now |
| Q8 | Commercial posture (PLD 2024/2853) | **Stay FOSS / non-commercial to retain the Art. 2(2) exemption** — recommendation only; owner's business call, needed before ship | N/A (business) | Owner decision |

---

## Q1 — Rate-independence: adopt fix (1), keep R1-6 as the net

**Decision.** Take the spec's preferred fix (1): `CorrectionConfig`/
`CorrectionPlan` carry the *design inputs* (`Peq { bands, design_rate }`, and
the FIR arm its target/taps + `design_rate`), and `build_correction` takes the
**live** `sample_rate` and re-derives coefficients on every rebuild. The
invariant lives in the type — it is impossible to install coefficients
designed for a stale rate.

**Reconciliation with engine-hardening R1-6** (this closes the plan's
cross-spec Open Question 1). R1-6's "refuse on `design_rate != stream_rate`,
fail open to flat pass-through" is **not** in conflict with (1); it is the
*last-resort guard*. Under (1), a `Peq`/FIR config re-derives at the live rate,
so there is normally no mismatch to refuse and the correction **survives** an
AirPods 44.1↔48 kHz handoff (a 47 Hz mode filter stays at 47 Hz with the right
Q, instead of landing at 51 Hz). R1-6 then fires only for a config that reaches
`build_correction` and genuinely *cannot* be re-derived at the live rate (a
legacy baked-SOS `Iir { sos_per_channel }`), where fail-open to flat is the
honest outcome. Net user experience: EQ that keeps working across device
switches, with a safe, disclosed fallback for anything un-re-derivable.

**Why not (2).** Re-deriving in the Tauri backend on `FormatChanged` puts the
invariant in a call site that can be missed on a code path, yielding silently
wrong EQ. No user-visible upside; a latent-bug downside.

**Confidence:** High. **Blocks:** Stage 2's R1-6; decide before that lands.

## Q2 — Do not make the `biquad` designers fallible

**Decision.** Leave the four designers returning `[f64; 6]` infallibly. Stage 1
already placed the real protection at the install boundary: `build_iir`
substitutes the identity section for any row failing the strict Jury
`is_stable` check (and counts it), and the `Q_max = 0.227·f_c/A` cap on the
design path prevents the realistic degenerate cases up front. A user can never
get a NaN filter to their ears regardless.

Making the designers fallible would change every golden-fixture call site to
buy a guarantee already enforced more cheaply at the one funnel that matters,
and the designers are oracle-pinned (the oracle itself is infallible). The only
thing it would add is a marginally more precise message when a user hand-types
a degenerate value in the manual-EQ path ("band 3: invalid Q" vs "1 band
dropped") — not worth the churn.

**Confidence:** High. Revisit only if a concrete need appears (e.g. a
manual-EQ UX that wants per-band validation messages).

## Q3 — Retain the FDW pre/post asymmetry, but treat it as the first thing to simplify

**Decision.** Keep the asymmetric capability with the spec's default
`n_c_pre = 3.0`, but mark it the **lowest-confidence knob** in the room DSP and
the first candidate to collapse to symmetric if real rooms show no effect.

**Grounding.** REW — the field-standard tool — implements FDW as a *single
symmetric* value (one "cycles" or octave-fraction) with **no** pre/post-peak
asymmetry, and it is entirely adequate; only Acourate exposes independent
pre/post (and low/high) cycle counts, and REW users have asked Mulcahy for it
without it being added. So ParaEQ's asymmetry is an Acourate-class refinement,
not table stakes. Its stated justification (bounding how much noise floor the
pre-lobe integrates, and making the asymmetry explicit rather than an accident
of the left-window clamp) is cheap insurance, so retain it — but ship symmetric
behavior as the sane default expectation and decide from the first real
measurements whether `n_c_pre ≠ n_c_post` ever changes the correction
audibly.

**Confidence:** Medium-low. **Kind:** ship, validate against real rooms.
([REW impulseresponse.html](https://www.roomeqwizard.com/help/help_en-GB/html/impulseresponse.html))

## Q4 — EGD flatness threshold: ship `1/(4·f_c)`

**Decision.** Veto boosting in any band whose **peak-to-peak excess group
delay** exceeds `1/(4·f_c)` seconds (a quarter of the center-frequency period),
measured over a 1/3-octave window on the fine bass grid, with the EGD trace
computed as REW does it (measured group delay minus the cepstrally-derived
minimum-phase group delay). Concrete ceilings: 30 Hz → 8.3 ms, 50 Hz → 5.0 ms,
80 Hz → 3.1 ms, 100 Hz → 2.5 ms, 160 Hz → 1.6 ms, 200 Hz → 1.25 ms.

**Why this value.** The framing is settled and mainstream: rooms are
*mixed-phase* through the modal region (summing minimum-phase modes does not
preserve minimum phase), so correctability must be judged per-region, and flat
excess group delay is the rigorous per-region test — flat EGD ⇒ minimum-phase ⇒
a parametric filter places a zero on the pole and corrects amplitude *and*
phase; a sharp EGD swing marks a non-minimum-phase null EQ cannot fill. REW
states this verbatim ("we cannot simply say a response is minimum phase below
some specific cutoff"; of a sharp dip, "Attempting to EQ the response to flat in
this region would be foolish"), and no shipping product boosts true nulls
(MSO/miniDSP doctrine: a cancellation stays put "no matter how much EQ is
applied").

The `1/4` multiplier has a clean meaning: `1/(4·f_c) = T/4` = a **90°**
excess-phase excursion, so the worst-case all-pass residual left after a passing
boost is ≤ 90°. At `1/(2·f_c)` (180°) the residual can invert polarity within
the band (EQ actively wrong); at `1/(8·f_c)` it rejects real modal peaks whose
EGD is never perfectly zero, buying nothing. A quarter-period residual is also
comfortably below the Blauert & Laws group-delay audibility thresholds
throughout the 20–200 Hz band. `1/4` is the safe middle; `1/(2·f_c)` and
`1/(8·f_c)` are documented as the loosest/tightest bounds.

**Retune signal.** Loosen toward `1/(2·f_c)` if the gate keeps vetoing bass
peaks that are otherwise obviously correctable (low σ, clean single-pole shape)
and a re-measure after a manual boost shows the multi-position average improved
with no headroom-limiter trip. Tighten toward `1/(8·f_c)` if a passing band,
once boosted, fails to lift the multi-position average, trips the excursion
limiter, or adds audible ringing. Cross-check against σ(f): in an untreated room
the EGD gate and the σ(f) ceiling should flag the same nulls; systematic
disagreement is the signal to retune. Note σ(f) (Q5) is the **primary** shipping
authority; EGD is the deferred refinement, and the two are belt-and-braces.

**Confidence:** Medium (framing high; exact multiplier needs rooms). **Kind:**
ship starting value.
([REW minimumphase.html](https://www.roomeqwizard.com/help/help_en-GB/html/minimumphase.html);
Blauert & Laws, JAES 1978)

## Q5 — σ authority thresholds: ship σ_full = 1.0 dB, σ_none = 6.0 dB

**Decision.** Ship the spec's values: FULL correction authority at inter-position
σ(f) ≤ 1.0 dB, ZERO authority at σ(f) ≥ 6.0 dB, linear between. Keep the wide
~5 dB gap.

**Grounding (verified to primary sources).** σ_none is anchored to the
diffuse-field level standard deviation, **5.571 dB** = `(10/ln10)·√(π²/6)` — a
mean-independent constant that follows because above the Schroeder frequency the
room transfer function's squared magnitude is exponentially distributed
(Schroeder; Kuttruff, *Room Acoustics* eqn 3.34a; Jacobsen & Rodríguez Molares,
JASA 2010, "the relative variance approaches unity"). So 6.0 dB sits just above
the point where inter-position variation is statistically indistinguishable from
a fully diffuse field — where no correction can generalize. σ_full ≈ 1.0 dB
hugs the measured diffuse-field spatial-SPL floor (0.56–0.66 dB in
well-diffused reverberation chambers), i.e. the "positions agree, one correction
works for the whole area" regime. This bracketing is consistent with the
practitioner rule that broadband room EQ should stop around the 300–400 Hz
transition (Toole; the σ throttle reaches σ_none right around there in a typical
room).

**Tuning notes (not required for ship).** (1) Under fractional-octave smoothing
the *observed* per-frequency spatial σ falls below the 5.571 dB pure-tone value
(≈ `5.571/√N_eff`), so σ_none = 6.0 dB is a rarely-reached ceiling rather than
the primary HF governor — fine, but it means the effective HF hand-off is set
more by where σ(f) crosses the throttle midpoint (3.5 dB) than by the endpoint.
(2) Optional cosmetic tightening — σ_none → 5.6 dB to sit exactly on the
asymptote, σ_full → 0.8 dB to hug the floor — changes almost nothing because
σ(f) rises steeply through the transition; not worth doing without data.
(3) **Do not narrow the gap:** with only ~5–9 positions the sample standard
deviation of a high-variance quantity carries ~30–50% uncertainty, so a wide
throttle span is what keeps it robust. Ship the σ(f) curve as on-screen evidence
so it can be retuned from real rooms.

**Retune signal.** If genuinely coherent LF modal features rarely drop below
σ_full = 1.0 dB, correction is being throttled where it should be full → raise
σ_full toward the observed coherent-feature floor. If σ(f) exceeds 6.0 dB
broadly at HF, suspect a pipeline issue (under-smoothing, positions closer than
λ/2, or an SPL-align failure inflating σ) rather than a threshold problem.

**Confidence:** High for the anchor and the ship values; Medium for the exact
endpoints. **Kind:** ship starting value.
([Jacobsen, JASA 2010](https://backend.orbit.dtu.dk/ws/portalfiles/portal/4463680/Jacobsen.pdf);
Kuttruff, *Room Acoustics*)

## Q6 — Two-clock: port REW's method (already resolved 2026-07-19)

**Decision.** Adopt REW's approach: play a timing marker at the **start and end**
of the sweep, count elapsed samples versus expected, compute the clock-rate
difference, and **resample** the capture to correct it — default on, with
`Warn(TwoClock)` as the fallback only when no estimate can be formed. Mulcahy
confirms the mechanism ("REW knows how many samples there should be between the
timing signals … and can resample the data accordingly") and the magnitude
("Only 12 ppm … when output and input are on different devices").

This de-risks what the spec called "the largest unpriced item in the rescope":
it is a known technique with a known ~12 ppm magnitude, not open research. Still
run the measurement-suite/9 experiment to confirm the magnitude on the owner's
own EARS/UMIK rig, and set a "clock adjustment too large" reject bound from that
data (REW warns above its own bound). **Drawer override:** clock-adjust toggle
with the estimated ppm shown, defaulted on. See the plan doc's REW-comparison
section for the full write-up; cross-spec Open Question 4 is re-pointed to this.

**Confidence:** High. **Kind:** pin now (port).
([REW analysis.html](https://www.roomeqwizard.com/help/help_en-GB/html/analysis.html);
[Mulcahy, AVNirvana](https://www.avnirvana.com/threads/propper-use-of-clock-adjustment.7839/))

## Q7 — Positions: default 9, ceiling 15, floor 5, hard-min 3

**Decision.** Ship a default of **9** positions, a ceiling of **15**, an
enforced floor of **5**, and a hard minimum of **3** before any average is
computed. Expose presets: ~5 (quick), 9 (default; single-listener/desktop),
13–15 (multi-seat sofa).

**Grounding (competitor counts verified from vendor docs).** Dirac Live
prescribes exactly 9 / 13 / 17 ("tightly focused / focused / wide"); Audyssey
recommends "six or more … up to eight"; Trinnov ~10; MSO ~5; Toole's stated
minimum is 4–5. Default 9 lands on the knee of the mean-convergence curve
(standard error ∝ 1/√N: 4→9 cuts it ~33%, 9→15 a further ~23%, then it
plateaus) and matches Dirac's single-listener preset. Ceiling 15 sits near the
**half-wavelength decorrelation limit**: discrete points in a real 2–3-seat area
(~1 m × 0.6 m × 0.3 m) cannot hold many more than ~12–16 statistically
independent samples at the frequencies room EQ targets, so beyond ~15 you mostly
add correlated points that barely move the mean or the variance estimate — which
is exactly why continuous MMM (Q3 of the plan's MMM section) exists. Floor 5 and
hard-min 3 respect the anti-overcorrection minimums Toole and Dirac (ART needs
≥3) enforce.

**Bias note.** If ParaEQ ships the per-frequency σ(f) band as a first-class
output that gates correction (it does — Q5), bias the *default* up toward 11–13
in the multi-seat mode, because variance estimation is the count-hungry
statistic (its estimator error only falls as `√(2/(N−1))`). For a converged mean
correction alone, 9 is comfortably sufficient.

**Confidence:** High. **Kind:** pin now.
([Dirac 9/13/17, Bluesound support](https://support1.bluesound.com/hc/en-us/articles/26482099028119-How-to-Perform-Dirac-Live-Calibration-on-Bluesound-Players);
[Audyssey 6–8, Marantz manual](https://manuals.marantz.com/CINEMA50/NA/EN/GFNFSYhguxmdhl.php);
[MMM half-wavelength independence, Ohl](https://www.ohl.to/audio/downloads/MMM-moving-mic-measurement.pdf))

## Q8 — Commercial posture: recommend staying FOSS / non-commercial

**Not a value to pin — a business decision to record before ship.** EU PLD
2024/2853 (transposition due 2026-12-09, inside this release's life) treats
software as a product, covers personal injury (Art. 6(1)(a)), and voids
contractual exclusion (Art. 14) — so the MIT "AS IS" disclaimer is legally
inert against a personal-injury claim. The only shield is Art. 2(2)'s FOSS
carve-out, which survives **only** while ParaEQ is supplied entirely outside
commercial activity (recital 15 forfeits it for software supplied "in exchange
for a price, or for personal data"). Any paid tier or data-monetization forfeits
the exemption and exposes full product liability.

**Recommendation:** keep ParaEQ free and non-commercial. This is also *why* the
safety design (level ladder, driver-excursion caps, hearing-safety refusals) is
as conservative as it is — that conservatism and the FOSS status are the same
shield. A monetized posture would demand product-liability insurance and an even
higher safety/QA bar. **Owner decision, needed before ship, not before code.**

---

## What still needs the owner

- **Sign-off (or amendment) on Q4 and Q5's starting values** — they set how the
  correction sounds; everything else is either mechanical (Q1, Q2, Q7) or
  already argued (Q6). Real EARS/UMIK measurements are the retune input.
- **Q8 is a business call** the recommendation cannot make.
- Q4 and Q5 both depend on the room path, which is gated behind the stage-4
  shell merge and the two-clock experiment — so they are *decisions to bank now*
  and *validate when the room path is measurable*, not blockers today.
