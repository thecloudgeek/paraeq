# over_ear_ears_heq_cal

**What it exercises.** The one cal variant that changes a decision rather than a
curve. A miniDSP EARS jig shipped with an **HEQ** calibration already has the
Harman over-ear target subtracted into it, so the measurement is already
target-relative; choosing a target on top of it applies the target twice and the
user gets a correction with roughly double the intended tilt. The case is an
`OverEar` run, five reseats, two capture channels, a `CalVariant::EarsHeq` cal
whose curve carries the baked-in shape (note the 5 kHz dip and the 20 Hz
shelf — that is the target, not the coupler), and an otherwise unremarkable set
of captures. Everything else about it is the `in_ear_clean_coupler` control:
same window, same gating, same smoothing, same averaging, clean stats
everywhere. The only thing that differs is `cal.variant`, which is what makes
this case a clean read on one rule.

**Expected verdict: `ProceedWithWarnings`.**

**Diagnostics it should raise:** `CalHasTargetBakedIn` at `Severity::Warn`, and
the `target` decision must be **forced** to the `flat` curve — not merely
defaulted to it, and not matched. The rationale key must be the baked-in one
rather than the matched one, because the copy the user reads differs materially:
"your cal already contains a target" is a different sentence from "we matched
your headphone to this curve". `flat` is in the candidate set for exactly this
reason.
