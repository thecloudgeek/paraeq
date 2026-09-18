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

**Expected verdict: `Refuse`.**

**Diagnostics it should raise:** `CalNeighbourOutlier` at `Severity::Refuse`,
naming 19.611 Hz and carrying the deviation as `Diagnostic::value` so the drawer
can show the margin. Nothing else: the captures, the noise floor, the position
count, the clock estimate and the sensitivity are all ordinary here, on purpose,
so that the refusal has exactly one cause.
