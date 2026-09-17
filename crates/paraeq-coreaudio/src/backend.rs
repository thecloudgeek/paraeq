//! `TapBackend`: the production Core Audio implementation of the engine's
//! [`AudioBackend`] seam. Composes the crate's layers into the spike's
//! single-IOProc architecture: one IOProc on the private (real-output +
//! tap) aggregate receives tap input AND writes device output.
//!
//! Realtime discipline: everything the IOProc closure needs (de/interleave
//! scratch, the [`RtProcessor`]) is preallocated/moved in at `start`; the
//! per-callback work is layout resolve + deinterleave (with the non-finite
//! capture guard fused in) + `process_block` + interleave — no allocation,
//! no locks, no logging. HAL-supplied values are never trusted: mismatched
//! or oversized frame counts zero the output (silence) and record the skip
//! via `RtProcessor::note_skipped_block` (which keeps the callback counter
//! honest) instead of panicking.
//!
//! Teardown: [`TapBackend::stop`] encodes the FULL invariant order in one
//! place — `AudioDeviceStop` → `AudioDeviceDestroyIOProcID` (both inside
//! `IoProcHandle::stop`) → `AudioHardwareDestroyAggregateDevice` →
//! `AudioHardwareDestroyProcessTap` (both inside `TapSystem::teardown`) —
//! and the struct's field declaration order (`io` before `system`) makes a
//! bare `Drop` respect the same order as a backup.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyStreamConfiguration, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectSystemObject,
};
use paraeq_engine::backend::{AudioBackend, BackendEvent, StreamInfo};
use paraeq_engine::shared::RtProcessor;
use paraeq_engine::EngineError;

use crate::error::CaError;
use crate::ioproc::{IoBlock, IoCallback, IoProcHandle};
use crate::listeners::{ListenerEvent, PropertyListener};
use crate::properties;
use crate::tap::TapSystem;

/// Error mapping lives on the coreaudio side, keeping the engine free of
/// coreaudio types.
impl From<CaError> for EngineError {
    fn from(e: CaError) -> EngineError {
        EngineError::Backend(e.to_string())
    }
}

/// Frames of de/interleave scratch to preallocate at minimum; a callback
/// delivering more frames than scratch capacity is skipped (silence +
/// `skipped_blocks`), never allocated for.
const MIN_SCRATCH_FRAMES: usize = 4096;

/// One channel of the deinterleave copy with the capture-boundary guard
/// fused in: finite samples copy bit-exact; non-finite samples (NaN/+-inf)
/// are written as 0.0 and counted. `f32::clamp` propagates NaN, so the
/// chain's downstream clamps are not sanitizers, and ONE NaN reaching a
/// DF2T biquad poisons its state for the processor's lifetime -- taps
/// deliver f32, the format class that can encode it. Extracts `dst.len()`
/// frames of lane `channel` from the interleaved `data` (`stride` = the
/// buffer's channel count). Returns the sanitized count; the caller reports
/// it through an atomic ([`RtProcessor::note_invalid_samples`]) -- never a
/// log. Realtime-safe: no allocation, no locks, no logging.
pub fn deinterleave_sanitize_channel(
    data: &[f32],
    stride: usize,
    channel: usize,
    dst: &mut [f32],
) -> u64 {
    let mut invalid = 0;
    for (i, d) in dst.iter_mut().enumerate() {
        let v = data[i * stride + channel];
        if v.is_finite() {
            *d = v;
        } else {
            invalid += 1;
            *d = 0.0;
        }
    }
    invalid
}

