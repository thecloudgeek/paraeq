// The AutoEQ DB browse dialog (Task 16): search the synced INDEX.md-derived
// entries (Task 9's cache-first Rust client), then apply a selected preset's
// bands + preamp through the SAME apply paths as everywhere else (`eqSetBands`
// / `engineSetPreampDb`) -- this dialog has no Rust state of its own beyond the
// client's on-disk cache.
//
// Row format is the prototype's disambiguated format (autoeq_browser.py:141):
// the same model recurs under multiple sources/rigs, so bare names cannot
// disambiguate -- rows key their React identity and their apply target on
// `entry.path`, never on `name`.
//
// Preamp clamp policy mirrors Task 14's file-import clamp EXACTLY
// (desktop/src-tauri/src/eq.rs PREAMP_MIN_DB/PREAMP_MAX_DB = -30/+10): the
// fetched preset's preamp is clamped client-side before it reaches
// `engineSetPreampDb` (which would otherwise reject an out-of-range value),
// and the toast says so when the clamp changed the value.

import { useEffect, useRef, useState } from "react";
import type { JSX } from "react";

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
  autoeqFetchPreset,
  autoeqSearch,
  autoeqSyncIndex,
  eqSetBands,
  engineSetPreampDb,
} from "@/ipc/commands";
import type { IndexEntry } from "@/ipc/types";

// Single source of truth for the preamp range mirrors
// desktop/src-tauri/src/eq.rs PREAMP_MIN_DB / PREAMP_MAX_DB.
const PREAMP_MIN_DB = -30;
const PREAMP_MAX_DB = 10;

const SEARCH_DEBOUNCE_MS = 200;

interface AutoEqBrowserProps {
  open: boolean;
  onApplied: (msg: string) => void;
  onOpenChange: (open: boolean) => void;
}

export function AutoEqBrowser({ open, onApplied, onOpenChange }: AutoEqBrowserProps): JSX.Element {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<IndexEntry[]>([]);
  const [selected, setSelected] = useState<IndexEntry | null>(null);
  const [entryCount, setEntryCount] = useState<number | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // First open with no cached index (entryCount still null) auto-syncs
  // (cache-first: `force=false` serves the on-disk cache when present, or
  // fetches once). Re-opens after a successful sync don't re-sync.
  useEffect(() => {
    if (!open || entryCount !== null) return;
    void sync(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Reset transient dialog state whenever it closes, so the next open starts
  // clean (but keeps the synced entryCount -- no need to re-sync every time).
  useEffect(() => {
    if (open) return;
    setQuery("");
    setResults([]);
    setSelected(null);
    setError(null);
  }, [open]);

  // Debounced live search.
  useEffect(() => {
    if (!open) return;
    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (query.trim() === "") {
      setResults([]);
      return;
    }
    debounceRef.current = setTimeout(() => {
      void (async () => {
        try {
          const r = await autoeqSearch(query);
          setResults(r);
        } catch (e) {
          setError(String(e));
        }
      })();
    }, SEARCH_DEBOUNCE_MS);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [query, open]);

  const sync = async (force: boolean) => {
    setSyncing(true);
    setError(null);
    try {
      const count = await autoeqSyncIndex(force);
      setEntryCount(count);
    } catch (e) {
      setError(String(e));
    } finally {
      setSyncing(false);
    }
  };

  const apply = async () => {
    if (!selected) return;
    setApplying(true);
    setError(null);
    try {
      const dto = await autoeqFetchPreset(selected.path);
      await eqSetBands(dto.bands);
      const clampedPreamp = Math.min(PREAMP_MAX_DB, Math.max(PREAMP_MIN_DB, dto.preamp_db));
      await engineSetPreampDb(clampedPreamp);
      const clampNote = clampedPreamp !== dto.preamp_db ? " (preamp clamped into range)" : "";
      onApplied(
        `Applied ${selected.name} (${dto.bands.length} band${dto.bands.length === 1 ? "" : "s"}, ` +
          `preamp ${clampedPreamp.toFixed(1)} dB${clampNote})`,
      );
      onOpenChange(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setApplying(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>Browse AutoEQ DB</DialogTitle>
        </DialogHeader>

        <div className="flex items-center gap-2">
          <Input
            autoFocus
            placeholder="Search model (e.g. HD 650)…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <Button size="sm" variant="outline" disabled={syncing} onClick={() => void sync(true)}>
            {syncing ? "Syncing…" : "Sync index"}
          </Button>
        </div>

        {entryCount !== null ? (
          <p className="text-xs text-muted-foreground">{entryCount} models indexed</p>
        ) : null}

        {error ? (
          <div
            role="alert"
            className="flex items-center justify-between gap-2 rounded-md border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            <span>{error}</span>
            <Button size="xs" variant="outline" onClick={() => void sync(entryCount === null)}>
              Retry
            </Button>
          </div>
        ) : null}

        <div className="max-h-72 overflow-y-auto rounded-md border">
          {results.length === 0 ? (
            <p className="p-3 text-sm text-muted-foreground">
              {query.trim() === "" ? "Type to search." : "No matches."}
            </p>
          ) : (
            <ul>
              {results.map((entry) => (
                <li key={entry.path}>
                  <button
                    type="button"
                    className={
                      "w-full px-3 py-2 text-left text-sm hover:bg-accent " +
                      (selected?.path === entry.path ? "bg-accent" : "")
                    }
                    onClick={() => setSelected(entry)}
                  >
                    {entry.name} — {entry.source} / {entry.rig}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button disabled={!selected || applying} onClick={() => void apply()}>
            {applying ? "Applying…" : "Apply"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
