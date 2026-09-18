# room_noisy_snr_boundary

**What it exercises.** A room that is too noisy to correct confidently and not
too noisy to correct at all — the soft SNR band, deliberately placed inside it
rather than at either edge. A `Bookshelf` run of five positions whose noise
floor carries `noise_offset_db = 52.0` — the same pink-tilted shape with the same
50 Hz mains bump as every other room case, 66 dB louder — so that the margin
between the analysed capture and that floor over the correction range sits in the
MIDDLE of the soft SNR band, the only region where the soft rule is
distinguishable from the hard one. The offset is measured onto that window rather
than chosen; the table below gives all three margins.

The captures carry a broadband bed of their own as well, so a noisy room's
captures are not artificially silent. It is deliberately NOT a calibrated match
for the reported floor: the spec's Detection column grades "capture RMS − floor
RMS" against the silence capture the bundle reports, and that spectrum is the
authoritative one.

The same case carries **`capture.clock_skew_ppm: None`**, and the pairing is
physical rather than convenient. The ppm estimate comes from a matched-filter
fit of the marker bracket against the same noise; a floor this high is exactly
where that fit fails to converge, so no estimate was formed and
`clock_adjusted` is false. This is the one bundle in the eight that takes the
`None` branch — every other case carries `Some(ppm)` — so it is the only place
the "fire the two-clock warning ONLY when no estimate was formed" rule is
graded by a golden case rather than by a unit test.

**Expected verdict: `ProceedWithWarnings`, with exactly seven diagnostics, in
this order:** `LowSnrSoft` at `Severity::Warn` once per position (indices 0–4,
values 22.12–22.59 dB), then `FewPositions` at `Severity::Warn` with
`value: 5.0`, then `TwoClock` at `Severity::Warn`.

**The three margins, because all three are what the case is for.** The SNR rows
compare the analysed per-position curve against the silence capture's own
spectrum, both reduced to a band RMS, so every number below is measured on this
bundle rather than assumed:

| quantity | value | gate | margin |
|---|---|---|---|
| in-band SNR | 22.12–22.59 dB | soft, 25 dB | 2.4–2.9 dB under |
| in-band SNR | 22.12–22.59 dB | hard, 15 dB | 7.1–7.6 dB over |
| floor band RMS | −26.11 dBFS | Dirac's, −24 dBFS | 2.11 dB under |

`LowSnrHard` and `NoiseFloorTooHigh` must NOT fire. If either does, the boundary
has moved, and that is a policy change to argue for rather than a fixture to
re-cut. The window between the two is only about 5 dB wide at this capture
level, which is why `noise_offset_db` was MEASURED onto it rather than chosen.

`LowSnrSoft` says the position was used **unweighted**, and means it: the
weighted averaging functions ship with no caller, and § D-P's own interim is
"emit the Warn and do not de-weight, and say so".

`TwoClock` fires because `clock_skew_ppm` is `None` — that is the whole
condition. This bundle captures at 48 000/48 000 like every other, and equal
nominal rates are two crystals and one label, not one clock.

**What to look at in `expected.json`.** Three keys, in this order:
`verdict`; `diagnostics` (each row's `code`, `severity`, `position` and
`value`); and the twenty-one non-`authority` entries under `decisions` — each
one's `value` and `source`. Everything else in the file is curve data: the
`analysis` block and the `evidence` arrays are there to be plotted, and they are
about 99.7 % of the bytes. Reading them is not the review.