/// MS-6's witness (measurement-safety `MS-6`) as a
/// cloneable handle: a live read-back of whether the backend's current tap
/// excludes ParaEQ's own process.
///
/// Why a shared cell and not a field read off `EngineState`: once a
/// [`TapBackend`] is handed to `EngineHandle::spawn` it lives inside the
/// controller thread and nothing outside can reach its [`TapSystem`]. A caller
/// takes a clone *before* that move (see [`TapBackend::exclusion_witness`]) and
/// keeps reading the same cell afterwards. `paraeq-measure` requires exactly
/// that: [`TapStatus`](paraeq_measure::TapStatus) "reports the tap's *current*
/// state, not the state at construction; the session polls it at every gate
/// that precedes emission" (`crates/paraeq-measure/src/seam.rs:96-98`). An
/// `EngineState` snapshot is published at most once per controller tick and is
/// additionally filtered by `effectively_equal` — up to a quarter second stale
/// on a safety gate.
///
/// **`false` whenever no tap is live** — before start, after stop, during a
/// device-change rebuild, and on a failed start. The invariant is not being
/// witnessed then, and a tap can come up on the very next controller tick, so
/// reporting `true` would be a claim about a topology that is not there.
///
/// Inherits the honest limit of [`TapSystem::self_excluded`]: the HAL has no
/// read-back for a tap's exclusion list, so this witnesses that we looked up
/// our own process object and passed it to the tap description — strictly
/// weaker than "the HAL is excluding us".
///
/// **This is the ONLY production `TapStatus`, and the realtime-activity witness
/// deliberately lives somewhere else.** A natural-looking extension is to hang
/// "is audio actually flowing through the tap?" off this same trait, since both
/// facts are about the tap. It does not fit: `ExclusionWitness` is one
/// `Arc<AtomicBool>` with no path to `RtShared`'s per-block counters, so the
/// activity fact would need a second implementor beside this one — two seams
/// reporting about one tap, free to disagree, which is exactly the defect
/// `self_excluded` was moved onto a live witness to avoid. The activity witness
/// is therefore a CONTROLLER fact, on
/// `paraeq_measure::EngineFacts::tap_activity`, and this trait keeps its single
/// method and its single production impl.
#[derive(Clone, Debug, Default)]
pub struct ExclusionWitness(Arc<AtomicBool>);

impl ExclusionWitness {
    /// Publish the live tap's self-exclusion. [`TapBackend`] calls this at the
    /// two points that change the answer: once the [`TapSystem`] is up, and
    /// with `false` after teardown.
    ///
    /// Public because the witness's authority never came from this method being
    /// private — [`TapStatus`](paraeq_measure::TapStatus) is a public trait any
    /// caller can implement, so a session is only as honest as the handle the
    /// wiring hands it. What makes *this* witness trustworthy is that it is the
    /// backend's own cell, obtained from [`TapBackend::exclusion_witness`].
    ///
    /// The contract that keeps it trustworthy: **`TapBackend` is the only
    /// writer.** Anything else that writes here is lying to the MS-6 gate about
    /// a live audio topology, with a full-level stimulus armed.
    pub fn set(&self, self_excluded: bool) {
        self.0.store(self_excluded, Ordering::Release);
    }
}

