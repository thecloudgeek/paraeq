# room_cal_neighbour_outlier

**What it exercises.** The one refusal in the eight, and the only case here whose
defect is in a file the user did not produce. A `Bookshelf` room run of five
positions, everything about the captures clean, with a UMIK-1 calibration file
carrying the shipping `7005770_90deg.txt` defect: an exact `0.0000` at
19.611 Hz sitting between −3.13 and −3.11. It is not hypothetical — it is in a
vendor file — and it lands exactly where correction authority is highest, so a
compensation pass that trusts it applies +3.1 dB of correction to a frequency
that was never wrong. The three rows are reproduced verbatim from the same
literal `crates/paraeq-dsp/tests/test_compensation.rs` uses, on the vendor's
real 0.25 Hz spacing with the two good neighbours either side, so the
single-defect-is-a-single-warning rule is exercised rather than the naive
per-point rule that reports three.

This case is also the reason the refusal path is worth a golden fixture at all:
a `Refuse` must still carry the decisions, the analysis and the evidence that
produced it. `correction` is `None`, and everything else is populated as far as
it got — that is what lets guided mode pick up where auto stopped, and it is
what the owner should check in `expected.json` first.

**Expected verdict: `Refuse`, with exactly two diagnostics, in this order:**
`CalNeighbourOutlier` at `Severity::Refuse` with `value: 3.12`, naming 19.611 Hz
so the drawer can show the margin; and `FewPositions` at `Severity::Warn` with
`value: 5.0`.

**One REFUSAL, one warning, and the distinction is the case.** The refusal has
exactly one cause — the vendor's bogus zero — which is what this directory is
for. `FewPositions` rides along because five positions is below the room path's
default of nine, the same warning every five-position room case here carries; it
is a `Warn`, so it changes no verdict and takes nothing away from the single
cause. The captures, the noise floor, the clock estimate and the sensitivity are
all ordinary on purpose.

`correction` is `None`, and everything else is populated as far as it got — that
is what lets guided mode pick up where auto stopped, and it is what the owner
should check in `expected.json` first.

**What to look at in `expected.json`.** Three keys, in this order:
`verdict`; `diagnostics` (each row's `code`, `severity`, `position` and
`value`); and the twenty-one non-`authority` entries under `decisions` — each
one's `value` and `source`. Everything else in the file is curve data: the
`analysis` block and the `evidence` arrays are there to be plotted, and they are
about 99.7 % of the bytes. Reading them is not the review.
