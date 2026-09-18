# bookshelf_clean_room

**What it exercises.** The room happy path, end to end, with nothing wrong.
A `Bookshelf` pair measured through one UMIK-1 at the profile's own default of
nine positions (so `positions_n` echoes `positions_default` and no
position-count diagnostic can fire), `routing: Both` because the system is being
corrected as one, a plain vendor cal with no target baked into it, a noise floor
~50 dB below the capture across the correction range, a clean capture readout at
every position, and a two-clock skew estimate that was formed and applied
(`clock_skew_ppm: Some(11.4)`, `clock_adjusted: true`). It is the case every
other room case is read against: FDW gating, variable smoothing, Align SPL over
the alignment band, the power average, σ(f), the room `Parametric` target and a
3.0 dB flatness target all run here on data with no defect in it. It is also the
only case with nine positions, which makes it the one that exercises the full
`positions_domain` and the widest σ(f) ensemble.

**Expected verdict: `Proceed`.**

**Diagnostics it should raise: none.** A `Warn` here is a regression in a rule,
not in this input — the bundle carries no condition any row of the refusal table
is about. In particular `TwoClock` must NOT fire: the skew estimate is `Some`,
and the rule is "fire only when it is `None`".
