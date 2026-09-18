// Thin typed wrappers over Tauri's `invoke`, one per Rust `#[tauri::command]`
// in desktop/src-tauri/src/commands.rs. Return types mirror the Rust `Ok`
// value; a rejected command surfaces the Rust `Err(String)` as a thrown value.
//
// Tauri arg-name convention (verified against the Tauri v2 docs, "Calling Rust
// > Passing Arguments"): the frontend passes args as a JSON object with
// camelCase keys, and Tauri maps them onto the Rust command's snake_case
// parameters. EVERY command below takes only single-word Rust params (`bypass`,
// `db`, `uid`, `bands`, `index`, `freqs`, `name`, `force`, `query`, `path`),
// which are spelled identically in camelCase and snake_case -- so no rename
// bites us here. If a future command adds a multi-word param, its key must be
// camelCase on this side (e.g. Rust `some_arg` -> JS `someArg`).

import { invoke } from "@tauri-apps/api/core";
import type {
  AppState,
  EQBand,
  ImportResult,
  IndexEntry,
  OutputDeviceInfo,
  ParsedPresetDto,
  ProbeVerdict,
  ResponseData,
  VerifyArmed,
  VerifyState,
} from "./types";

// --- AutoEq ---------------------------------------------------------------

export const autoeqFetchPreset = (path: string) =>
  invoke<ParsedPresetDto>("autoeq_fetch_preset", { path });

export const autoeqSearch = (query: string) =>
  invoke<IndexEntry[]>("autoeq_search", { query });

export const autoeqSyncIndex = (force: boolean) =>
  invoke<number>("autoeq_sync_index", { force });

// --- Engine ---------------------------------------------------------------

export const engineDisable = () => invoke<void>("engine_disable");

export const engineEnable = () => invoke<void>("engine_enable");

export const engineListOutputs = () =>
  invoke<OutputDeviceInfo[]>("engine_list_outputs");

export const engineSetBypass = (bypass: boolean) =>
  invoke<void>("engine_set_bypass", { bypass });

export const engineSetDefaultOutput = (uid: string) =>
  invoke<void>("engine_set_default_output", { uid });

export const engineSetPreampDb = (db: number) =>
  invoke<void>("engine_set_preamp_db", { db });

// --- EQ -------------------------------------------------------------------

export const eqAddBand = () => invoke<void>("eq_add_band");

export const eqExportAutoeq = (path: string) =>
  invoke<void>("eq_export_autoeq", { path });

export const eqImportAutoeq = (path: string) =>
  invoke<ImportResult>("eq_import_autoeq", { path });

export const eqRemoveBand = (index?: number) =>
  invoke<void>("eq_remove_band", { index: index ?? null });

export const eqResponse = (freqs: number[]) =>
  invoke<ResponseData>("eq_response", { freqs });

export const eqSetBands = (bands: EQBand[]) =>
  invoke<void>("eq_set_bands", { bands });

// --- Initial state fetch --------------------------------------------------

export const getAppState = () => invoke<AppState>("get_app_state");

// --- Profiles -------------------------------------------------------------

export const profilesActivate = (name: string) =>
  invoke<void>("profiles_activate", { name });

export const profilesList = () => invoke<string[]>("profiles_list");

export const profilesSave = (name: string) =>
  invoke<void>("profiles_save", { name });

// --- Setup wizard ---------------------------------------------------------

// `enable` records the user's ACTUAL terminal choice, decoupled from the probe's
// temporary enable: `true` on explicit opt-in (a verified-Running probe → Finish),
// `false` on "Skip for now" / declining (Rust then reverts the probe's enable and
// persists engine_enabled=false, so a skipping user is not re-muted next launch).
export const setupComplete = (enable: boolean) =>
  invoke<void>("setup_complete", { enable });

export const setupOpenPrivacySettings = () =>
  invoke<void>("setup_open_privacy_settings");

export const setupProbeStart = () => invoke<void>("setup_probe_start");

export const setupProbeStop = () => invoke<void>("setup_probe_stop");

// `elapsed_ms` is multi-word → camelCase key on this side (Tauri maps it to the
// Rust `elapsed_ms` param).
export const setupProbeVerdict = (elapsedMs: number) =>
  invoke<ProbeVerdict>("setup_probe_verdict", { elapsedMs });

// --- Verification ---------------------------------------------------------

// `request` is the whole arm request: the baseline bundle, the armed correction
// plan, the device, and the position to re-measure. It is passed through
// opaquely because every field of it is the decision engine's or the
// measurement crate's shape, and re-modelling those here would be a fourth
// hand-mirrored wire.
//
// Arming runs every gate that can refuse BEFORE a process exists. What comes
// back is the MS-18 acknowledgement's content: the output device's name and the
// SPL the sweep will project at the mic.
export const verifyArm = (request: unknown) =>
  invoke<VerifyArmed>("verify_arm", { request });

// `deviceName` and `acknowledgedSplDb` are multi-word → camelCase keys on this
// side (Tauri maps them to the Rust `device_name` / `acknowledged_spl_db`).
//
// `acknowledgedSplDb` must be the number the user was ACTUALLY SHOWN: the pass
// refuses a stale one, because an acknowledgement of a different level
// authorizes nothing. Resolves as soon as the worker starts; the outcome
// arrives on the `app-state` event.
export const verifyRun = (deviceName: string, acknowledgedSplDb: number) =>
  invoke<void>("verify_run", { deviceName, acknowledgedSplDb });

// Abort whatever is armed or running. The helper is asked to RAMP, never
// hard-stopped, and the full restore sequence runs whichever path gets here.
export const verifyAbort = () => invoke<void>("verify_abort");

// The verification slot, for a UI that mounted after the last event.
export const verifyReport = () => invoke<VerifyState>("verify_report");
