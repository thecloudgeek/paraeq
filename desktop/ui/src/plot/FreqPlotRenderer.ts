// Pure, React-free frequency-response plot math + trivial canvas drawing.
//
// All coordinate/tick/hit-test logic is unit-tested (FreqPlotRenderer.test.ts)
// with NO DOM/canvas — the draw* methods take a ctx but delegate every
// calculation to the pure methods here, so their bodies are trivial loops.
// Keep this class EQ-agnostic: it is reused by target-anchor and analyzer
// traces in later stages.

export interface PlotRange {
  dbMax: number;
  dbMin: number;
  fMax: number;
  fMin: number;
}

// prototype parity
export const EQ_PLOT_RANGE: PlotRange = { dbMax: 20, dbMin: -20, fMax: 20000, fMin: 20 };

// prototype palette, cycled per band
export const BAND_PALETTE = [
  "#EF5350",
  "#FFA726",
  "#FFEE58",
  "#66BB6A",
  "#26C6DA",
  "#42A5F5",
  "#AB47BC",
  "#EC407A",
];

export interface Trace {
  color: string;
  dash?: number[];
  dbs: number[];
  freqs: number[];
  width: number;
}

export interface PlotHandle {
  color: string;
  db: number;
  f: number;
  id: number;
}

// Internal plot-area margins (CSS px).
const MARGIN_LEFT = 44;
const MARGIN_RIGHT = 8;
const MARGIN_TOP = 8;
const MARGIN_BOTTOM = 24;

// Fixed audio-standard frequency ticks.
const FREQ_TICKS: { f: number; label: string }[] = [
  { f: 20, label: "20" },
  { f: 50, label: "50" },
  { f: 100, label: "100" },
  { f: 200, label: "200" },
  { f: 500, label: "500" },
  { f: 1000, label: "1k" },
  { f: 2000, label: "2k" },
  { f: 5000, label: "5k" },
  { f: 10000, label: "10k" },
  { f: 20000, label: "20k" },
];

/** n log-spaced points from fMin..fMax inclusive (512 = prototype parity). */
export function logspace(fMin: number, fMax: number, n: number): number[] {
  const out = new Array<number>(n);
  if (n === 1) {
    out[0] = fMin;
    return out;
  }
  const logMin = Math.log10(fMin);
  const logMax = Math.log10(fMax);
  const step = (logMax - logMin) / (n - 1);
  for (let i = 0; i < n; i++) {
    out[i] = Math.pow(10, logMin + step * i);
  }
  // Pin endpoints exactly (avoid float drift at the ends).
  out[0] = fMin;
  out[n - 1] = fMax;
  return out;
}

// Q clamp for the handle-wheel gesture (Task 15). Deliberately narrower than
// the engine's Q_MAX=100: the wheel is a coarse sculpting gesture, and the
// table still accepts up to 100 by typing. Mirrors the plot's own gain clamp,
// which likewise bounds to the display range while the table allows ±30.
export const Q_WHEEL_MIN = 0.1;
export const Q_WHEEL_MAX = 20;

/**
 * One wheel notch over a band handle → the band's new Q (decision 13).
 * `q *= 1.05^-sign(deltaY)`: scroll up (deltaY < 0) narrows (Q up), scroll down
 * widens (Q down); clamped to [Q_WHEEL_MIN, Q_WHEEL_MAX] and rounded to 3
 * decimals (the table's Q display precision). Pure — unit-tested.
 */
export function qWheelStep(q: number, deltaY: number): number {
  const next = q * Math.pow(1.05, -Math.sign(deltaY));
  const clamped = Math.min(Q_WHEEL_MAX, Math.max(Q_WHEEL_MIN, next));
  return Math.round(clamped * 1000) / 1000;
}

export class FreqPlotRenderer {
  private range: PlotRange;
  private logFMin: number;
  private logFMax: number;

  // Plot-area edges in CSS px (set by setSize).
  private left = MARGIN_LEFT;
  private right = MARGIN_LEFT;
  private top = MARGIN_TOP;
  private bottom = MARGIN_TOP;

  constructor(range: PlotRange) {
    this.range = range;
    this.logFMin = Math.log10(range.fMin);
    this.logFMax = Math.log10(range.fMax);
  }

  /** CSS px; margins are internal. */
  setSize(cssWidth: number, cssHeight: number): void {
    this.left = MARGIN_LEFT;
    this.right = cssWidth - MARGIN_RIGHT;
    this.top = MARGIN_TOP;
    this.bottom = cssHeight - MARGIN_BOTTOM;
  }

  private get plotWidth(): number {
    return this.right - this.left;
  }

