// A thin React wrapper around the pure FreqPlotRenderer (Task 12). It owns NO
// domain state: parents pass `traces` + `handles` and receive interaction
// callbacks. All geometry/hit-test math lives in the renderer (unit-tested);
// this file is deliberately dumb plumbing — two stacked canvases, a
// ResizeObserver + devicePixelRatio pass, and a pointer/wheel event protocol.
//
// Layers: a base canvas (grid + traces, redrawn only when traces/size change)
// under an overlay canvas (handles + live drag feedback, redrawn via a
// dirty-flag requestAnimationFrame flush). One renderer instance, shared by
// both. The rAF loop runs imperatively (ref-owned), OUTSIDE React's render
// cycle, so drags never thrash component state.

import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import type { JSX } from "react";

import { FreqPlotRenderer } from "@/plot/FreqPlotRenderer";
import type { PlotHandle, PlotRange, Trace } from "@/plot/FreqPlotRenderer";

export interface FrequencyPlotProps {
  handles?: PlotHandle[];
  onHandleDrag?: (id: number, f: number, db: number, phase: "move" | "end" | "start") => void;
  onHandleWheel?: (id: number, deltaY: number) => void;
  range: PlotRange;
  title?: string;
  traces: Trace[];
}

interface Size {
  cssH: number;
  cssW: number;
  dpr: number;
}

