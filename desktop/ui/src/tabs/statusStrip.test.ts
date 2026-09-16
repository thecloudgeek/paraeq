import { describe, expect, it } from "vitest";

import { clippedNotice, rateMismatchNotice } from "./statusStrip";

describe("rateMismatchNotice", () => {
  it("says nothing when the engine installed the correction", () => {
    expect(rateMismatchNotice(null)).toBeNull();
  });

  it("names the rate the correction must be redesigned for", () => {
    expect(rateMismatchNotice(44100)).toBe(
      "EQ paused — correction must be redesigned for 44.1 kHz",
    );
  });

  it("drops the decimal on whole-kHz rates", () => {
    expect(rateMismatchNotice(48000)).toBe(
      "EQ paused — correction must be redesigned for 48 kHz",
    );
    expect(rateMismatchNotice(96000)).toBe(
      "EQ paused — correction must be redesigned for 96 kHz",
    );
  });

  it("stays silent on a nonsense rate rather than rendering NaN at the user", () => {
    expect(rateMismatchNotice(Number.NaN)).toBeNull();
  });
});

describe("clippedNotice", () => {
  it("says nothing when the clamp never engaged", () => {
    expect(clippedNotice(0, null)).toBeNull();
    expect(clippedNotice(0, -9.4)).toBeNull();
  });

  it("reports a plain clip count when no auto-preamp is running", () => {
    expect(clippedNotice(1234, null)).toBe("clipped (1,234 samples)");
  });

  it("flags a clip under an active auto-preamp as unexpected (spec :571)", () => {
    expect(clippedNotice(6, -12)).toBe(
      "clipped (6 samples — unexpected with auto-preamp)",
    );
  });

  it("stays silent on a nonsense count rather than rendering NaN at the user", () => {
    expect(clippedNotice(Number.NaN, null)).toBeNull();
  });
});
