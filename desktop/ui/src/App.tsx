// The app shell: a five-tab layout in the prototype's exact order
// (Measure | Target | EQ | Analyzer | Profiles). Rust owns all state -- a single
// `useAppState()` here holds the latest snapshot and passes slices down; React
// keeps no forked copy. Only EQ has content this stage (a stub until Task 14);
// the rest name the stage they arrive in.

import { useEffect, useState } from "react";
import type { JSX } from "react";

import { OutputPicker } from "@/components/OutputPicker";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { engineListOutputs, engineSetDefaultOutput } from "@/ipc/commands";
import type { AppState } from "@/ipc/types";
import { useAppState } from "@/ipc/useAppState";
import { EqTab } from "@/tabs/EqTab";
import { PlaceholderTab } from "@/tabs/PlaceholderTab";
import { SetupWizard } from "@/wizard/SetupWizard";

function App() {
  const state = useAppState();

  // First-launch gate: until setup is marked complete, the wizard is the ONLY
  // surface — it is the sole gateway to audible audio (the engine stays disabled
  // until the wizard enables it). Closing the window mid-wizard just hides it;
  // re-showing resumes here because `setup_complete` is still false.
  if (state && !state.setup_complete) {
    return (
      <div className="flex h-screen flex-col">
        <SetupWizard state={state} />
      </div>
    );
  }

  return (
    <div className="flex h-screen flex-col p-4">
      <OutputHeader state={state} />
      <Tabs defaultValue="eq" className="flex h-full flex-col">
        <TabsList>
          <TabsTrigger value="measure">Measure</TabsTrigger>
          <TabsTrigger value="target">Target</TabsTrigger>
          <TabsTrigger value="eq">EQ</TabsTrigger>
          <TabsTrigger value="analyzer">Analyzer</TabsTrigger>
          <TabsTrigger value="profiles">Profiles</TabsTrigger>
        </TabsList>

        <TabsContent value="measure">
          <PlaceholderTab stage="stage 6" title="Measure" />
        </TabsContent>
        <TabsContent value="target">
          <PlaceholderTab stage="stage 5" title="Target" />
        </TabsContent>
        <TabsContent value="eq" className="min-h-0 flex-1">
          {state ? (
            <EqTab state={state} />
          ) : (
            <div className="p-4 text-sm text-muted-foreground">loading app state…</div>
          )}
        </TabsContent>
        <TabsContent value="analyzer">
          <PlaceholderTab stage="stage 6" title="Analyzer" />
        </TabsContent>
        <TabsContent value="profiles">
          <PlaceholderTab stage="stage 5" title="Profiles" />
        </TabsContent>
      </Tabs>
    </div>
  );
}

// The always-visible output-device picker: unlike the setup wizard's one-time
// device confirmation, this lets the user redirect ParaEQ's output at any
// time without leaving the app -- previously the only way was to change the
// macOS system default from outside ParaEQ. Sets the system default output
// only; the engine's own DefaultOutputChanged listener rebuilds the tap to
// follow, so this never sends an engine command directly (same contract as
// the wizard's DeviceStep in wizard/SetupWizard.tsx).
function OutputHeader({ state }: { state: AppState | null }): JSX.Element {
  const [error, setError] = useState<string | null>(null);

  // Refresh the device list once on mount; after that `state.devices` and the
  // current-device fields stay live via the app-state snapshot, no polling.
  useEffect(() => {
    void engineListOutputs().catch((e) => setError(String(e)));
  }, []);

  const pick = async (uid: string) => {
    setError(null);
    try {
      await engineSetDefaultOutput(uid);
    } catch (e) {
      setError(String(e));
    }
  };

  // Prefer the engine's LIVE stream device (what's actually playing right
  // now) over the last-set default -- they can briefly differ across a
  // device-change rebuild, and the live one is the honest "current" answer.
  // Falls back to the persisted default, then to nothing (still-loading /
  // never-set) rather than guessing.
  const currentUid = state?.engine.stream?.device_uid ?? state?.default_output_uid ?? null;

  return (
    <div className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border bg-muted/30 px-3 py-2">
      <label className="text-sm font-medium" htmlFor="header-output-device">
        Output
      </label>
      <OutputPicker
        currentUid={currentUid}
        devices={state?.devices ?? []}
        emptyMessage="No output devices found."
        id="header-output-device"
        onSelect={(uid) => void pick(uid)}
        size="sm"
        triggerClassName="w-64"
      />
      <span
        className="text-xs text-muted-foreground"
        title="Changes the macOS system output device; ParaEQ's EQ follows it automatically."
      >
        Sets the system output — the EQ follows automatically.
      </span>
      {error ? <span className="text-xs text-destructive">{error}</span> : null}
    </div>
  );
}

export default App;
