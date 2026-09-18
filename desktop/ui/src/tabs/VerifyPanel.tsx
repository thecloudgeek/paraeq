// The Verify panel: the minimum surface that can show a verification pass
// honestly. Deliberately NOT the measurement wizard — Stage 7 owns the spine
// that measures a baseline, and nothing here can produce one.
//
// What it is for until then: an armed pass gets its MS-18 acknowledgement, a
// running pass gets an abort, and a finished pass shows its residual against
// its limit, its verdict, the mandatory preamp disclosure, and every diagnostic
// with the remedy Rust wrote for it.
//
// Arming is not reachable from here, and that is honest rather than
// unfinished: `verify_arm` needs a baseline `MeasurementBundle` and an armed
// `CorrectionPlan`, which only the wizard produces. The panel says so instead
// of offering a button that cannot work.

import { useState } from "react";
import type { JSX } from "react";

import { Button } from "@/components/ui/button";
import { verifyAbort, verifyRun } from "@/ipc/commands";
import type { VerifyDiagnostic, VerifyState } from "@/ipc/types";
import {
  acknowledgementPrompt,
  diagnosticLabel,
  orderedDiagnostics,
  preampDisclosure,
  residualLine,
  verifyHeadline,
} from "./verifyCopy";

// The Esc binding used to live here, and that was the defect: Radix
// `TabsContent` unmounts inactive tabs, so it existed only while the Measure
// tab was selected -- which it is not by default. It now lives in App.tsx's
// `VerifyAbortBar`, outside the tabs, along with an always-visible Stop.
// The buttons below stay: they are the controls in reach when the user IS
// looking at this panel, and MS-18's acknowledgement needs its Cancel beside
// its Play.
export function VerifyPanel({ state }: { state: VerifyState }): JSX.Element {
  const [error, setError] = useState<string | null>(null);

  const acknowledgement = acknowledgementPrompt(state);

  return (
    <div className="flex h-full flex-col gap-4 p-4">
      <div>
        <h2 className="text-lg font-medium">Verify</h2>
        <p className="text-sm text-muted-foreground">{verifyHeadline(state)}</p>
      </div>

      {state.phase === "idle" && (
        <p className="text-sm text-muted-foreground">
          A verification pass re-measures your correction with the sweep played
          from a separate helper process, so it goes through the EQ instead of
          around it. It is started by the measurement wizard, which arrives in
          the next stage.
        </p>
      )}

      {state.phase === "armed" && acknowledgement && (
        <div className="flex flex-col gap-3 rounded-md border bg-muted/30 p-3">
          <p className="text-sm">{acknowledgement}</p>
          {/* MS-18: a DELIBERATE action, never a default-focused button. No
              autoFocus here, and the abort sits beside it rather than behind a
              second click. */}
          <div className="flex gap-2">
            <Button
              onClick={() => {
                setError(null);
                void verifyRun(state.device_name, state.projected_spl_db).catch((e) =>
                  setError(String(e)),
                );
              }}
              size="sm"
              variant="default"
            >
              Play the sweep
            </Button>
            <Button
              onClick={() => {
                setError(null);
                void verifyAbort().catch((e) => setError(String(e)));
              }}
              size="sm"
              variant="outline"
            >
              Cancel
            </Button>
          </div>
        </div>
      )}

      {state.phase === "running" && (
        <div className="flex items-center gap-3">
          <span className="text-sm text-muted-foreground">
            Keep the room quiet until this finishes.
          </span>
          <Button
            onClick={() => {
              setError(null);
              void verifyAbort().catch((e) => setError(String(e)));
            }}
            size="sm"
            variant="outline"
          >
            Stop (Esc)
          </Button>
        </div>
      )}

      {state.phase === "failed" && state.remedy && (
        <p className="text-sm">
          {state.remedy}
          {state.code != null && (
            <span className="ml-2 text-xs text-muted-foreground">
              (code {state.code})
            </span>
          )}
        </p>
      )}

      {state.phase === "complete" && <Report state={state} />}

      {error && <p className="text-sm text-destructive">{error}</p>}
    </div>
  );
}

/** The badge's colour for one severity. The same three-way split
 *  `diagnosticLabel` makes in words, made in weight. */
function severityTone(severity: VerifyDiagnostic["severity"]): string {
  switch (severity) {
    case "dropped":
      return "text-amber-700 dark:text-amber-400";
    case "refuse":
      return "text-destructive";
    case "warn":
      return "text-muted-foreground";
  }
}

function Report({
  state,
}: {
  state: Extract<VerifyState, { phase: "complete" }>;
}): JSX.Element {
  const { report } = state;
  const residual = residualLine(report);
  const diagnostics = orderedDiagnostics(report);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      {/* The residual against its limit. Absent rather than zero when
          `decide()` never got far enough to grade. */}
      {residual ? (
        <p className="text-sm">{residual}</p>
      ) : (
        <p className="text-sm text-muted-foreground">
          No residual was computed — the checks below stopped the grading before
          it could be.
        </p>
      )}

      {/* Mandatory, not decorative. */}
      <p className="text-sm text-muted-foreground">{preampDisclosure(report)}</p>

      {diagnostics.length > 0 && (
        <ul className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
          {diagnostics.map((d) => (
            <li
              className="rounded-md border p-2 text-sm"
              key={`${d.code}-${d.summary}`}
            >
              <div className="flex items-baseline gap-2">
                {/* Three severities, three weights. A dropped capture sits
                    between the two: it did not stop the run, so it is not
                    destructive, but it threw away one of the user's
                    measurements, so it must not read as a footnote either. */}
                <span
                  className={`text-xs font-medium uppercase ${severityTone(d.severity)}`}
                >
                  {diagnosticLabel(d)}
                </span>
                <span className="text-xs text-muted-foreground">
                  {d.summary} (code {d.code})
                </span>
              </div>
              {/* Rust's remedy, verbatim. The UI is a text field, not an
                  author. */}
              <p className="mt-1">{d.remedy}</p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
