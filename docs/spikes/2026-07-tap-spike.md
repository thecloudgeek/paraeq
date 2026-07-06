# Tap Spike Findings — 2026-07

Spike code: `spikes/tap-spike/` (kept for reference; not a workspace member).
Protocol: docs/plans/2026-07-02-rust-port-foundation.md Task 3 Step 3.
Hardware: MacBook built-in speakers (`BuiltInSpeakerDevice`, 48 kHz), macOS 15.x, 2026-07-05.

| Check | Result |
|---|---|
| Tap capture works (TCC granted) | ✅ — but the TCC prompt **never fired** for the unsigned CLI binary (documented gotcha confirmed). Until manually granted: silent zeros, no device mute, no error anywhere. Fixed by System Settings → Privacy & Security → Screen & System Audio Recording → "System Audio Recording Only" → + add the terminal app, then fully relaunching the terminal. After grant: `peak_in ≈ 0.40`, `nonzero` climbing steadily. |
| EQ audible / bypass clean | ✅ — peaking 80 Hz +9 dB Q1 clearly changed the sound; on/off comparison via process kill confirmed the difference is the spike's EQ. |
| Volume keys + HUD native | ✅ — volume up, volume down, and mute all worked with the normal macOS HUD while the spike was processing. **The architectural goal of the tap design, confirmed.** |
| Ctrl-C teardown restores audio | ✅ — observed on multiple runs; "system audio back to normal" and playback continued unprocessed. |
| Process kill restores audio by itself | ✅ — user killed the running spike; system audio returned automatically (macOS drops the tap mute when the owning process dies). |
| Default-output switch behavior | Not exercised in the spike. Engine (stage 3) implements rebuild-on-change via `kAudioHardwarePropertyDefaultOutputDevice` listeners per the spec — unchanged. |
| Device rate + fc correctness | 48 kHz device; tap format matched (`48000 Hz, 2 ch, 32-bit float`); EQ centered correctly. |
| out→in sample delta, steady state | **2991 samples = 62.3 ms, perfectly constant** across all runs (94 callbacks/s ⇒ 512-frame IO). Constant delta = single clock domain confirmed (no drift, as designed). Above the spec's 20–30 ms budget at default buffer sizes — see work items. |
| CPU % steady state | Not formally profiled; zero glitches/dropouts across multi-minute runs with per-sample f64 biquad processing. Formal profiling deferred to the engine stage. |
| Single-IOProc output design worked (vs needing ring+second IOProc) | ✅ — one IOProc on the (real-output-sub-device + tap) aggregate received tap input **and** wrote the device output buffers. No ring buffer, no second IOProc, no resampler needed for the main path. |

## Decision

**GO for the process-tap architecture.** All load-bearing claims of the design validated on real hardware: system-mix capture, mute-at-device, audible EQ insertion, native volume keys/HUD/mute, and self-healing teardown (including hard kill). The BlackHole+aggregate fallback stays documented in docs/CONTEXT.md but is not needed.

## Surprises / knowledge for the engine implementation

- **TCC prompt does not fire for unsigned/ad-hoc CLI binaries.** `AudioHardwareCreateProcessTap` succeeds and delivers silence with no error status. The shipped, signed+notarized .app should prompt normally, but: (1) the dev-loop workflow needs the manual-grant procedure documented; (2) the engine MUST ship an amplitude watchdog + "check permission" UX, because there is no public API to query the permission and the failure mode is indistinguishable from silence.
- **~4 s of zero callbacks at startup** on the first run after permission grant (24 zero blocks) before the tap engaged; subsequent output was stable. Engine should tolerate a slow tap start (don't declare failure before ~5 s) and gate its "running" state on first nonzero input, not on `AudioDeviceStart` returning.
- **62.3 ms added latency at default buffer config** (~6 × 512 frames of aggregate-internal buffering). Work item for the engine: request smaller IO buffers (`kAudioDevicePropertyBufferFrameSize`), measure the floor, and expose the number honestly (SoundSource-style) if it can't reach the 20–30 ms budget.
- **The iqualize composition rules held exactly**: tap list must be in the aggregate's creation dictionary (with a real output sub-device present), `private:true` required for `tapautostart`, self-exclusion via `kAudioHardwarePropertyTranslatePIDToProcessObject` before creating the tap description.
- **Engine must not copy two spike shortcuts** (flagged by review, spike-only): the `&mut EqState` aliased across threads in the IOProc (engine: `&EqState` + `UnsafeCell`/atomics), and the `&'static mut` lifetime lie in `buffers_of`.
