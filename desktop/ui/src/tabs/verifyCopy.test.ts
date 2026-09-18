import { describe, expect, it } from "vitest";

import type { VerifyDiagnostic, VerifyReport } from "@/ipc/types";
import {
  abortBannerText,
  acknowledgementPrompt,
  canAbort,
  diagnosticLabel,
  orderedDiagnostics,
  preampDisclosure,
  residualLine,
  verifyHeadline,
} from "./verifyCopy";

function report(overrides: Partial<VerifyReport> = {}): VerifyReport {
  return {
    abort_acoustic_budget_ms: 21.7,
    diagnostics: [],
    gate_db: 2.0,
    installed_preamp_db: -6.0,
    level_dbfs: -21.0,
    residual_rms_db: 1.8,
    verdict: "proceed",
    ...overrides,
  };
}

describe("verifyHeadline", () => {
  it("covers every phase", () => {
    expect(verifyHeadline({ phase: "idle" })).toContain("No verification");
    expect(
      verifyHeadline({
        phase: "armed",
        device_name: "AirPods Max",
        level_dbfs: -21,
        projected_spl_db: 78,
      }),
    ).toContain("confirm the level");
    expect(verifyHeadline({ phase: "running" })).toContain("analysing");
    expect(
      verifyHeadline({
        phase: "failed",
        code: 24,
        remedy: "Turn ParaEQ on.",
        summary: "EngineNotRunning",
      }),
    ).toContain("EngineNotRunning");
  });

  it("says what the verdict means, not what it is called", () => {
    expect(verifyHeadline({ phase: "complete", report: report() })).toBe(
      "Verified — the correction is doing what we designed.",
    );
    expect(
      verifyHeadline({
        phase: "complete",
        report: report({ verdict: "refuse" }),
      }),
    ).toContain("will not ship this correction silently");
  });
});

describe("residualLine", () => {
  it("names both numbers and which side of the limit we landed on", () => {
    expect(residualLine(report())).toBe(
      "Residual 1.8 dB RMS — within the 2.0 dB limit.",
    );
    expect(residualLine(report({ residual_rms_db: 2.9 }))).toBe(
      "Residual 2.9 dB RMS — outside the 2.0 dB limit.",
    );
  });

  // The failure this guards: a bundle refused before grading carries no
  // residual, and rendering it as "0.0 dB" would tell the one user who most
  // needs to be told otherwise that the correction is perfect.
  it("says nothing rather than zero when decide() never graded", () => {
    expect(residualLine(report({ residual_rms_db: null }))).toBeNull();
    expect(residualLine(report({ residual_rms_db: Number.NaN }))).toBeNull();
  });

  it("still reports a residual when only the limit is missing", () => {
    expect(residualLine(report({ gate_db: null }))).toBe("Residual 1.8 dB RMS.");
  });
});

describe("preampDisclosure", () => {
  // Mandatory, not decorative: this is the number the user's music is now
  // being played through.
  // The MAGNITUDE, not the signed value: "-6.0 dB ... turned down by this
  // much" is a double negative, and the spec's own wording uses the positive
  // number. The sign lives in the sentence.
  it("names the number as a magnitude and explains it in the user's terms", () => {
    const text = preampDisclosure(report());
    expect(text).toContain("6.0 dB");
    expect(text).not.toContain("-6.0 dB");
    expect(text).toContain("turned down");
    expect(text).toContain("clip");
  });

  it("says so plainly when no headroom was taken", () => {
    expect(preampDisclosure(report({ installed_preamp_db: 0 }))).toContain(
      "only cuts",
    );
  });
});

describe("acknowledgementPrompt", () => {
  // MS-18: the acknowledgement names the device and the projected SPL, and it
  // is the only door to a sweep.
  it("names the device and the level in the armed phase", () => {
    const text = acknowledgementPrompt({
      phase: "armed",
      device_name: "AirPods Max",
      level_dbfs: -21,
      projected_spl_db: 78.4,
    });
    expect(text).toContain("AirPods Max");
    expect(text).toContain("78 dB SPL");
  });

  it("does not exist outside the armed phase", () => {
    expect(acknowledgementPrompt({ phase: "idle" })).toBeNull();
    expect(acknowledgementPrompt({ phase: "running" })).toBeNull();
    expect(
      acknowledgementPrompt({ phase: "complete", report: report() }),
    ).toBeNull();
  });
});

