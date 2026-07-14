import { describe, expect, it } from "vitest";

import {
  BAND_PALETTE,
  EQ_PLOT_RANGE,
  FreqPlotRenderer,
  logspace,
  type PlotHandle,
} from "./FreqPlotRenderer";

// Internal margins (must match the renderer): left 44, bottom 24, top/right 8.
const LEFT = 44;
const RIGHT_MARGIN = 8;
const TOP = 8;
const BOTTOM_MARGIN = 24;

function makeRenderer(w = 800, h = 400): FreqPlotRenderer {
  const r = new FreqPlotRenderer(EQ_PLOT_RANGE);
  r.setSize(w, h);
  return r;
}

describe("EQ_PLOT_RANGE / BAND_PALETTE", () => {
  it("matches prototype parity", () => {
    expect(EQ_PLOT_RANGE).toEqual({ dbMax: 20, dbMin: -20, fMax: 20000, fMin: 20 });
    expect(BAND_PALETTE).toHaveLength(8);
  });
});

describe("x transforms", () => {
  it("xForFreq(fMin) is the plot-area left edge", () => {
    const r = makeRenderer();
    expect(r.xForFreq(EQ_PLOT_RANGE.fMin)).toBeCloseTo(LEFT, 9);
  });

  it("xForFreq(fMax) is the plot-area right edge", () => {
    const w = 800;
    const r = makeRenderer(w, 400);
    expect(r.xForFreq(EQ_PLOT_RANGE.fMax)).toBeCloseTo(w - RIGHT_MARGIN, 9);
  });

  it("xForFreq(1000) equals the analytic value", () => {
    const w = 800;
    const r = makeRenderer(w, 400);
    const left = LEFT;
    const plotW = w - RIGHT_MARGIN - LEFT;
    const expected = left + plotW * (Math.log10(1000 / 20) / Math.log10(20000 / 20));
    expect(r.xForFreq(1000)).toBeCloseTo(expected, 9);
  });

  it("round-trips freqForX(xForFreq(f)) ~= f across a log sweep (1e-9 rel)", () => {
    const r = makeRenderer();
    for (let i = 0; i <= 100; i++) {
      const f = 20 * Math.pow(1000, i / 100); // 20 .. 20000
      const back = r.freqForX(r.xForFreq(f));
      expect(Math.abs(back - f) / f).toBeLessThan(1e-9);
    }
  });
});

describe("y transforms", () => {
  it("yForDb(0) is the vertical center for the symmetric range", () => {
    const h = 400;
    const r = makeRenderer(800, h);
    const center = TOP + (h - BOTTOM_MARGIN - TOP) / 2;
    expect(r.yForDb(0)).toBeCloseTo(center, 9);
  });

  it("yForDb(dbMax) is the top edge, yForDb(dbMin) is the bottom edge", () => {
    const h = 400;
    const r = makeRenderer(800, h);
    expect(r.yForDb(EQ_PLOT_RANGE.dbMax)).toBeCloseTo(TOP, 9);
    expect(r.yForDb(EQ_PLOT_RANGE.dbMin)).toBeCloseTo(h - BOTTOM_MARGIN, 9);
  });

  it("round-trips dbForY(yForDb(db)) ~= db", () => {
    const r = makeRenderer();
    for (let db = -20; db <= 20; db += 2.5) {
      expect(r.dbForY(r.yForDb(db))).toBeCloseTo(db, 9);
    }
  });
});

describe("freqTicks", () => {
  it("returns exactly the fixed 10-tick audio list with labels", () => {
    const r = makeRenderer();
    const ticks = r.freqTicks();
    expect(ticks.map((t) => t.f)).toEqual([
      20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000,
    ]);
    expect(ticks.map((t) => t.label)).toEqual([
      "20", "50", "100", "200", "500", "1k", "2k", "5k", "10k", "20k",
    ]);
  });
});

describe("dbTicks", () => {
  it("step 5 yields -20..20", () => {
    const r = makeRenderer();
    expect(r.dbTicks(5)).toEqual([-20, -15, -10, -5, 0, 5, 10, 15, 20]);
  });
});

describe("logspace", () => {
  it("has n points with exact endpoints and constant neighbor ratio", () => {
    const pts = logspace(20, 20000, 512);
    expect(pts).toHaveLength(512);
    expect(pts[0]).toBe(20);
    expect(pts[511]).toBe(20000);
    const r0 = pts[1] / pts[0];
    for (let i = 1; i < pts.length; i++) {
      const ratio = pts[i] / pts[i - 1];
      expect(Math.abs(ratio - r0) / r0).toBeLessThan(1e-9);
    }
  });
});

describe("hitTest", () => {
  const handles: PlotHandle[] = [
    { color: "#EF5350", db: 0, f: 100, id: 0 },
    { color: "#FFA726", db: 6, f: 1000, id: 1 },
    { color: "#FFEE58", db: -6, f: 10000, id: 2 },
  ];

  it("selects the nearest handle within the radius", () => {
    const r = makeRenderer();
    const px = r.xForFreq(1000);
    const py = r.yForDb(6);
    expect(r.hitTest(px + 3, py - 2, handles)).toBe(1);
  });

  it("returns null when outside the radius", () => {
    const r = makeRenderer();
    const px = r.xForFreq(1000);
    const py = r.yForDb(6);
    expect(r.hitTest(px + 50, py + 50, handles)).toBeNull();
  });

  it("prefers the nearer of two overlapping handles", () => {
    const r = makeRenderer();
    const overlap: PlotHandle[] = [
      { color: "#EF5350", db: 0, f: 1000, id: 5 },
      { color: "#FFA726", db: 0.2, f: 1000, id: 6 },
    ];
    const px = r.xForFreq(1000);
    const py = r.yForDb(0); // nearer to id 5 (db 0) than id 6 (db 0.2)
    expect(r.hitTest(px, py, overlap)).toBe(5);
  });
});

describe("clampToRange", () => {
  it("clamps both axes", () => {
    const r = makeRenderer();
    expect(r.clampToRange(5, 0)).toEqual({ db: 0, f: 20 });
    expect(r.clampToRange(50000, 100)).toEqual({ db: 20, f: 20000 });
    expect(r.clampToRange(1000, -100)).toEqual({ db: -20, f: 1000 });
  });
});
