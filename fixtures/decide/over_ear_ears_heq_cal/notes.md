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

**Expected verdict: `ProceedWithWarnings`, with exactly one diagnostic:**
`CalHasTargetBakedIn` at `Severity::Warn`.

The `target` decision must be **forced** to the `flat` curve — not merely
defaulted to it, and not matched.

**`CalNeighbourOutlier` must NOT fire, and the reason is the whole case.** The
5 kHz dip an HEQ calibration carries IS the Harman target, and on a twelve-point
vendor grid it reads as a 4.7 dB neighbour deviation against a 1.5 dB threshold.
The neighbour-outlier rule cannot tell a target's shape from a bad point — both
are "one value far from its neighbours" — so it exempts the three EARS variants
and `CalHasTargetBakedIn` is what the user is told instead. The exemption is
reversible and marked `OPEN [OWNER]` at the check. The rationale key must be the baked-in one
rather than the matched one, because the copy the user reads differs materially:
"your cal already contains a target" is a different sentence from "we matched
your headphone to this curve". `flat` is in the candidate set for exactly this
reason.

**What to look at in `expected.json`.** Three keys, in this order:
`verdict`; `diagnostics` (each row's `code`, `severity`, `position` and
`value`); and the twenty-one non-`authority` entries under `decisions` — each
one's `value` and `source`. Everything else in the file is curve data: the
`analysis` block and the `evidence` arrays are there to be plotted, and they are
about 99.7 % of the bytes. Reading them is not the review.
