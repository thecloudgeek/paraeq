# in_ear_clean_coupler

**What it exercises.** The coupler happy path, and the structural claim the
whole `PathProfile` table rests on: headphone measurement is the same operation
as room measurement with six values changed. An `InEar` pair on a 711-class
coupler, five reseats (the coupler path's own `positions_default`), **two
capture channels** because the jig hears both ears at once, no gating
(`GatingMode::None` — a coupler has no reflection problem to window away),
`Fixed(6)` smoothing, the dB-domain mean rather than the power average, a
1.0 dB flatness target, and a target candidate set that is class-filtered down
to the in-ear curves plus `flat` and `diffuse_field`. An unfiltered match would
return `harman_oe_2018` here and apply over-ear ear gain to an in-ear
measurement, so the filtered `Choice` domain is part of what this case pins. The
cal is a plain transfer function with no target baked in, which is what makes it
the control for `over_ear_ears_heq_cal`. The impulse responses use the short
coupler window (100 ms before the peak, 250 ms after) rather than the room
store window — honest, because the energy is gone inside ~30 ms, and the
`right_window_ms` the analysis applies is bounded by the data the recording
holds.

**Expected verdict: `Proceed`, with no diagnostics at all** —
`verdict: "Proceed"`, `diagnostics: []`.

Five positions IS the coupler default, so `FewPositions` must not fire here even
though the same count warns on the room path — that asymmetry is a profile fact
and this case is where it shows.

**The correction carries one band per channel, and the count is the point.** This
is the case that caught a fit defect worth naming: the cascade ceiling used to be
checked with a float-equality epsilon, and `COUPLER_EXCURSION_DB` is exactly zero
above 10 kHz, so no realizable filter could ever satisfy it and BOTH coupler
paths emitted nothing at all. One band is what the coupler envelope licenses
here: it allows 2 dB of cut above 500 Hz and nothing above 10 kHz, and this
capture's largest deviation sits just under that cutoff, so a high shelf is what
survives. The eleven `clamps` rows are the candidates the envelope shaped or
refused, which is exactly the reporting the Advanced drawer exists to render.

**What to look at in `expected.json`.** Three keys, in this order:
`verdict`; `diagnostics` (each row's `code`, `severity`, `position` and
`value`); and the twenty-one non-`authority` entries under `decisions` — each
one's `value` and `source`. Everything else in the file is curve data: the
`analysis` block and the `evidence` arrays are there to be plotted, and they are
about 99.7 % of the bytes. Reading them is not the review.
