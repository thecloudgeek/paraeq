import { describe, expect, it } from "vitest";

import { rateMismatchNotice } from "./statusStrip";

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
