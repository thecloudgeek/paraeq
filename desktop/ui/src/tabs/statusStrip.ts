// Pure helpers for the EQ tab's status strip, split out so they can be unit
// tested without rendering the component (same pattern as wizard/probePanel.ts).

/**
 * The user-facing disclosure for `EngineState.correction_rate_mismatch` (R1-6).
 *
 * The engine could not build the retained correction for the live stream --
 * baked coefficients from another rate, or a band set with nothing left below
 * the new Nyquist -- so it is running FLAT. That is the safe direction, but a
 * silent fail-open is exactly the invisible failure the hardening work exists
 * to prevent, so the strip has to say so. Returns `null` when there is nothing
 * to disclose.
 */
export function rateMismatchNotice(mismatch: number | null): string | null {
  if (mismatch == null || !Number.isFinite(mismatch)) return null;
  return `EQ paused — correction must be redesigned for ${formatRate(mismatch)}`;
}

/** 44100 -> "44.1 kHz", 48000 -> "48 kHz". Mirrors the strip's other rate. */
function formatRate(hz: number): string {
  const khz = hz / 1000;
  const text = Number.isInteger(khz) ? khz.toString() : khz.toFixed(1);
  return `${text} kHz`;
}
