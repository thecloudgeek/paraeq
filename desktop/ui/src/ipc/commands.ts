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
  ResponseData,
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
