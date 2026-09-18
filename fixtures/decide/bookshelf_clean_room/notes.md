# bookshelf_clean_room

**What it exercises.** The room happy path, end to end, with nothing wrong.
A `Bookshelf` pair measured through one UMIK-1 at the profile's own default of
nine positions (so `positions_n` echoes `positions_default` and no
position-count diagnostic can fire), `routing: Both` because the system is being
corrected as one, a plain vendor cal with no target baked into it, a noise floor
**95.1 dB** below the analysed capture across the correction range — measured,
not assumed, and quiet enough that no SNR row can fire on rounding — a clean
capture readout at every position, and a two-clock skew estimate that was formed
and applied (`clock_skew_ppm: Some(11.4)`, `clock_adjusted: true`). It is the case every
other room case is read against: FDW gating, variable smoothing, Align SPL over
the alignment band, the power average, σ(f), the room `Parametric` target and a
3.0 dB flatness target all run here on data with no defect in it. It is also the
only case with nine positions, which makes it the one that exercises the full
`positions_domain` and the widest σ(f) ensemble.

**Expected verdict: `Proceed`, with no diagnostics at all.** Written out so
the claim is checkable against the blessed file: `verdict: "Proceed"`,
`diagnostics: []`.

A `Warn` here is a regression in a rule, not in this input — the bundle carries
no condition any row of the refusal table is about. In particular `TwoClock`
must NOT fire: the skew estimate is `Some`, and the rule is "fire only when no
estimate was formed".

**The correction carries ZERO bands, and that is the case working.** Nine
positions of a room with no defect in it analyse to a curve whose residual
against the room target is already inside the 3.0 dB `flatness_target_db`, so
`auto_fit_room` stops before placing a filter — "the greedy loop stops as soon as
residual RMS over the authority band is below `flatness_target_db`". `clamps` and
`dropped` are empty for the same reason. The case that proves the fit DOES place
filters is `in_ear_clean_coupler`; this one proves it declines to when there is
nothing to fix, which is the harder half.

**What to look at in `expected.json`.** Three keys, in this order:
`verdict`; `diagnostics` (each row's `code`, `severity`, `position` and
`value`); and the twenty-one non-`authority` entries under `decisions` — each
one's `value` and `source`. Everything else in the file is curve data: the
`analysis` block and the `evidence` arrays are there to be plotted, and they are
about 99.7 % of the bytes. Reading them is not the review.