impl paraeq_measure::TapStatus for ExclusionWitness {
    fn self_excluded(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Production tap backend. One instance drives at most one live
/// tap/aggregate/IOProc set at a time; the engine controller starts/stops
/// it (including one stop+start renegotiation cycle when the reported
/// geometry differs from its provisional chain).
///
/// Field declaration order is the drop-order backup for the teardown
/// invariant: listeners (passive registrations) first, then `io` (stop +
/// destroy IOProc), then `system` (destroy aggregate → destroy tap). Rust
/// drops fields in declaration order, so even a bare `Drop` — without
/// [`stop`](AudioBackend::stop) — tears down in the validated sequence.
/// `exclusion` is deliberately declared FIRST, ahead of that ordered tail: it
/// owns no HAL object, so dropping it early cannot disturb the sequence, and
/// keeping it out of the tail stops a later reader from mistaking it for a
/// teardown step.
#[derive(Default)]
pub struct TapBackend {
    exclusion: ExclusionWitness,
    listeners: Vec<PropertyListener>,
    events: Option<Receiver<ListenerEvent>>,
    /// Mapped-but-unreturned events (the per-poll dedup buffer).
    pending: VecDeque<BackendEvent>,
    io: Option<IoProcHandle>,
    system: Option<TapSystem>,
}

impl TapBackend {
    pub fn new() -> TapBackend {
        TapBackend::default()
    }

    /// Take a clone of this backend's MS-6 witness.
    ///
    /// Call it **before** the backend is moved into `EngineHandle::spawn` —
    /// after that move the backend is owned by the controller thread and
    /// unreachable. The returned handle tracks the backend's start/stop for as
    /// long as the backend lives, and reads `false` once it does not.
    pub fn exclusion_witness(&self) -> ExclusionWitness {
        self.exclusion.clone()
    }

    /// The 8-step start, with partial state stashed in `self` as it is
    /// created so the caller's cleanup (`stop`) can unwind any prefix.
    fn start_inner(
        &mut self,
        processor: RtProcessor,
        requested_buffer_frames: Option<usize>,
    ) -> Result<StreamInfo, EngineError> {
        // 1. Tap + private aggregate (spike-validated composition).
        self.system = Some(TapSystem::create()?);
        let system = self.system.as_ref().expect("just stored");
        // MS-6: publish the live tap's self-exclusion as soon as there is a
        // tap. Any later failure in this function unwinds through `stop`,
        // which clears it again.
        self.exclusion.set(system.self_excluded);
        let aggregate = system.aggregate;
        let device = system.device;
        let device_uid = system.device_uid.clone();
        let sample_rate = system.format.mSampleRate;
        // The parity tap is stereo; clamp defensively — the chain (and the
        // controller's renegotiation) only supports 1 or 2 channels.
        let channels = system.format.mChannelsPerFrame.clamp(1, 2) as usize;

        // 2. Buffer-frame-size request + effective read-back (the spike's
        // latency work item). A failed set is non-fatal: the read-back
        // reports the truth and the controller renegotiates to it.
        if let Some(frames) = requested_buffer_frames {
            if let Err(e) = properties::set_buffer_frame_size(aggregate, frames as u32) {
                log::warn!("set_buffer_frame_size({frames}) failed, keeping device default: {e}");
            }
        }
        let effective = properties::buffer_frame_size(aggregate)? as usize;
        log::debug!(
            "tap backend: {channels} ch @ {sample_rate} Hz, buffer {effective} frames \
             (requested {requested_buffer_frames:?})"
        );

        // 3. Preallocated de/interleave scratch. Input (tap) and output
        // (device) layouts are independent (interleaved stereo vs split
        // mono — the DGR Labs gotcha), so a layout-blind copy is wrong and
        // scratch is mandatory. Oversize guard below skips (silence +
        // skipped_blocks) instead of allocating in-callback.
        let max_frames = effective.max(MIN_SCRATCH_FRAMES);
        let mut in_scratch: Vec<Vec<f32>> = vec![vec![0.0; max_frames]; channels];
        let mut out_scratch: Vec<Vec<f32>> = vec![vec![0.0; max_frames]; channels];

        // 4. The realtime closure. REALTIME LANE: no allocation, no locks,
        // no logging, and NEVER a panic on HAL-supplied values.
        //
        // Input-stream identity: the aggregate composes its sub-device with
        // kAudioSubDeviceInputChannelsKey: 0 (tap.rs::create_aggregate), so
        // the input buffer list below carries the tap's stream(s) only — a
        // mic-capable default output (AirPods, USB headset) contributes no
        // mic buffers and no stream-identification logic is needed.
        let mut processor = processor;
        let cb: IoCallback = Box::new(move |mut block: IoBlock<'_>| {
            // Zero ALL output buffers first (spike order, main.rs:56-60):
            // every exit path below leaves unwritten output space silent.
            for (buf, _) in block.output.buffers_mut() {
                buf.fill(0.0);
            }

            // Resolve the input layout ONCE per callback and deinterleave
            // into scratch. Frame count comes from the first buffer; every
            // buffer must agree, and it must fit the scratch.
            let mut frames: usize = 0;
            let mut first = true;
            let mut filled = 0usize; // input channels deinterleaved so far
            let mut invalid = 0u64; // non-finite input samples zeroed
            for (data, buf_channels) in block.input.buffers() {
                let bc = buf_channels.max(1);
                let f = data.len() / bc;
                if first {
                    frames = f;
                    first = false;
                } else if f != frames {
                    // Mismatched per-buffer frame counts: leave silence for
                    // this block (outputs already zeroed) and count it. The
                    // skip still counts as a callback (note_skipped_block
                    // bumps BOTH counters): a persistently-skipping session
                    // must read as NoInputDetected/InputSilent to the
                    // watchdog, not as benign Idle.
                    processor.note_skipped_block();
                    return;
                }
                if f > max_frames {
                    // Oversized block: one silent >4096-frame block is
                    // acceptable and near-unreachable; allocating is not.
                    // Counts as a callback too (see above).
                    processor.note_skipped_block();
                    return;
                }
                for c in 0..bc {
                    let ch = filled + c;
                    if ch >= channels {
                        break;
                    }
                    invalid += deinterleave_sanitize_channel(data, bc, c, &mut in_scratch[ch][..f]);
                }
                filled += bc;
            }
            // Channels the HAL did not deliver this cycle stay silent
            // (in_scratch has exactly `channels` rows).
            for ch_scratch in in_scratch.iter_mut().skip(filled) {
                ch_scratch[..frames].fill(0.0);
            }
            // Report zeroed non-finite input samples (relaxed atomic add,
            // no-op at 0). The warn! stays OFF the realtime thread: the
            // controller tick reads the counter and logs the delta.
            processor.note_invalid_samples(invalid);

            // Input views from StreamInfo.channels (matching the chain's
            // build parameter after renegotiation): a stack array sliced to
            // the channel count — never a Vec of refs.
            let empty_in: &[f32] = &[];
            let in_views: [&[f32]; 2] = [
                in_scratch.first().map_or(empty_in, |v| &v[..frames]),
                in_scratch.get(1).map_or(empty_in, |v| &v[..frames]),
            ];

            let delta = block.out_sample_time - block.in_sample_time;
            {
                let (head, tail) = out_scratch.split_at_mut(1);
                let empty_out: &mut [f32] = &mut [];
                let second: &mut [f32] = if channels > 1 {
                    &mut tail[0][..frames]
                } else {
                    empty_out
                };
                let mut out_views: [&mut [f32]; 2] = [&mut head[0][..frames], second];
                processor.process_block(&in_views[..channels], &mut out_views[..channels], delta);
            }

            // Interleave the processed scratch back out, resolving the
            // OUTPUT layout independently of the input's. Output space
            // beyond our channels/frames stays zeroed from the initial fill.
            let mut ch_offset = 0usize;
            for (data, buf_channels) in block.output.buffers_mut() {
                let bc = buf_channels.max(1);
                let out_frames = data.len() / bc;
                let n = out_frames.min(frames);
                for c in 0..bc {
                    let ch = ch_offset + c;
                    if ch >= channels {
                        break;
                    }
                    let src = &out_scratch[ch][..n];
                    for (i, s) in src.iter().enumerate() {
                        data[i * bc + c] = *s;
                    }
                }
                ch_offset += bc;
            }
        });

        // 5. Register + start the IOProc on the aggregate.
        self.io = Some(IoProcHandle::register(aggregate, cb)?);
        self.io.as_mut().expect("just stored").start()?;

        // 6. Listeners: default-output on the system object; device-alive,
        // nominal-rate, stream-config on the physical device.
        let (tx, rx) = mpsc::channel();
        self.events = Some(rx);
        self.listeners.push(PropertyListener::watch(
            kAudioObjectSystemObject as u32,
            kAudioHardwarePropertyDefaultOutputDevice,
            tx.clone(),
        )?);
        for selector in [
            kAudioDevicePropertyDeviceIsAlive,
            kAudioDevicePropertyNominalSampleRate,
            kAudioDevicePropertyStreamConfiguration,
        ] {
            self.listeners
                .push(PropertyListener::watch(device, selector, tx.clone())?);
        }

        // 7. Close the rebuild-window race: the default output may have
        // changed between TapSystem::create (which captured the then-default
        // device) and the listener registration in step 6 — a change in that
        // window fired no listener and would leave us taped to the WRONG
        // device until the next unrelated event. Re-query now that the
        // listeners are armed and queue a synthetic event so the controller
        // immediately rebuilds onto the new device.
        match properties::default_output_device() {
            Ok(current) if current != device => {
                self.pending.push_back(BackendEvent::DefaultOutputChanged);
            }
            Ok(_) => {}
            Err(e) => log::warn!("post-start default-output recheck failed: {e}"),
        }

        // 8. The effective geometry the controller negotiates against.
        Ok(StreamInfo {
            buffer_frames: effective,
            channels,
            device_uid,
            sample_rate,
        })
    }
}

/// Map a HAL property-change notification to the engine's event vocabulary.
/// Selectors are globally distinct fourccs, so the object id is not needed.
fn map_listener_event(selector: u32) -> Option<BackendEvent> {
    #[allow(non_upper_case_globals)] // Apple constant naming in patterns
    match selector {
        kAudioHardwarePropertyDefaultOutputDevice => Some(BackendEvent::DefaultOutputChanged),
        kAudioDevicePropertyDeviceIsAlive => Some(BackendEvent::DeviceDied),
        kAudioDevicePropertyNominalSampleRate | kAudioDevicePropertyStreamConfiguration => {
            Some(BackendEvent::FormatChanged)
        }
        _ => None,
    }
}

impl AudioBackend for TapBackend {
    fn start(
        &mut self,
        processor: RtProcessor,
        requested_buffer_frames: Option<usize>,
    ) -> Result<StreamInfo, EngineError> {
        if self.system.is_some() {
            return Err(EngineError::Backend(
                "TapBackend::start called while already started".into(),
            ));
        }
        let result = self.start_inner(processor, requested_buffer_frames);
        if result.is_err() {
            // Trait contract: a failed start leaves the backend fully
            // stopped. `stop` unwinds whatever prefix start_inner built.
            let _ = self.stop();
        }
        result
    }

