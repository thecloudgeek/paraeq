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

**Expected verdict: `Proceed`.**

**Diagnostics it should raise: none.** Five positions IS the coupler default, so
`FewPositions` must not fire here even though the same count warns on the room
path — that asymmetry is a profile fact and this case is where it shows.
