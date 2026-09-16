// The state bridge. Rust owns all state; this hook renders the latest snapshot
// and holds NO forked copy. State arrives two ways, and the order matters:
//
//   1. The `app-state` event carries a full AppState on every backend change.
//   2. `get_app_state` fetches the current snapshot once on mount.
//
// Events emitted before `listen` resolves are LOST, so we register the listener
// FIRST and only then fetch the initial state. A snapshot that arrived via the
// event between listen and fetch is newer than the fetch result, so the fetch
// only fills an empty slot (`s ?? initial`) -- it never clobbers a live event.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getAppState } from "./commands";
import type { AppState } from "./types";

export function useAppState(): AppState | null {
  const [state, setState] = useState<AppState | null>(null);
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void (async () => {
      unlisten = await listen<AppState>("app-state", (e) => setState(e.payload));
      const initial = await getAppState();
      if (!cancelled) setState((s) => s ?? initial); // an already-arrived event wins over the fetch
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
  return state;
}
