# room_clipped_position

**What it exercises.** One position railed the microphone's ADC, and the other
four did not. A `Bookshelf` room run of five positions where position index 3
carries a capture readout of 4 096 clipped samples at a −0.006 dBFS peak, while
the rest sit at −8.4 dBFS with nothing clipped. A clipped capture is not a bad
room: the deconvolution of a railed sweep produces harmonic products that read
as early reflections, so the position has to be refused rather than de-weighted,
and the other four still have to produce a correction.

The condition is expressed **both ways on purpose**, because they are two facts
measured on two sides of the transducer and the product has to be able to tell
them apart. `positions[3].capture` is the capture meter's own readout — the
mic's ADC, the far side, MS-21's number. And the position's impulse response is
normalised to −0.006 dBFS while every other IR in the whole fixture set sits at
−6 dBFS, so a sample-domain read of the same event agrees with the meter instead
of contradicting it. Neither of those is the engine's ±1.0 output clamp, which
watches the near side and is a different counter entirely.

**Expected verdict: `ProceedWithWarnings`, with exactly two diagnostics, in
this order:** `ClippingPosition` at `Severity::RefusePosition` scoped to
position 3 with `value: -0.006`, and `FewPositions` at `Severity::Warn` with
`value: 4.0`.

`RefusePosition` is the spec's own third Severity value — "Refuse *that
position*" — so position 3 is REMOVED from the cohort and
`analysis.per_position_db` carries **four** curves. That is what the row's own
copy has always promised ("We dropped it — re-measure just that one"), and the
four survivors are above the hard minimum of three, so the session produces an
installable correction. `FewPositions` grades the SURVIVOR count, four, against
the room default of nine.

`ClippingSession` must NOT fire: one railed position out of five is a bad
position, not a bad session, and collapsing the two would refuse a run that is
perfectly recoverable.

**What to look at in `expected.json`.** Three keys, in this order:
`verdict`; `diagnostics` (each row's `code`, `severity`, `position` and
`value`); and the twenty-one non-`authority` entries under `decisions` — each
one's `value` and `source`. Everything else in the file is curve data: the
`analysis` block and the `evidence` arrays are there to be plotted, and they are
about 99.7 % of the bytes. Reading them is not the review.
