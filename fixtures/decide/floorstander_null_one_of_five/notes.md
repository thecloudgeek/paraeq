# floorstander_null_one_of_five

**What it exercises.** The averaging divergence, which is the one number in the
decision engine that a wrong choice turns into 6 dB of boost aimed at a seat
nobody sits in. Five positions of a `Floorstander` pair; the fifth (index 4)
carries a −30 dB null at 80 Hz, applied as a `Peaking` biquad of Q 2.0 so the
depth is `paraeq-dsp`'s own arithmetic rather than a shape drawn into samples.
The null sits well below the (500, 2000) Hz alignment band, so Align SPL cannot
absorb it and it survives to the spatial average — which is the whole point.
Being a `Floorstander`, the case also pins the one field the two room profiles
differ in: `sweep.f_start_hz = 20.0` rather than the bookshelf's 30.0. Five
positions is below the room path's default of nine and at its enforced floor, so
the position-count warning is part of what this case is for.
`tests/test_arithmetic_pins.rs` asserts the spec's literal (−6.0 dB by dB mean,
−0.97 dB by power) against this directory.

**Expected verdict: `ProceedWithWarnings`.**

**Diagnostics it should raise:** `FewPositions` at `Severity::Warn` — five is
below `positions_default` (9) and at or above the hard minimum of three. The
null itself is NOT a diagnostic: a single deep seat-specific null is what the
power average exists to survive, and refusing it would refuse an ordinary room.
`TwoClock` must not fire (`clock_skew_ppm: Some(9.8)`).