  private get plotHeight(): number {
    return this.bottom - this.top;
  }

  // ---- pure transforms (CSS px) ----

  xForFreq(f: number): number {
    const t = (Math.log10(f) - this.logFMin) / (this.logFMax - this.logFMin);
    return this.left + this.plotWidth * t;
  }

  freqForX(x: number): number {
    const t = (x - this.left) / this.plotWidth;
    return Math.pow(10, this.logFMin + t * (this.logFMax - this.logFMin));
  }

  yForDb(db: number): number {
    const t = (this.range.dbMax - db) / (this.range.dbMax - this.range.dbMin);
    return this.top + this.plotHeight * t;
  }

  dbForY(y: number): number {
    const t = (y - this.top) / this.plotHeight;
    return this.range.dbMax - t * (this.range.dbMax - this.range.dbMin);
  }

  freqTicks(): { f: number; label: string }[] {
    return FREQ_TICKS.map((t) => ({ ...t }));
  }

  dbTicks(step: number): number[] {
    const out: number[] = [];
    for (let db = this.range.dbMin; db <= this.range.dbMax + 1e-9; db += step) {
      out.push(Math.round(db * 1e6) / 1e6);
    }
    return out;
  }

  clampToRange(f: number, db: number): { db: number; f: number } {
    return {
      db: Math.min(this.range.dbMax, Math.max(this.range.dbMin, db)),
      f: Math.min(this.range.fMax, Math.max(this.range.fMin, f)),
    };
  }

  /** Nearest handle within radiusPx (default 10), else null. */
  hitTest(x: number, y: number, handles: PlotHandle[], radiusPx = 10): number | null {
    let best: number | null = null;
    let bestDist = radiusPx;
    for (const h of handles) {
      const dx = this.xForFreq(h.f) - x;
      const dy = this.yForDb(h.db) - y;
      const dist = Math.hypot(dx, dy);
      if (dist <= bestDist) {
        bestDist = dist;
        best = h.id;
      }
    }
    return best;
  }

  // ---- drawing (ctx already DPR-scaled by the wrapper) ----

  drawGrid(ctx: CanvasRenderingContext2D): void {
    ctx.save();
    ctx.strokeStyle = "rgba(255,255,255,0.10)";
    ctx.fillStyle = "rgba(255,255,255,0.55)";
    ctx.lineWidth = 1;
    ctx.font = "10px system-ui, sans-serif";
    ctx.textBaseline = "top";

    for (const tick of this.freqTicks()) {
      const x = this.xForFreq(tick.f);
      ctx.beginPath();
      ctx.moveTo(x, this.top);
      ctx.lineTo(x, this.bottom);
      ctx.stroke();
      ctx.textAlign = "center";
      ctx.fillText(tick.label, x, this.bottom + 4);
    }

    for (const db of this.dbTicks(5)) {
      const y = this.yForDb(db);
      ctx.beginPath();
      ctx.moveTo(this.left, y);
      ctx.lineTo(this.right, y);
      ctx.stroke();
      ctx.textAlign = "right";
      ctx.textBaseline = "middle";
      ctx.fillText(String(db), this.left - 6, y);
      ctx.textBaseline = "top";
    }
    ctx.restore();
  }

  drawTraces(ctx: CanvasRenderingContext2D, traces: Trace[]): void {
    ctx.save();
    for (const trace of traces) {
      ctx.beginPath();
      ctx.strokeStyle = trace.color;
      ctx.lineWidth = trace.width;
      ctx.setLineDash(trace.dash ?? []);
      const n = Math.min(trace.freqs.length, trace.dbs.length);
      for (let i = 0; i < n; i++) {
        const x = this.xForFreq(trace.freqs[i]);
        const y = this.yForDb(trace.dbs[i]);
        if (i === 0) {
          ctx.moveTo(x, y);
        } else {
          ctx.lineTo(x, y);
        }
      }
      ctx.stroke();
    }
    ctx.setLineDash([]);
    ctx.restore();
  }

  drawHandles(
    ctx: CanvasRenderingContext2D,
    handles: PlotHandle[],
    activeId: number | null,
  ): void {
    ctx.save();
    for (const h of handles) {
      const x = this.xForFreq(h.f);
      const y = this.yForDb(h.db);
      const r = h.id === activeId ? 7 : 5;
      ctx.beginPath();
      ctx.arc(x, y, r, 0, Math.PI * 2);
      ctx.fillStyle = h.color;
      ctx.fill();
      ctx.lineWidth = 2;
      ctx.strokeStyle = "rgba(0,0,0,0.6)";
      ctx.stroke();
    }
    ctx.restore();
  }
}