export function FrequencyPlot(props: FrequencyPlotProps): JSX.Element {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const baseCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const overlayCanvasRef = useRef<HTMLCanvasElement | null>(null);

  const rendererRef = useRef<FreqPlotRenderer | null>(null);
  const sizeRef = useRef<Size>({ cssH: 0, cssW: 0, dpr: 1 });

  // Ephemeral in-flight drag state — the ONLY state this component holds, and
  // it never triggers a React render (imperative canvas feedback only).
  const draggingRef = useRef<{ id: number } | null>(null);
  const pendingMoveRef = useRef<{ x: number; y: number } | null>(null);
  const rafRef = useRef<number | null>(null);

  // Latest props for the imperative (rAF / native-listener) code paths, which
  // are created once and would otherwise capture stale props.
  const latest = useRef(props);
  latest.current = props;

  const redrawBase = useCallback(() => {
    const renderer = rendererRef.current;
    const canvas = baseCanvasRef.current;
    if (!renderer || !canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const { cssH, cssW } = sizeRef.current;
    ctx.clearRect(0, 0, cssW, cssH);
    renderer.drawGrid(ctx);
    renderer.drawTraces(ctx, latest.current.traces);
  }, []);

  const redrawOverlay = useCallback(() => {
    const renderer = rendererRef.current;
    const canvas = overlayCanvasRef.current;
    if (!renderer || !canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const { cssH, cssW } = sizeRef.current;
    ctx.clearRect(0, 0, cssW, cssH);
    renderer.drawHandles(ctx, latest.current.handles ?? [], draggingRef.current?.id ?? null);
  }, []);

  // Renderer lifecycle + sizing (DPR) + ResizeObserver + native wheel listener.
  // Re-runs only when `range` changes (a new renderer is needed for new axes).
  useLayoutEffect(() => {
    const container = containerRef.current;
    const overlay = overlayCanvasRef.current;
    if (!container || !overlay) return;

    const renderer = new FreqPlotRenderer(latest.current.range);
    rendererRef.current = renderer;

    const applySize = () => {
      const rect = container.getBoundingClientRect();
      const cssW = rect.width;
      const cssH = rect.height;
      if (cssW === 0 || cssH === 0) return;
      const dpr = window.devicePixelRatio || 1;
      sizeRef.current = { cssH, cssW, dpr };
      for (const canvas of [baseCanvasRef.current, overlayCanvasRef.current]) {
        if (!canvas) continue;
        canvas.width = Math.round(cssW * dpr);
        canvas.height = Math.round(cssH * dpr);
        canvas.style.width = `${cssW}px`;
        canvas.style.height = `${cssH}px`;
        // Draw in CSS px; the DPR scale keeps strokes crisp on Retina.
        canvas.getContext("2d")?.setTransform(dpr, 0, 0, dpr, 0, 0);
      }
      renderer.setSize(cssW, cssH);
      redrawBase();
      redrawOverlay();
    };

    // React registers `wheel` as passive at its root, so preventDefault() there
    // is ignored — attach a native non-passive listener to actually cancel the
    // page scroll when the pointer is over a handle.
    const onWheel = (e: WheelEvent) => {
      const r = rendererRef.current;
      if (!r) return;
      const rect = overlay.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      const id = r.hitTest(x, y, latest.current.handles ?? []);
      if (id === null) return; // no hit → default scroll (no pan/zoom in stage 4)
      e.preventDefault();
      latest.current.onHandleWheel?.(id, e.deltaY);
    };
    overlay.addEventListener("wheel", onWheel, { passive: false });

    applySize();
    const observer = new ResizeObserver(applySize);
    observer.observe(container);

    return () => {
      observer.disconnect();
      overlay.removeEventListener("wheel", onWheel);
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
  }, [props.range, redrawBase, redrawOverlay]);

  // Diff-driven layer redraws (props in → imperative canvas out).
  useEffect(() => {
    redrawBase();
  }, [props.traces, redrawBase]);

  useEffect(() => {
    redrawOverlay();
  }, [props.handles, redrawOverlay]);

  const localPoint = (e: { clientX: number; clientY: number }): { x: number; y: number } => {
    const canvas = overlayCanvasRef.current;
    if (!canvas) return { x: 0, y: 0 };
    const rect = canvas.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };

  // Coalesce moves to at most one onHandleDrag("move") per animation frame.
  const scheduleMoveFlush = useCallback(() => {
    if (rafRef.current !== null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      const pending = pendingMoveRef.current;
      const drag = draggingRef.current;
      const renderer = rendererRef.current;
      if (!pending || !drag || !renderer) return;
      pendingMoveRef.current = null;
      const { db, f } = renderer.clampToRange(
        renderer.freqForX(pending.x),
        renderer.dbForY(pending.y),
      );
      latest.current.onHandleDrag?.(drag.id, f, db, "move");
    });
  }, []);

  const handlePointerDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const renderer = rendererRef.current;
    if (!renderer) return;
    const { x, y } = localPoint(e);
    const handles = props.handles ?? [];
    const id = renderer.hitTest(x, y, handles);
    if (id === null) return; // miss → no drag (no pan/zoom in stage 4)
    const handle = handles.find((h) => h.id === id);
    if (!handle) return;
    draggingRef.current = { id };
    overlayCanvasRef.current?.setPointerCapture(e.pointerId);
    props.onHandleDrag?.(id, handle.f, handle.db, "start");
    redrawOverlay(); // active handle draws larger
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (draggingRef.current === null) return;
    pendingMoveRef.current = localPoint(e);
    scheduleMoveFlush();
  };

  const handlePointerUp = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const drag = draggingRef.current;
    if (drag === null) return;
    draggingRef.current = null;
    pendingMoveRef.current = null;
    overlayCanvasRef.current?.releasePointerCapture(e.pointerId);
    const renderer = rendererRef.current;
    if (renderer) {
      const { x, y } = localPoint(e);
      const { db, f } = renderer.clampToRange(renderer.freqForX(x), renderer.dbForY(y));
      props.onHandleDrag?.(drag.id, f, db, "end");
    }
    redrawOverlay(); // active handle shrinks back
  };

  return (
    <div ref={containerRef} className="relative h-full w-full">
      {props.title ? (
        <span className="pointer-events-none absolute right-2 top-1 z-10 text-xs text-muted-foreground">
          {props.title}
        </span>
      ) : null}
      <canvas ref={baseCanvasRef} className="pointer-events-none absolute inset-0" />
      <canvas
        ref={overlayCanvasRef}
        className="absolute inset-0"
        style={{ touchAction: "none" }}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerUp}
      />
    </div>
  );
}
