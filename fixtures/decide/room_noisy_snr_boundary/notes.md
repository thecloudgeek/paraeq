# room_noisy_snr_boundary

**What it exercises.** A room that is too noisy to correct confidently and not
too noisy to correct at all — the soft SNR band, deliberately placed inside it
rather than at either edge. A `Bookshelf` run of five positions whose noise
floor is raised 22 dB relative to every other room case in the set (the same
pink-tilted shape with the same 50 Hz mains bump, just louder), with a matching
broadband noise bed added into the captures themselves so the floor the bundle
reports and the floor the captures contain are the same floor. The margin
between the capture and that floor over the correction range therefore sits
below the accept threshold and above the hard refusal, which is the only region
where the soft rule is distinguishable from the hard one.

The same case carries **`capture.clock_skew_ppm: None`**, and the pairing is
physical rather than convenient. The ppm estimate comes from a matched-filter
fit of the marker bracket against the same noise; a floor this high is exactly
where that fit fails to converge, so no estimate was formed and
`clock_adjusted` is false. This is the one bundle in the eight that takes the
`None` branch — every other case carries `Some(ppm)` — so it is the only place
the "fire the two-clock warning ONLY when no estimate was formed" rule is
graded by a golden case rather than by a unit test.

**Expected verdict: `ProceedWithWarnings`.**

**Diagnostics it should raise:** `LowSnrSoft` at `Severity::Warn`, which should
also de-weight rather than refuse; `TwoClock` at `Severity::Warn`, because
`clock_skew_ppm` is `None`; and a position-count warning, because five is below
the room default of nine. `LowSnrHard` and `NoiseFloorTooHigh` must NOT fire —
if either does, the boundary has moved, and that is a policy change to argue for
rather than a fixture to re-cut.
