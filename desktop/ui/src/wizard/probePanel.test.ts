import { describe, expect, it } from "vitest";

import { isTerminalPhase, nextProbePhase, type ProbePhase } from "./probePanel";

describe("nextProbePhase", () => {
  it("stays probing while still_probing and status transient", () => {
    expect(nextProbePhase("probing", "still_probing", "starting")).toBe("probing");
    expect(nextProbePhase("probing", null, "no_input_detected")).toBe("probing");
    expect(nextProbePhase("probing", "still_probing", "idle")).toBe("probing");
  });

  it("goes to success on a running verdict", () => {
    expect(nextProbePhase("probing", "running", "running")).toBe("success");
  });

  it("goes to guidance on a timed_out verdict", () => {
    expect(nextProbePhase("probing", "timed_out", "no_input_detected")).toBe("guidance");
  });

  it("goes to guidance when the engine already auto-disabled", () => {
    expect(nextProbePhase("probing", "still_probing", "auto_disabled_no_input")).toBe("guidance");
  });

  it("goes to failed on a hard engine failure, over any verdict", () => {
    expect(nextProbePhase("probing", "running", "failed")).toBe("failed");
    expect(nextProbePhase("probing", "still_probing", "failed")).toBe("failed");
  });

  it("leaves terminal phases unchanged (only Re-test resets to probing)", () => {
    for (const phase of ["intro", "success", "guidance", "failed"] as ProbePhase[]) {
      expect(nextProbePhase(phase, "running", "running")).toBe(phase);
      expect(nextProbePhase(phase, "timed_out", "no_input_detected")).toBe(phase);
    }
  });
});

describe("isTerminalPhase", () => {
  it("marks the three result panels terminal", () => {
    expect(isTerminalPhase("success")).toBe(true);
    expect(isTerminalPhase("guidance")).toBe(true);
    expect(isTerminalPhase("failed")).toBe(true);
  });

  it("marks intro and probing non-terminal", () => {
    expect(isTerminalPhase("intro")).toBe(false);
    expect(isTerminalPhase("probing")).toBe(false);
  });
});
