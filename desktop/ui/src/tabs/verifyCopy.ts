// Pure copy helpers for the Verify panel, split out so they can be unit tested
// without rendering the component (the pattern statusStrip.ts and
// wizard/probePanel.ts already use). Named `verifyCopy` rather than
// `verifyPanel` because macOS's filesystem is case-insensitive and
// `VerifyPanel.tsx` sits beside it.
//
// EVERY REMEDY STRING HERE COMES FROM RUST. The decision engine authors the
// plain-language fix for each diagnostic and the measurement crate authors the
// one for each refusal; this file renders them and never writes one. A second
// remedy vocabulary in TypeScript is a second place for the wrong advice to be
// given, and the wrong advice about a measurement is advice that sends someone
// back to a rig for an hour.

import type { VerifyDiagnostic, VerifyReport, VerifyState } from "@/ipc/types";

/** The one-line headline for each phase. */
export function verifyHeadline(state: VerifyState): string {
  switch (state.phase) {
    case "armed":
      return "Ready to verify — confirm the level before the sweep plays.";
    case "complete":
      return verdictHeadline(state.report);
    case "failed":
      return `Verification stopped: ${state.summary}`;
    case "idle":
      return "No verification has run yet.";
    case "running":
      return "Playing the verification sweep, then analysing the capture…";
  }
}

function verdictHeadline(report: VerifyReport): string {
  switch (report.verdict) {
    case "proceed":
      return "Verified — the correction is doing what we designed.";
    case "proceed_with_warnings":
      return "Verified, with notes — the correction is doing what we designed.";
    case "refuse":
      return "Not verified — we will not ship this correction silently.";
  }
}

/**
 * The residual sentence: what we measured against what we allowed.
 *
 * Returns `null` when `decide()` never got far enough to grade, which is a real
 * outcome and not a zero: a bundle refused on routing or on a dropped band
 * carries diagnostics and no residual, and rendering "0.0 dB" there would read
 * as a perfect result to the one user who most needs to be told otherwise.
 */
export function residualLine(report: VerifyReport): string | null {
  const { gate_db: gate, residual_rms_db: residual } = report;
  if (residual == null || !Number.isFinite(residual)) return null;
  const measured = `${residual.toFixed(1)} dB RMS`;
  if (gate == null || !Number.isFinite(gate)) return `Residual ${measured}.`;
  const verb = residual <= gate ? "within" : "outside";
  return `Residual ${measured} — ${verb} the ${gate.toFixed(1)} dB limit.`;
}

/**
 * The preamp disclosure, which is MANDATORY rather than decorative.
 *
 * The correction is applying this much attenuation to everything the user
 * plays, and they have to be told the number and why. It is the engine's own
 * armed preamp — recomputed at the live rate over the bands that survived —
 * and not the plan's, which is a different number whenever the two rates
 * differ.
 */
export function preampDisclosure(report: VerifyReport): string {
  const db = report.installed_preamp_db;
  if (!Number.isFinite(db) || db >= 0) {
    return "No headroom was needed: the correction only cuts.";
  }
  // The MAGNITUDE, not the signed value: "-6.0 dB ... turned down by this much"
  // is a double negative, and wizard-design.md's own wording uses the positive
  // number. The sign lives in the sentence, where a reader can use it.
  return (
    `Headroom: ${Math.abs(db).toFixed(1)} dB. The correction boosts some ` +
    `frequencies, so everything is turned down by this much first to leave ` +
    `room for them — otherwise the loud parts would clip.`
  );
}

/**
 * The MS-18 acknowledgement's text: the device and the SPL, named.
 *
 * `null` outside the `armed` phase, because the acknowledgement is not a
 * confirmation dialog that can be shown at any time — it is the one door to a
 * sweep, and it must state the two facts the user is consenting to.
 */
export function acknowledgementPrompt(state: VerifyState): string | null {
  if (state.phase !== "armed") return null;
  const spl = Number.isFinite(state.projected_spl_db)
    ? `about ${Math.round(state.projected_spl_db)} dB SPL`
    : "an unknown level";
  return (
    `A test sweep will play through ${state.device_name} at ${spl} at the ` +
    `microphone. Take your headphones off your ears if you are wearing them.`
  );
}

/**
 * Diagnostics in the order a person should read them: refusals first.
 *
 * A warning listed above the refusal that stopped the run buries the reason
 * nothing was installed. Within a severity the engine's own order is kept —
 * it is the order the checks ran in, and that is the order the failure
 * happened in.
 */
export function orderedDiagnostics(report: VerifyReport): VerifyDiagnostic[] {
  const rank = (d: VerifyDiagnostic) => (d.severity === "refuse" ? 0 : 1);
  return [...report.diagnostics].sort((a, b) => rank(a) - rank(b));
}

/**
 * Is an abort meaningful right now?
 *
 * True while a pass exists, so the control is live from the moment a sweep
 * COULD play rather than from the moment one does: arming already holds the
 * measurement lease and has pinned the user's trim, and those must be
 * releasable without waiting for audio.
 */
export function canAbort(state: VerifyState): boolean {
  return state.phase === "armed" || state.phase === "running";
}

/**
 * The app-wide abort banner's line, or `null` when there is nothing to stop.
 *
 * This copy belongs to a control that lives OUTSIDE the Measure tab, and that
 * is the point: a verification pass keeps its lease, its pinned trim and — once
 * running — its sweep, no matter which tab the user is looking at, so the way
 * to stop it has to be visible from all of them. It names the phase because
 * "armed" and "playing" are different promises: one says nothing has made a
 * sound yet, the other says something is making one right now.
 */
export function abortBannerText(state: VerifyState): string | null {
  switch (state.phase) {
    case "armed":
      return `Verification is armed on ${state.device_name}. Nothing has played yet.`;
    case "running":
      return "A verification sweep is playing. Keep the room quiet.";
    default:
      return null;
  }
}
