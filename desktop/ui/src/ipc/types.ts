// THE UI-side wire contract. Hand-written to mirror the Rust serde shapes
// EXACTLY -- there is no codegen. Every interface here is pinned by a Rust
// golden test; if a field name, an optionality, or the `EngineStatus` tagging
// drifts, that Rust test fails in the same PR and this file must change with it.
//
// Enforcement sources (the tripwires):
//   1. crates/paraeq-engine/tests/test_wire_format.rs
//        -- EngineState + StreamInfo + the internally-tagged `{ kind: ... }`
//           snake_case EngineStatus union.
//   2. crates/paraeq-dsp/tests/test_peq.rs (band_serde_roundtrip_and_golden_json)
//        -- the EQBand golden JSON: { filter_type, fc, gain_db, q }.
//   3. desktop/src-tauri/src/state.rs (app_state_wire_format_is_pinned, and
//        every_verify_phase_is_pinned_on_the_wire for the VerifyState union)
//        -- the full AppState envelope + EqState + OutputDeviceInfo +
//           VerifyState/VerifyReport/VerifyDiagnostic.
//
// Command arg/return shapes (IndexEntry, ResponseData, ParsedPresetDto) mirror
// desktop/src-tauri/src/{commands.rs,eq.rs,autoeq.rs}. All fields are snake_case.

/** FilterType: serde `rename_all = "snake_case"` on paraeq_dsp::peq::FilterType. */
export type FilterType = "high_shelf" | "low_shelf" | "notch" | "peaking";

export interface EQBand {
  fc: number;
  filter_type: FilterType;
  gain_db: number;
  q: number;
}

/**
 * EngineStatus: internally tagged on `kind` (snake_case). Unit variants carry
 * only `kind`; struct variants carry their extra fields alongside it.
 */
export type EngineStatus =
  | { kind: "auto_disabled_no_input"; after_ms: number }
  | { kind: "failed"; reason: string }
  | { kind: "idle"; since_ms: number }
  | { kind: "input_silent"; since_ms: number }
  | { kind: "no_input_detected"; since_ms: number }
  | { kind: "running" }
  | { kind: "starting"; since_ms: number }
  | { kind: "stopped" };

export interface StreamInfo {
  buffer_frames: number;
  channels: number;
  device_uid: string;
  sample_rate: number;
}

export interface EngineState {
  /** R1-1: the computed auto-preamp the engine is applying to the corrected
   *  path right now, in dB and never positive; `null` when no correction is
   *  running. Distinct from `EqState.preamp_db` (the user's manual trim,
   *  which rides `gain_db` and applies on both paths) -- the two compose. */
  auto_preamp_db: number | null;
  /** R1-6 / D-10: bands the engine could not design at the live rate and
   *  dropped, so the installed cascade is a SUBSET of the one that was asked
   *  for. The measurement wizard's verification gate refuses on `> 0` --
   *  grading a partial cascade against the full plan's prediction would blame
   *  the chain for our own prediction fault. Session-scoped: `0` when nothing
   *  is installed. */
  bands_dropped: number;
  bypass: boolean;
  /** R1-8: output samples the +-1.0 clamp engaged on, counted per sample per
   *  channel and retained across a teardown. Nonzero while `auto_preamp_db`
   *  is set is a BUG SIGNAL, not a user error. */
  clipped_samples: number;
  correction: string | null;
  /** R1-6: the stream rate the correction must be redesigned for, when the
   *  engine could not build it for the live stream and is running flat.
   *  `null` whenever a correction is installed (or none is retained). */
  correction_rate_mismatch: number | null;
  enabled: boolean;
  frame_mismatch_blocks: number;
  gain_db: number;
  input_peak: number;
  /** R1-8: the monotonic session maximum of the input peak. */
  input_peak_session: number;
  /** R1-2/R1-8: non-finite samples zeroed at the capture and output guards. */
  invalid_samples: number;
  latency_ms: number | null;
  /** R1-8: the maximum output sample, taken PRE-clamp, so an overshoot reads
   *  as (say) 1.99 rather than saturating at 1.0. */
  output_peak: number;
  /** R1-3: SOS rows the stability funnel replaced with the identity section.
   *  Evidence, not a refusal -- the response prediction substitutes
   *  identically, so it already models this. Session-scoped, like
   *  `bands_dropped`. */
  sections_substituted: number;
  /** wizard/1 (MS-6): whether the engine's live capture excludes ParaEQ's own
   *  audio right now. `false` with `stream != null` is the documented
   *  fail-open path -- the measurement wizard refuses and asks for a restart.
   *  `false` with `stream == null` just means nothing is running. */
  self_excluded: boolean;
  status: EngineStatus;
  stream: StreamInfo | null;
}