    /// The FULL teardown invariant in one place: `IoProcHandle::stop`
    /// (AudioDeviceStop → AudioDeviceDestroyIOProcID) THEN
    /// `TapSystem::teardown` (DestroyAggregateDevice → DestroyProcessTap).
    /// Idempotent; every OSStatus failure is logged where it occurs and the
    /// teardown ones are additionally folded into the returned error
    /// (`IoProcHandle::stop` logs its statuses internally and does not
    /// return them). A dead ParaEQ must never leave the system muted.
    fn stop(&mut self) -> Result<(), EngineError> {
        // Listeners first: nothing should queue events for a system that is
        // being torn down.
        self.listeners.clear();
        self.events = None;
        self.pending.clear();

        if let Some(mut io) = self.io.take() {
            io.stop();
        }
        let errors = match self.system.take() {
            Some(mut system) => system.teardown(),
            None => Vec::new(),
        };
        // MS-6: no tap, nothing witnessed. After the teardown, so the witness
        // never reads `false` while a tap is still up. This is also the
        // failed-start unwind path (`start` calls `stop` on error) and the
        // first half of a device-change rebuild.
        self.exclusion.set(false);

        if errors.is_empty() {
            Ok(())
        } else {
            Err(EngineError::Backend(format!(
                "teardown: {}",
                errors
                    .iter()
                    .map(CaError::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )))
        }
    }

    fn poll_event(&mut self) -> Option<BackendEvent> {
        if let Some(rx) = &self.events {
            while let Ok(ev) = rx.try_recv() {
                if let Some(mapped) = map_listener_event(ev.selector) {
                    // Dedup within a poll: the controller reacts to any
                    // event with the same full rebuild, so duplicates only
                    // add churn.
                    if !self.pending.contains(&mapped) {
                        self.pending.push_back(mapped);
                    }
                }
            }
        }
        self.pending.pop_front()
    }

    /// The engine's half of the MS-6 witness, forwarding the same cell
    /// [`TapStatus`](paraeq_measure::TapStatus) reads. One source of truth:
    /// `EngineState.self_excluded` and a live measurement session can never
    /// disagree about the same tap, they only differ in staleness (the
    /// snapshot is published at most once per controller tick).
    fn self_excluded(&self) -> bool {
        paraeq_measure::TapStatus::self_excluded(&self.exclusion)
    }
}

impl Drop for TapBackend {
    fn drop(&mut self) {
        // Field drop order alone would tear down correctly (io before
        // system); running the explicit stop also logs/collects statuses.
        let _ = self.stop();
    }
}
