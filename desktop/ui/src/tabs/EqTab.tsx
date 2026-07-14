// The EQ tab: the first audible corrected-audio path. Rust owns ALL state; this
// component renders the AppState snapshot and turns every edit into a typed
// command. It holds NO forked band/preamp state — numeric cells are uncontrolled
// inputs keyed on the AppState band values (so a successful edit remounts them
// from the new snapshot, and a rejected edit snaps back via an error nonce),
// and the Type select / preamp / toolbar actions all commit immediately (the
// prototype contract: no Apply button).
//
// Layout, top to bottom (prototype parity, manual_eq_editor.py): toolbar, band
// table, preamp row, frequency plot (read-only curve here; drag handles arrive
// in Task 15), status strip.

import { useEffect, useState } from "react";
import type { JSX } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { FrequencyPlot } from "@/components/FrequencyPlot";
import {
  eqAddBand,
  eqExportAutoeq,
  eqImportAutoeq,
  eqRemoveBand,
  eqResponse,
  eqSetBands,
  engineDisable,
  engineEnable,
  engineSetBypass,
  engineSetPreampDb,
  profilesSave,
} from "@/ipc/commands";
import type { AppState, EQBand, EngineState, FilterType } from "@/ipc/types";
import { BAND_PALETTE, EQ_PLOT_RANGE, logspace } from "@/plot/FreqPlotRenderer";
import type { Trace } from "@/plot/FreqPlotRenderer";

// The plot's frequency grid (prototype parity: 512 log-spaced points 20..20k).
const PLOT_FREQS = logspace(20, 20000, 512);

// Filter-type options, in the prototype's exact order.
const FILTER_TYPES: { label: string; value: FilterType }[] = [
  { label: "Peaking", value: "peaking" },
  { label: "Low Shelf", value: "low_shelf" },
  { label: "High Shelf", value: "high_shelf" },
  { label: "Notch", value: "notch" },
];

type NumField = "fc" | "gain_db" | "q";

interface EqTabProps {
  state: AppState;
}