describe("orderedDiagnostics", () => {
  // A warning listed above the refusal that stopped the run buries the reason
  // nothing was installed.
  it("puts refusals first and keeps the engine's order within a severity", () => {
    const ordered = orderedDiagnostics(
      report({
        diagnostics: [
          { code: 105, remedy: "a", severity: "warn", summary: "first warn" },
          { code: 41, remedy: "b", severity: "refuse", summary: "the refusal" },
          { code: 106, remedy: "c", severity: "warn", summary: "second warn" },
        ],
      }),
    );
    expect(ordered.map((d) => d.summary)).toEqual([
      "the refusal",
      "first warn",
      "second warn",
    ]);
  });

  // Ruling R-A1's severity reads between the two: it did not stop the run, so
  // it goes under the refusals, but it threw away one of the user's captures,
  // so it must not be buried among the notes.
  it("sorts a dropped position under the refusals and above the notes", () => {
    const ordered = orderedDiagnostics(
      report({
        diagnostics: [
          { code: 105, remedy: "a", severity: "warn", summary: "a note" },
          { code: 31, remedy: "b", severity: "dropped", summary: "position 3" },
          { code: 41, remedy: "c", severity: "refuse", summary: "the refusal" },
          { code: 32, remedy: "d", severity: "dropped", summary: "position 4" },
        ],
      }),
    );
    expect(ordered.map((d) => d.summary)).toEqual([
      "the refusal",
      "position 3",
      "position 4",
      "a note",
    ]);
  });
});

describe("diagnosticLabel", () => {
  // The badge is the only word most people read. "Dropped" alone does not say
  // what was dropped, and the enum's own name is not user-facing copy.
  it("says what each severity DID, and names the thing set aside", () => {
    const at = (severity: VerifyDiagnostic["severity"]) =>
      diagnosticLabel({ code: 1, remedy: "r", severity, summary: "s" });
    expect(at("refuse")).toBe("stops here");
    expect(at("dropped")).toBe("one position was set aside");
    expect(at("warn")).toBe("note");
  });
});

describe("canAbort", () => {
  // Live from the moment a sweep COULD play: arming already holds the
  // measurement lease and has pinned the user's trim, and those must be
  // releasable without waiting for audio.
  it("is true while a pass exists and false otherwise", () => {
    expect(
      canAbort({
        phase: "armed",
        device_name: "d",
        level_dbfs: -21,
        projected_spl_db: 78,
      }),
    ).toBe(true);
    expect(canAbort({ phase: "running" })).toBe(true);
    expect(canAbort({ phase: "idle" })).toBe(false);
    expect(canAbort({ phase: "complete", report: report() })).toBe(false);
    expect(
      canAbort({ phase: "failed", code: null, remedy: null, summary: "x" }),
    ).toBe(false);
  });
});

describe("abortBannerText", () => {
  // The banner lives outside the Measure tab, because a pass keeps its lease,
  // its pinned trim and its sweep no matter which tab is in front. Its two
  // lines are different promises: armed means nothing has made a sound yet,
  // running means something is making one right now.
  it("distinguishes armed from running and names the device", () => {
    const armed = abortBannerText({
      phase: "armed",
      device_name: "AirPods Max",
      level_dbfs: -21,
      projected_spl_db: 78,
    });
    expect(armed).toContain("AirPods Max");
    expect(armed).toContain("Nothing has played yet");
    expect(abortBannerText({ phase: "running" })).toContain("playing");
  });

  it("says nothing when there is nothing to stop", () => {
    expect(abortBannerText({ phase: "idle" })).toBeNull();
    expect(
      abortBannerText({ phase: "failed", code: null, remedy: null, summary: "x" }),
    ).toBeNull();
  });

  // The banner and the Esc binding are shown by the same predicate, so a phase
  // that can be aborted must always have a line to show.
  it("has a line for exactly the phases that can be aborted", () => {
    for (const state of [
      { phase: "idle" } as const,
      { phase: "running" } as const,
      {
        phase: "armed",
        device_name: "d",
        level_dbfs: -21,
        projected_spl_db: 78,
      } as const,
    ]) {
      expect(abortBannerText(state) !== null).toBe(canAbort(state));
    }
  });
});
