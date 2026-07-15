// A reusable output-device Select bound to Rust-owned AppState. Renders
// `devices` as options and reflects `currentUid` as the selected value.
// Callers own the actual command (typically `engineSetDefaultOutput`) and
// their own error surface -- this component is purely presentational and
// holds no state of its own. Shared by the always-visible header picker
// (App.tsx) and the setup wizard's one-time device confirmation
// (wizard/SetupWizard.tsx) so the two never drift apart.
//
// Radix's SelectValue falls back to its `placeholder` prop whenever `value`
// doesn't match a mounted SelectItem -- which covers BOTH "nothing selected
// yet" and "selected, but the uid isn't in the current device list" (e.g. the
// engine's live stream device raced ahead of a stale/pre-refresh device
// list). We use that fallback to show the current device's name when known,
// or the raw uid when not -- so a real selection never renders as a blank
// "pick one" placeholder.

import type { JSX } from "react";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { OutputDeviceInfo } from "@/ipc/types";

interface OutputPickerProps {
  currentUid: string | null;
  devices: OutputDeviceInfo[];
  emptyMessage?: string;
  id: string;
  onSelect: (uid: string) => void;
  size?: "default" | "sm";
  triggerClassName?: string;
}

export function OutputPicker({
  currentUid,
  devices,
  emptyMessage = "No output devices found.",
  id,
  onSelect,
  size = "default",
  triggerClassName,
}: OutputPickerProps): JSX.Element {
  const current = devices.find((d) => d.uid === currentUid);
  const placeholder = current ? current.name : (currentUid ?? "Select an output device…");

  return (
    <>
      <Select value={currentUid ?? undefined} onValueChange={onSelect}>
        <SelectTrigger id={id} size={size} className={triggerClassName}>
          <SelectValue placeholder={placeholder} />
        </SelectTrigger>
        <SelectContent>
          {devices.map((d) => (
            <SelectItem key={d.uid} value={d.uid}>
              {d.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {devices.length === 0 ? (
        <p className="text-xs text-muted-foreground">{emptyMessage}</p>
      ) : null}
    </>
  );
}
