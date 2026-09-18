# over_ear_lost_seal_reseat

**What it exercises.** Two things, and the second one is the reason this
directory is bigger than its neighbours.

**(1) A position-scoped refusal that does not refuse the session.** Five reseats
of an `OverEar` pair; the third (index 2) lost its seal, which on a headphone
means everything below ~200 Hz leaked away. It is applied as a `LowShelf` at
200 Hz, −12 dB, Q 0.7, so the mean absolute deviation over 20–200 Hz is far past
the 6 dB the coupler outlier rule refuses a position at. Four survivors remain,
comfortably above the hard minimum of three, so the set still produces a
correction — refusing the *position* and refusing the *run* are different
answers and this case is where the difference is visible. It is a seal problem,
not the headphone, and the guided-mode copy has to say so.

**(2) The closed-loop verification block, in its final shape.** This is the one
case in the eight that carries a `verification`, and it carries every field:
`capture` (the mic's own MS-21 peak and clip readout, on the far side of the
transducer), `gain_db: 0.0` (the preamp rides inside the correction, never on
the gain stage), `installed` with per-channel `clamps` and a `dropped` band,
`installed_preamp_db`, `ir`, `level_dbfs`, `position_index: 0`, `routing: Both`,
`running_rate_hz` and a `two_clock` fit. Three properties are deliberate:

- **Two capture channels and two correction channels**, with the two band rows
  **identical**. `Both` means the capture heard every output channel, and a sum
  of differently-EQ'd channels is not the response of any one band set; the
  residual is only defined when the rows agree, and this case takes that branch.
- **`installed.design_rate` is 44 100 Hz while `running_rate_hz` is 48 000 Hz.**
  The plan was fitted in an earlier session and re-armed at the current stream
  rate, which is the ordinary case and the one that proves `installed_preamp_db`
  is the engine's own live-rate number rather than a copy of
  `installed.preamp_db`. The two differ, and they differ because of the rate.
- **The capture is constructed to be a PASSING verification.** The sidecar is
  the position-0 baseline filtered through the installed cascade at the live
  rate and scaled by `2 × installed_preamp_db` dB — once for the preamp the
  engine applies inside the correction, once for the MS-19 re-levelling to
  `L_verify = L_measure + preamp_db`. Under the spec's own level compensation
  (`K = L_measure − L_verify = −preamp_db`) the residual of this pass is zero by
  construction. If the blessed `expected.json` disagrees, the disagreement is
  worth reading before it is accepted: either the input's construction or the
  residual recipe is wrong, and this note is the record of which one this
  fixture claims.

**Expected verdict: `ProceedWithWarnings`.**

**Diagnostics it should raise:** `PositionOutlierCouplerLf` at `Severity::Refuse`
scoped to position 2, with the surviving four still producing a correction. No
verification diagnostic: the routing matches the baseline's, the channel counts
agree, the band rows are identical, the capture did not clip, and the residual
is zero by construction. `TwoClock` must not fire — the fit was formed.
