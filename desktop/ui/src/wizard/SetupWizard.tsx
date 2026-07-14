// The first-launch setup wizard. Rendered by App INSTEAD of the tabs while
// `state.setup_complete === false`; it is the ONLY first-launch gateway to
// audible audio. Rust owns all state — the wizard renders the AppState snapshot
// and drives typed `setup_*`/`engine_*` commands.
//
// Four steps: (1) explain; (2) confirm the output device; (3) enable + verify
// capture via the deterministic chime probe (with a live readout, Re-test, and
// a TCC guidance panel); (4) done → mark setup complete. A "Skip for now" link
// on step 3 completes setup without a verified probe (engine stays disabled),
// per the prototype's tolerant wizard.
//
// SAFETY: the probe spawns a looping `afplay` child in Rust. We stop it on every
// exit path — leaving the verify step, unmounting, completing, and skipping —
// so no chime ever outlives its step (Rust also caps the loop and kills on
// app exit; this is the belt to that suspenders).

import { useEffect, useRef, useState } from "react";
import type { JSX } from "react";

import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  engineListOutputs,
  engineSetDefaultOutput,
  setupComplete,
  setupOpenPrivacySettings,
  setupProbeStart,
  setupProbeStop,
  setupProbeVerdict,
} from "@/ipc/commands";
import type { AppState, ProbeVerdict } from "@/ipc/types";
import { isTerminalPhase, nextProbePhase, type ProbePhase } from "./probePanel";

const STEP_WELCOME = 0;
const STEP_DEVICE = 1;
const STEP_VERIFY = 2;
const STEP_DONE = 3;
const STEP_COUNT = 4;

// How often the verify step re-asks Rust for the verdict while probing.
const VERDICT_POLL_MS = 400;
// Grace before auto-advancing off the success panel (lets the user register it).
const SUCCESS_ADVANCE_MS = 1400;

interface SetupWizardProps {
  state: AppState;
}

export function SetupWizard({ state }: SetupWizardProps): JSX.Element {
  const [step, setStep] = useState(STEP_WELCOME);

  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-6 p-6">
      <StepIndicator step={step} />
      <div className="min-h-0 flex-1 overflow-auto">
        {step === STEP_WELCOME && <WelcomeStep onNext={() => setStep(STEP_DEVICE)} />}
        {step === STEP_DEVICE && (
          <DeviceStep
            state={state}
            onBack={() => setStep(STEP_WELCOME)}
            onNext={() => setStep(STEP_VERIFY)}
          />
        )}
        {step === STEP_VERIFY && (
          <VerifyStep
            state={state}
            onBack={() => setStep(STEP_DEVICE)}
            onVerified={() => setStep(STEP_DONE)}
          />
        )}
        {step === STEP_DONE && <DoneStep onBack={() => setStep(STEP_VERIFY)} />}
      </div>
    </div>
  );
}

function StepIndicator({ step }: { step: number }): JSX.Element {
  const labels = ["Welcome", "Device", "Verify", "Done"];
  return (
    <div className="flex items-center gap-2 text-xs">
      {labels.map((label, i) => (
        <div key={label} className="flex items-center gap-2">
          <span
            className={
              i === step
                ? "font-semibold text-foreground"
                : i < step
                  ? "text-muted-foreground"
                  : "text-muted-foreground/50"
            }
          >
            {i + 1}. {label}
          </span>
          {i < STEP_COUNT - 1 ? <span className="text-muted-foreground/30">→</span> : null}
        </div>
      ))}
    </div>
  );
}

function WelcomeStep({ onNext }: { onNext: () => void }): JSX.Element {
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-semibold">Welcome to ParaEQ</h1>
      <p className="text-sm text-muted-foreground">
        ParaEQ applies parametric equalization to <em>all</em> system audio on your Mac — it
        corrects your headphones or speakers in real time, system-wide, with no per-app setup.
      </p>
      <div className="rounded-md border bg-muted/40 p-4 text-sm">
        <p className="font-medium">One-time permission</p>
        <p className="mt-1 text-muted-foreground">
          To process audio, ParaEQ captures the system audio stream. When you click{" "}
          <strong>Enable EQ</strong> in a moment, macOS will ask for a one-time{" "}
          <strong>System Audio Recording</strong> permission. After you grant it, macOS may require
          you to <strong>relaunch ParaEQ</strong> for the permission to take effect.
        </p>
      </div>
      <div className="flex justify-end">
        <Button onClick={onNext}>Get started</Button>
      </div>
    </div>
  );
}