export function EqTab({ state }: EqTabProps): JSX.Element {
  const bands = state.eq.bands;
  const engine = state.engine;

  const [selected, setSelected] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Bumped on every rejected edit to force the offending uncontrolled input to
  // remount from unchanged AppState (value "snaps back").
  const [errNonce, setErrNonce] = useState(0);
  const [traces, setTraces] = useState<Trace[]>([]);
  const [saveOpen, setSaveOpen] = useState(false);
  const [profileName, setProfileName] = useState("");

  const sampleRate = engine.stream?.sample_rate ?? 48000;
  const bandsSig = JSON.stringify(bands);

  // Recompute the plot curves whenever the bands or the live rate change. The
  // Rust `eq_response` designs at the live stream rate (48k fallback); we pass
  // the display grid and echo `sampleRate` into the deps so a rate flip redraws.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const resp = await eqResponse(PLOT_FREQS);
        if (cancelled) return;
        const next: Trace[] = [
          { color: "#FFFFFF", dbs: resp.composite, freqs: PLOT_FREQS, width: 2.5 },
          ...resp.per_band.map((dbs, i) => ({
            color: BAND_PALETTE[i % BAND_PALETTE.length],
            dash: [2, 3],
            dbs,
            freqs: PLOT_FREQS,
            width: 1,
          })),
        ];
        setTraces(next);
      } catch {
        // A response failure leaves the previous curve; band edits surface their
        // own errors through the command path below.
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bandsSig, sampleRate]);

  const fail = (e: unknown) => {
    setError(String(e));
    setNotice(null);
    setErrNonce((n) => n + 1);
  };

  const succeed = (msg?: string) => {
    setError(null);
    setNotice(msg ?? null);
  };

  // Commit one numeric cell: parse, build the full new band list from the
  // current snapshot + this edit, and apply. Rust validates; a rejection snaps
  // the input back and shows the Rust message.
  const commitNum = async (i: number, field: NumField, raw: string) => {
    const value = Number(raw);
    if (raw.trim() === "" || !Number.isFinite(value)) {
      fail(`band ${i}: ${field} must be a finite number`);
      return;
    }
    const next: EQBand[] = bands.map((b, j) => (j === i ? { ...b, [field]: value } : b));
    try {
      await eqSetBands(next);
      succeed();
    } catch (e) {
      fail(e);
    }
  };

  const commitType = async (i: number, value: FilterType) => {
    const next: EQBand[] = bands.map((b, j) =>
      j === i ? { ...b, filter_type: value } : b,
    );
    try {
      await eqSetBands(next);
      succeed();
    } catch (e) {
      fail(e);
    }
  };

  const commitPreamp = async (raw: string) => {
    const value = Number(raw);
    if (raw.trim() === "" || !Number.isFinite(value)) {
      fail("preamp must be a finite number");
      return;
    }
    try {
      await engineSetPreampDb(value);
      succeed();
    } catch (e) {
      fail(e);
    }
  };

  const addBand = async () => {
    try {
      await eqAddBand();
      succeed();
    } catch (e) {
      fail(e);
    }
  };

  const removeBand = async () => {
    try {
      await eqRemoveBand(selected ?? undefined);
      setSelected(null);
      succeed();
    } catch (e) {
      fail(e);
    }
  };

  const importPreset = async () => {
    try {
      const path = await open({
        filters: [{ extensions: ["txt", "csv"], name: "AutoEQ preset" }],
      });
      if (typeof path !== "string") return; // cancelled (null) or multi-select
      const r = await eqImportAutoeq(path);
      succeed(
        `Imported ${r.band_count} band${r.band_count === 1 ? "" : "s"}, ` +
          `preamp ${r.preamp_db.toFixed(1)} dB${r.preamp_clamped ? " (clamped into range)" : ""}`,
      );
    } catch (e) {
      fail(e);
    }
  };

  const exportPreset = async () => {
    try {
      const path = await save({
        defaultPath: "eq_preset.txt",
        filters: [{ extensions: ["txt"], name: "AutoEQ preset" }],
      });
      if (typeof path !== "string") return; // cancelled
      await eqExportAutoeq(path);
      succeed(`Exported to ${path}`);
    } catch (e) {
      fail(e);
    }
  };

  const saveProfile = async () => {
    try {
      await profilesSave(profileName);
      setSaveOpen(false);
      setProfileName("");
      succeed(`Saved profile "${profileName.trim()}"`);
    } catch (e) {
      fail(e);
    }
  };

  const toggleBypass = async () => {
    try {
      await engineSetBypass(!engine.bypass);
      succeed();
    } catch (e) {
      fail(e);
    }
  };

  const toggleEnabled = async (enable: boolean) => {
    try {
      await (enable ? engineEnable() : engineDisable());
      succeed();
    } catch (e) {
      fail(e);
    }
  };

  return (
    <div className="flex h-full flex-col gap-3 p-2">
      {/* Toolbar */}
      <div className="flex flex-wrap items-center gap-2">
        <Button size="sm" variant="secondary" onClick={addBand}>
          Add Band
        </Button>
        <Button size="sm" variant="secondary" onClick={removeBand}>
          Remove Band
        </Button>
        <div className="flex-1" />
        <Button size="sm" variant="outline" onClick={() => setSaveOpen(true)}>
          Save Profile…
        </Button>
        <Button size="sm" variant="outline" disabled title="Coming in Task 16">
          Browse AutoEQ DB…
        </Button>
        <Button size="sm" variant="outline" onClick={importPreset}>
          Import AutoEQ…
        </Button>
        <Button size="sm" variant="outline" onClick={exportPreset}>
          Export AutoEQ…
        </Button>
      </div>

      {/* Inline error / notice banner */}
      {error ? (
        <div
          role="alert"
          className="flex items-start justify-between gap-2 rounded-md border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          <span>{error}</span>
          <button className="shrink-0 opacity-70 hover:opacity-100" onClick={() => setError(null)}>
            ✕
          </button>
        </div>
      ) : notice ? (
        <div className="flex items-start justify-between gap-2 rounded-md border bg-muted px-3 py-2 text-sm text-muted-foreground">
          <span>{notice}</span>
          <button className="shrink-0 opacity-70 hover:opacity-100" onClick={() => setNotice(null)}>
            ✕
          </button>
        </div>
      ) : null}

      {/* Band table */}
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Type</TableHead>
            <TableHead>Freq (Hz)</TableHead>
            <TableHead>Gain (dB)</TableHead>
            <TableHead>Q</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {bands.length === 0 ? (
            <TableRow>
              <TableCell className="text-muted-foreground" colSpan={4}>
                No bands — flat passthrough. Add a band to begin.
              </TableCell>
            </TableRow>
          ) : (
            bands.map((band, i) => {
              const sig = `${band.filter_type}:${band.fc}:${band.gain_db}:${band.q}:${errNonce}`;
              const isNotch = band.filter_type === "notch";
              return (
                <TableRow
                  key={i}
                  data-state={selected === i ? "selected" : undefined}
                  className="cursor-pointer"
                  onClick={() => setSelected(i)}
                >
                  <TableCell>
                    <Select
                      value={band.filter_type}
                      onValueChange={(v) => void commitType(i, v as FilterType)}
                    >
                      <SelectTrigger size="sm" className="w-32">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {FILTER_TYPES.map((t) => (
                          <SelectItem key={t.value} value={t.value}>
                            {t.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </TableCell>
                  <TableCell>
                    <Input
                      key={`fc:${sig}`}
                      type="number"
                      className="w-28"
                      min={0}
                      max={sampleRate / 2}
                      step={1}
                      defaultValue={band.fc.toFixed(1)}
                      onBlur={(e) => void commitNum(i, "fc", e.currentTarget.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") e.currentTarget.blur();
                      }}
                    />
                  </TableCell>
                  <TableCell>
                    <Input
                      key={`gain:${sig}`}
                      type="number"
                      className="w-24"
                      min={-30}
                      max={30}
                      step={0.5}
                      title={
                        isNotch
                          ? "Notch filters ignore gain in the engine; the value is kept for parity."
                          : undefined
                      }
                      defaultValue={band.gain_db.toFixed(1)}
                      onBlur={(e) => void commitNum(i, "gain_db", e.currentTarget.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") e.currentTarget.blur();
                      }}
                    />
                  </TableCell>
                  <TableCell>
                    <Input
                      key={`q:${sig}`}
                      type="number"
                      className="w-24"
                      min={0.1}
                      max={100}
                      step={0.1}
                      defaultValue={band.q.toFixed(3)}
                      onBlur={(e) => void commitNum(i, "q", e.currentTarget.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") e.currentTarget.blur();
                      }}
                    />
                  </TableCell>
                </TableRow>
              );
            })
          )}
        </TableBody>
      </Table>

      {/* Preamp row */}
      <div className="flex items-center gap-2">
        <label className="text-sm font-medium" htmlFor="preamp">
          Preamp (dB)
        </label>
        <Input
          id="preamp"
          key={`preamp:${state.eq.preamp_db}:${errNonce}`}
          type="number"
          className="w-28"
          min={-30}
          max={10}
          step={0.5}
          defaultValue={state.eq.preamp_db.toFixed(1)}
          onBlur={(e) => void commitPreamp(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
        />
      </div>

      {/* Frequency plot (read-only curve; drag handles land in Task 15) */}
      <div className="min-h-48 flex-1">
        <FrequencyPlot range={EQ_PLOT_RANGE} traces={traces} title={`${(sampleRate / 1000).toFixed(1)} kHz`} />
      </div>

      {/* Status strip */}
      <StatusStrip engine={engine} onToggleEnabled={toggleEnabled} onToggleBypass={toggleBypass} />

      {/* Save Profile dialog */}
      <Dialog open={saveOpen} onOpenChange={setSaveOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Save Profile</DialogTitle>
          </DialogHeader>
          <Input
            autoFocus
            placeholder="Profile name"
            value={profileName}
            onChange={(e) => setProfileName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void saveProfile();
            }}
          />
          <DialogFooter>
            <Button variant="outline" onClick={() => setSaveOpen(false)}>
              Cancel
            </Button>
            <Button onClick={saveProfile} disabled={profileName.trim() === ""}>
              Save
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

// The honest engine line + enable/bypass affordances. Kept in-file: it renders
// purely from EngineState and delegates its two actions to the parent.
function StatusStrip({
  engine,
  onToggleBypass,
  onToggleEnabled,
}: {
  engine: EngineState;
  onToggleBypass: () => void;
  onToggleEnabled: (enable: boolean) => void;
}): JSX.Element {
  const { label, hint } = describeStatus(engine.status);

  const parts: string[] = [label];
  if (engine.latency_ms != null) parts.push(`${engine.latency_ms.toFixed(1)} ms`);
  if (engine.stream) {
    parts.push(`${(engine.stream.sample_rate / 1000).toFixed(1)} kHz`);
    parts.push(engine.stream.channels === 2 ? "stereo" : `${engine.stream.channels} ch`);
  }
  if (engine.frame_mismatch_blocks > 0) parts.push("degraded (frame mismatch)");
  if (engine.bypass) parts.push("bypassed");

  // A user-disabled engine, an auto-disable fail-open, or a hard failure all
  // need an Enable (retry) — the engine contract's only way back on.
  const kind = engine.status.kind;
  const showEnable =
    !engine.enabled || kind === "auto_disabled_no_input" || kind === "failed";

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t pt-2 text-sm">
      <span className="font-medium">Engine: {parts.join(" · ")}</span>
      {hint ? <span className="text-muted-foreground">{hint}</span> : null}
      <div className="flex-1" />
      <Button
        size="xs"
        variant={engine.bypass ? "default" : "outline"}
        onClick={onToggleBypass}
      >
        {engine.bypass ? "Bypassed" : "Bypass"}
      </Button>
      {showEnable ? (
        <Button size="xs" onClick={() => onToggleEnabled(true)}>
          Enable
        </Button>
      ) : (
        <Button size="xs" variant="outline" onClick={() => onToggleEnabled(false)}>
          Disable
        </Button>
      )}
    </div>
  );
}

// Honest status label + optional user-facing hint, from the tagged EngineStatus.
function describeStatus(status: EngineState["status"]): { hint: string | null; label: string } {
  const permHint = "Check System Audio Recording permission, or play some audio.";
  switch (status.kind) {
    case "auto_disabled_no_input":
      return { hint: permHint, label: "Auto-disabled (no input)" };
    case "failed":
      return { hint: null, label: `Failed: ${status.reason}` };
    case "idle":
      return { hint: null, label: "Idle" };
    case "input_silent":
      return { hint: null, label: "Input silent" };
    case "no_input_detected":
      return { hint: permHint, label: "No input detected" };
    case "running":
      return { hint: null, label: "Running" };
    case "starting":
      return { hint: null, label: "Starting…" };
    case "stopped":
      return { hint: null, label: "Stopped" };
  }
}