export interface OutputDeviceInfo {
  name: string;
  uid: string;
}

export interface EqState {
  bands: EQBand[];
  preamp_db: number;
}

/** One diagnostic the Verify panel shows (state::VerifyDiagnostic).
 *
 *  `remedy` is rendered in Rust and displayed verbatim. The decision engine is
 *  the author of this copy; the UI is a text field, not an author -- a second
 *  remedy vocabulary here is a second place for the wrong advice to be given.
 */
export interface VerifyDiagnostic {
  code: number;
  remedy: string;
  /** `paraeq_decide::Severity`, snake_cased on this wire — one value per
   *  variant, listed alphabetically per the repo convention rather than by
   *  severity (the reading order lives in `orderedDiagnostics`).
   *
   *  `"dropped"` is `Severity::RefusePosition` (ruling R-A1). It is not a
   *  refusal — the session still produced an installable correction — and it is
   *  not a warning either: one of the user's captures was thrown away and the
   *  correction was computed without it. A warning is something we noticed;
   *  this is something we did. */
  severity: "dropped" | "refuse" | "warn";
  summary: string;
}

/** What a graded verification pass produced (state::VerifyReport).
 *
 *  `gate_db` and `residual_rms_db` are nullable because `decide()` only
 *  produces them when it got far enough to grade -- a bundle refused on routing
 *  or on a dropped band carries diagnostics and no residual, and rendering 0.0
 *  there would read as a perfect result.
 */
export interface VerifyReport {
  abort_acoustic_budget_ms: number;
  diagnostics: VerifyDiagnostic[];
  gate_db: number | null;
  /** The engine's OWN armed preamp during the pass, dB. The preamp disclosure
   *  is mandatory: it is the number the user's music is now played through. */
  installed_preamp_db: number;
  /** `L_verify`, sweep-span RMS, dBFS. */
  level_dbfs: number;
  /** RMS of `residual_vs_prediction` over the authority band, as the WORST
   *  capture channel -- never the mean. */
  residual_rms_db: number | null;
  verdict: "proceed" | "proceed_with_warnings" | "refuse";
}

/** Where a verification pass is (state::VerifyState).
 *
 *  Internally tagged on `phase` (snake_case), the same convention `EngineStatus`
 *  uses for `kind`. `armed` is its own phase because MS-18 makes it one: the
 *  acknowledgement names the device and the projected SPL, and NO sweep is
 *  reachable without it.
 */
export type VerifyState =
  | { phase: "armed"; device_name: string; level_dbfs: number; projected_spl_db: number }
  | { phase: "complete"; report: VerifyReport }
  | { phase: "failed"; code: number | null; remedy: string | null; summary: string }
  | { phase: "idle" }
  | { phase: "running" };

/** What `verify_arm` returns: the MS-18 acknowledgement's content
 *  (verify::VerifyArmed). */
export interface VerifyArmed {
  device_name: string;
  level_dbfs: number;
  projected_spl_db: number;
}

/** THE snapshot the UI renders: pushed on the `app-state` event and returned
 *  by the `get_app_state` command. */
export interface AppState {
  active_profile: string | null;
  default_output_uid: string | null;
  devices: OutputDeviceInfo[];
  engine: EngineState;
  eq: EqState;
  profiles: string[];
  setup_complete: boolean;
  verification: VerifyState;
}

/** The setup wizard's three-way probe read (setup::ProbeVerdict, serialized
 *  snake_case as a bare string). `running` = capture works; `timed_out` = the
 *  TCC grant is likely missing; `still_probing` = keep waiting. */
export type ProbeVerdict = "running" | "still_probing" | "timed_out";

/** An AutoEq index row (autoeq::IndexEntry). */
export interface IndexEntry {
  name: string;
  path: string;
  rig: string;
  source: string;
}

/** The magnitude response the plot draws (eq::ResponseData). */
export interface ResponseData {
  composite: number[];
  per_band: number[][];
  sample_rate: number;
}

/** A parsed AutoEq preset (autoeq::ParsedPresetDto). */
export interface ParsedPresetDto {
  bands: EQBand[];
  preamp_db: number;
}

/** The outcome of an AutoEQ file import (eq::ImportResult). `preamp_db` is the
 *  value actually applied (clamped into [-30, +10]); `preamp_clamped` says
 *  whether the clamp changed the file's value. */
export interface ImportResult {
  band_count: number;
  preamp_clamped: boolean;
  preamp_db: number;
}
