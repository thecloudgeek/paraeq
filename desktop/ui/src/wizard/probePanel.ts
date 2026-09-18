// The verify-capture step's client-side state machine, factored out pure so it
// is unit-testable without a DOM. The 10 s timing decision itself lives in Rust
// (`setup::probe_verdict`, surfaced by the `setup_probe_verdict` command); this
// only maps that verdict + the live engine status onto the panel the wizard
// shows.

import type { EngineStatus, ProbeVerdict } from "@/ipc/types";

/** The verify step's panels. "probing" is the only non-terminal phase; the
 *  three terminal phases stay put until an explicit Re-test resets to
 *  "probing". "intro" is before the user clicks Enable EQ. */
export type ProbePhase = "failed" | "guidance" | "intro" | "probing" | "success";

/**
 * Pure transition. Only advances OUT of "probing":
 *   - a hard engine `failed`  → "failed" (the wizard surfaces `reason`)
 *   - Rust verdict `running`  → "success" (capture works)
 *   - Rust verdict `timed_out`, or the engine already `auto_disabled_no_input`
 *                             → "guidance" (TCC grant likely missing)
 *   - anything else           → stay "probing".
 * Any non-probing phase is returned unchanged (terminal until Re-test).
 */
export function nextProbePhase(
  phase: ProbePhase,
  verdict: ProbeVerdict | null,
  statusKind: EngineStatus["kind"],
): ProbePhase {
  if (phase !== "probing") return phase;
  if (statusKind === "failed") return "failed";
  if (verdict === "running") return "success";
  if (verdict === "timed_out" || statusKind === "auto_disabled_no_input") return "guidance";
  return "probing";
}

/** Whether a phase is terminal (the probe should be stopped and polling ended). */
export function isTerminalPhase(phase: ProbePhase): boolean {
  return phase === "failed" || phase === "guidance" || phase === "success";
}