function DeviceStep({
  state,
  onBack,
  onNext,
}: {
  state: AppState;
  onBack: () => void;
  onNext: () => void;
}): JSX.Element {
  const [error, setError] = useState<string | null>(null);

  // Refresh the device list on entry so the picker reflects reality.
  useEffect(() => {
    void engineListOutputs().catch((e) => setError(String(e)));
  }, []);

  const current = state.devices.find((d) => d.uid === state.default_output_uid);

  const pick = async (uid: string) => {
    setError(null);
    try {
      await engineSetDefaultOutput(uid);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-semibold">Output device</h1>
      <p className="text-sm text-muted-foreground">
        Confirm which output ParaEQ should correct. This is your Mac’s current system output; change
        it here or later in the EQ tab.
      </p>
      <div className="flex flex-col gap-2">
        <label className="text-sm font-medium" htmlFor="wizard-device">
          System output
        </label>
        <Select value={state.default_output_uid ?? undefined} onValueChange={(v) => void pick(v)}>
          <SelectTrigger id="wizard-device" className="w-full max-w-md">
            <SelectValue placeholder={current ? current.name : "Select an output device…"} />
          </SelectTrigger>
          <SelectContent>
            {state.devices.map((d) => (
              <SelectItem key={d.uid} value={d.uid}>
                {d.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {state.devices.length === 0 ? (
          <p className="text-xs text-muted-foreground">No output devices found yet.</p>
        ) : null}
      </div>
      {error ? <p className="text-sm text-destructive">{error}</p> : null}
      <div className="flex justify-between">
        <Button variant="outline" onClick={onBack}>
          Back
        </Button>
        <Button onClick={onNext}>Next</Button>
      </div>
    </div>
  );
}

function VerifyStep({
  state,
  onBack,
  onVerified,
}: {
  state: AppState;
  onBack: () => void;
  onVerified: () => void;
}): JSX.Element {
  const [phase, setPhase] = useState<ProbePhase>("intro");
  const [verdict, setVerdict] = useState<ProbeVerdict | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The client's probe-start timestamp; Rust owns the timing decision but needs
  // the elapsed value. null ⇒ not polling.
  const [startedAt, setStartedAt] = useState<number | null>(null);

  const statusKind = state.engine.status.kind;

  // Start (or restart / Re-test) the probe: enable the engine — this clears any
  // fail-open latch, the engine contract's only way back on — then start the
  // chime loop. `setup_probe_start` also enables server-side, so this is belt +
  // suspenders; the explicit enable keeps the UI intent obvious.
  const startProbe = async () => {
    setError(null);
    setVerdict(null);
    setPhase("probing");
    try {
      await setupProbeStart();
      setStartedAt(Date.now());
    } catch (e) {
      setError(String(e));
      setPhase("failed");
    }
  };

  // Poll Rust for the verdict while probing.
  useEffect(() => {
    if (phase !== "probing" || startedAt == null) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const v = await setupProbeVerdict(Date.now() - startedAt);
        if (!cancelled) setVerdict(v);
      } catch {
        // A transient verdict-command failure is ignored; the next tick retries.
      }
    };
    void tick();
    const id = setInterval(() => void tick(), VERDICT_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [phase, startedAt]);

  // Advance the phase from the verdict + live status. On any terminal result,
  // stop the chime and end polling.
  useEffect(() => {
    if (phase !== "probing") return;
    const next = nextProbePhase(phase, verdict, statusKind);
    if (next !== phase) {
      setPhase(next);
      if (isTerminalPhase(next)) {
        setStartedAt(null);
        void setupProbeStop();
      }
    }
  }, [phase, verdict, statusKind]);

  // Auto-advance off the success panel after a short grace.
  useEffect(() => {
    if (phase !== "success") return;
    const id = setTimeout(onVerified, SUCCESS_ADVANCE_MS);
    return () => clearTimeout(id);
  }, [phase, onVerified]);

  // Stop the chime whenever we leave the verify step or unmount — no orphaned
  // playback.
  useEffect(() => {
    return () => {
      void setupProbeStop();
    };
  }, []);

  const skip = async () => {
    try {
      await setupComplete();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-semibold">Enable &amp; verify</h1>
      <p className="text-sm text-muted-foreground">
        Clicking <strong>Enable EQ</strong> starts processing and plays a short test chime to
        confirm ParaEQ can capture system audio.
      </p>

      {phase === "intro" && (
        <div className="flex flex-col gap-3">
          <Button className="self-start" onClick={() => void startProbe()}>
            Enable EQ
          </Button>
        </div>
      )}

      {phase === "probing" && (
        <div className="rounded-md border bg-muted/40 p-4 text-sm">
          <p className="font-medium">Testing capture…</p>
          <p className="mt-1 text-muted-foreground">
            Playing a test chime and listening for it in the captured stream. Engine status:{" "}
            <code>{statusKind}</code>. If macOS asked for permission, grant it — you may need to
            relaunch ParaEQ for it to take effect.
          </p>
        </div>
      )}

      {phase === "success" && (
        <div className="rounded-md border border-green-600/40 bg-green-600/10 p-4 text-sm">
          <p className="font-medium text-green-700 dark:text-green-400">Capture confirmed</p>
          <p className="mt-1 text-muted-foreground">
            ParaEQ is processing your system audio. Continuing…
          </p>
        </div>
      )}

      {phase === "guidance" && (
        <div className="flex flex-col gap-3 rounded-md border border-amber-600/40 bg-amber-600/10 p-4 text-sm">
          <p className="font-medium text-amber-700 dark:text-amber-400">
            Couldn’t confirm capture
          </p>
          <p className="text-muted-foreground">
            The test chime played but wasn’t detected in the captured stream — this almost always
            means the <strong>System Audio Recording</strong> permission is missing. Open Privacy
            settings, enable ParaEQ under <strong>Screen &amp; System Audio Recording</strong>, then{" "}
            <strong>relaunch ParaEQ</strong> and re-test.
          </p>
          <div className="flex flex-wrap gap-2">
            <Button
              variant="outline"
              onClick={() => void setupOpenPrivacySettings().catch((e) => setError(String(e)))}
            >
              Open Privacy Settings
            </Button>
            <Button onClick={() => void startProbe()}>Re-test</Button>
          </div>
        </div>
      )}

      {phase === "failed" && (
        <div className="flex flex-col gap-3 rounded-md border border-destructive/50 bg-destructive/10 p-4 text-sm">
          <p className="font-medium text-destructive">Engine failed to start</p>
          <p className="text-muted-foreground">
            {state.engine.status.kind === "failed"
              ? state.engine.status.reason
              : "The engine reported a failure."}
          </p>
          <Button className="self-start" onClick={() => void startProbe()}>
            Re-test
          </Button>
        </div>
      )}

      {error ? <p className="text-sm text-destructive">{error}</p> : null}

      <div className="mt-2 flex items-center justify-between">
        <Button variant="outline" onClick={onBack}>
          Back
        </Button>
        <button
          className="text-sm text-muted-foreground underline-offset-4 hover:underline"
          onClick={() => void skip()}
        >
          Skip for now
        </button>
      </div>
    </div>
  );
}

function DoneStep({ onBack }: { onBack: () => void }): JSX.Element {
  const [error, setError] = useState<string | null>(null);
  const finishing = useRef(false);

  const finish = async () => {
    if (finishing.current) return;
    finishing.current = true;
    try {
      await setupComplete();
    } catch (e) {
      finishing.current = false;
      setError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-semibold">You’re all set</h1>
      <p className="text-sm text-muted-foreground">
        ParaEQ is enabled and correcting your system audio. Build a correction in the EQ tab, import
        an AutoEQ preset, or save profiles — ParaEQ lives in your menu bar and restores this state
        each launch.
      </p>
      {error ? <p className="text-sm text-destructive">{error}</p> : null}
      <div className="flex justify-between">
        <Button variant="outline" onClick={onBack}>
          Back
        </Button>
        <Button onClick={() => void finish()}>Finish</Button>
      </div>
    </div>
  );
}
