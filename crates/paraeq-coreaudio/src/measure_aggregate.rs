//! MS-22: the measurement aggregate — the default output device plus a NAMED
//! input device (the mic) in ONE private aggregate, drift compensation enabled
//! on the mic sub-device (the same HAL mechanism `tap.rs::create_aggregate`
//! already enables for the tap sub-device via
//! `kAudioSubTapDriftCompensationKey`), so play and record share one clock
//! domain (safety spec § The Two-Clock Complication, "Preferred resolution").
//! A configuration that cannot be realized returns a structured
//! [`MeasureAggregateError`] — MS-22's row is "refuse rather than proceed",
//! never degrade silently.
//!
//! Capture path: one IOProc on the aggregate takes channel 0 of the mic's
//! first populated input buffer (mono — a UMIK-1 is mono; multi-channel
//! interfaces contribute their first lane), sanitizes non-finite samples to
//! 0.0 at the boundary with a count (the [`CaptureSource`] contract's
//! boundary 2, fused into the deinterleave exactly as
//! `backend.rs::deinterleave_sanitize_channel` does for the tap), and pushes
//! into a bounded SPSC ring (`rtrb`). [`CaptureSource::capture`] drains the
//! ring to `f64` off the realtime thread. On overflow the newest samples are
//! dropped and counted ([`MicCapture::counters`]) — the realtime side never
//! blocks, allocates, or logs.
//!
//! Stimulus path (Stage 5): the output route this module reserved is now
//! written. The same IOProc zero-fills the output buffers and then drains a
//! second SPSC ring into them, one mono sample per frame, honouring
//! [`StimulusRouting`]; an empty ring writes silence and counts the underrun
//! rather than holding the last sample, which would be a DC step into the
//! driver. [`StimulusOutput`] is the non-realtime half — `paraeq-measure`'s
//! [`StimulusSink`](paraeq_measure::StimulusSink), handed out once by
//! [`MicCapture::take_stimulus_sink`]. **It writes into this aggregate rather
//! than opening its own output device, and that is the whole point:** the
//! safety spec's preferred resolution to the two-clock problem is one
//! aggregate holding both the output and the mic with drift compensation, and
//! a sink on a separate stream would put play and record back on different
//! crystals and hand the gating path an untrustworthy `t = 0`. A caller that
//! never takes the sink gets the pre-Stage-5 behaviour exactly: silence into
//! the output device's mix.
//!
//! Sub-device hardening mirrors tap.rs: the OUTPUT sub-device is composed
//! with `kAudioSubDeviceInputChannelsKey: 0` so a mic-capable default output
//! (AirPods, USB headset) contributes no input streams — the IOProc's input
//! list carries the named mic and nothing else. Symmetrically the MIC
//! sub-device is composed with `kAudioSubDeviceOutputChannelsKey: 0` so a
//! mic with a monitor output contributes no output streams.
//!
//! Teardown mirrors `backend.rs::stop`'s invariant order exactly: listeners
//! first, then the IOProc (`AudioDeviceStop` → `AudioDeviceDestroyIOProcID`),
//! then `AudioHardwareDestroyAggregateDevice`; the struct's field declaration
//! order backs the same order into a bare `Drop`. Every exit path — an error
//! mid-`create`, `stop`, `Drop`, a mic dying mid-capture — runs the full
//! sequence, and every OSStatus failure is logged and collected, never
//! silently discarded.
//!
//! Test tier: none of the four oracle tiers applies — platform FFI with no
//! numerical oracle (the volume.rs precedent). Pure parts (selector
//! validation, ring drain, event mapping) get the non-hardware unit tests
//! below; HAL behaviour gets `#[ignore]` hardware tests
//! (tests/test_measure_hardware.rs).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceIsAlive, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectSystemObject, kAudioSubDeviceDriftCompensationKey, kAudioSubDeviceInputChannelsKey,
    kAudioSubDeviceOutputChannelsKey, kAudioSubDeviceUIDKey, AudioHardwareCreateAggregateDevice,
    AudioHardwareDestroyAggregateDevice, AudioObjectID,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use paraeq_measure::{CaptureSource, MeasureError, StreamFormat};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::backend::deinterleave_sanitize_channel;
use crate::error::{check, CaError};
use crate::ioproc::{IoBlock, IoCallback, IoProcHandle};
use crate::listeners::{ListenerEvent, PropertyListener};
use crate::properties;

/// Which input device joins the aggregate as the measurement mic.
#[derive(Clone, Debug, PartialEq)]
pub enum MicSelector {
    /// The system default input device at create time.
    DefaultInput,
    /// A device named by its HAL UID (`kAudioDevicePropertyDeviceUID`).
    /// An empty string is refused before any HAL call.
    Uid(String),
}

/// Creation parameters for [`MicCapture::create`].
#[derive(Clone, Debug, PartialEq)]
pub struct MeasureAggregateConfig {
    /// Drift compensation on the mic sub-device (the MS-22 default). The
    /// two-clock experiment harness sets `false` to measure the RAW skew on
    /// the mic's own clock (measurement-suite/9); the product path keeps it
    /// `true`.
    pub drift_compensation: bool,
    pub mic: MicSelector,
    /// Which output channel(s) a stimulus plays on. Only consulted when the
    /// caller takes the sink ([`MicCapture::take_stimulus_sink`]); with no
    /// sink taken the aggregate's output side stays silent, exactly as before
    /// Stage 5.
    pub routing: StimulusRouting,
}

impl Default for MeasureAggregateConfig {
    fn default() -> Self {
        MeasureAggregateConfig {
            drift_compensation: true,
            mic: MicSelector::DefaultInput,
            routing: StimulusRouting::Both,
        }
    }
}

/// MS-22's structured refusal: creation either yields a working aggregate or
/// says exactly why not — it never proceeds degraded.
#[derive(Debug, thiserror::Error)]
pub enum MeasureAggregateError {
    #[error(
        "default output device changed between aggregate build and listener arming — rebuild required"
    )]
    DefaultOutputChangedDuringBuild,
    #[error("mic device UID must not be empty")]
    EmptyMicUid,
    #[error(transparent)]
    Hal(#[from] CaError),
    #[error(
        "measurement mic '{uid}' changed or died between aggregate build and listener arming — rebuild required"
    )]
    MicChangedDuringBuild { uid: String },
    #[error("device '{uid}' has no input channels — not usable as a measurement mic")]
    MicHasNoInput { uid: String },
    #[error("mic '{uid}' is the default output device — the measurement aggregate needs a separate microphone")]
    MicIsOutputDevice { uid: String },
    #[error("no input device found for '{uid}'")]
    MicNotFound { uid: String },
}

/// Snapshot of the realtime-side counters (all monotonic since create).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CaptureCounters {
    /// IOProc invocations.
    pub callbacks: u64,
    /// Mic samples dropped because the ring was full or a block exceeded the
    /// scratch capacity. Nonzero means the capture timeline has holes — the
    /// consumer polled too slowly.
    pub dropped_samples: u64,
    /// Non-finite mic samples zeroed at the capture boundary.
    pub invalid_samples: u64,
}

#[derive(Debug, Default)]
struct InnerCounters {
    callbacks: AtomicU64,
    dropped: AtomicU64,
    invalid: AtomicU64,
}

/// Where a mono stimulus goes on the aggregate's output side.
///
/// The stimulus is always mono — the mic is mono, and one capture measures one
/// acoustic path. What varies is which output channel carries it, and a stereo
/// coupler measurement needs that: left and right are separate measurements,
/// so playing to both at once measures their sum and nothing useful.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StimulusRouting {
    /// Every output channel. Correct for a single-driver path, and for a room
    /// measurement of a system being corrected as one.
    Both,
    /// One channel index only; every other channel stays silent.
    Only(usize),
}

/// Snapshot of the stimulus side's realtime counters (monotonic since create).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StimulusCounters {
    /// Output frames the IOProc had to fill with silence because the ring was
    /// empty. Nonzero mid-sweep means the emitter fell behind the device and
    /// the stimulus has gaps — the measurement is not trustworthy.
    pub underrun_frames: u64,
    /// Stimulus samples discarded by [`StimulusOutput::stop`]'s flush. These
    /// are intentional: `stop` drops queued audio rather than flushing it at
    /// level, which is what keeps it from clicking.
    pub flushed_samples: u64,
}

#[derive(Debug, Default)]
struct StimulusInner {
    flush: std::sync::atomic::AtomicBool,
    flushed: AtomicU64,
    underrun: AtomicU64,
}

/// The measurement capture source: RAII owner of the private (output + mic)
/// aggregate, its IOProc, and its listeners. Implements
/// [`paraeq_measure::CaptureSource`].
///
/// Field declaration order is the drop-order backup for the teardown
/// invariant (the `TapBackend` precedent): listeners first, then `io`
/// (stop + destroy IOProc), then `aggregate` (destroy aggregate device).
pub struct MicCapture {
    listeners: Vec<PropertyListener>,
    events: Option<Receiver<ListenerEvent>>,
    io: Option<IoProcHandle>,
    aggregate: Option<MeasureAggregate>,
    consumer: Consumer<f32>,
    counters: Arc<InnerCounters>,
    drift_compensation: bool,
    /// Terminal failure recorded by `capture` (mic died, default output
    /// changed); every later `capture` repeats it.
    fatal: Option<String>,
    frames_per_block: usize,
    mic_nominal_rate_hz: f64,
    mic_uid: String,
    sample_rate_hz: f64,
    /// Handed out once by [`MicCapture::take_stimulus_sink`]. Held here until
    /// taken so a caller that only captures never has to know it exists.
    stimulus: Option<StimulusOutput>,
    torn_down: bool,
}

impl MicCapture {
    /// Resolve devices, refuse structurally-bad configurations, create the
    /// aggregate, and start capturing. Any failure destroys everything built
    /// so far (the partially-built resources are RAII and locals drop in
    /// reverse creation order — listener, IOProc, aggregate — so `?` unwinds
    /// in the invariant order) and returns a structured error.
    pub fn create(config: MeasureAggregateConfig) -> Result<MicCapture, MeasureAggregateError> {
        // 1. Selector validation BEFORE any HAL call (the volume.rs shape).
        if let MicSelector::Uid(uid) = &config.mic {
            if uid.is_empty() {
                return Err(MeasureAggregateError::EmptyMicUid);
            }
        }

        // 2. Resolve both devices; refuse rather than guess.
        let out_dev = properties::default_output_device()?;
        let out_uid = properties::device_uid(out_dev)?;
        let mic_dev = match &config.mic {
            MicSelector::DefaultInput => properties::default_input_device()?,
            MicSelector::Uid(uid) => properties::translate_uid_to_device(uid)?,
        };
        if mic_dev == 0 {
            return Err(MeasureAggregateError::MicNotFound {
                uid: match &config.mic {
                    MicSelector::DefaultInput => "<default input>".to_string(),
                    MicSelector::Uid(uid) => uid.clone(),
                },
            });
        }
        let mic_uid = properties::device_uid(mic_dev)?;
        if mic_dev == out_dev {
            return Err(MeasureAggregateError::MicIsOutputDevice { uid: mic_uid });
        }
        if properties::input_stream_channel_count(mic_dev)? == 0 {
            return Err(MeasureAggregateError::MicHasNoInput { uid: mic_uid });
        }
        let mic_nominal_rate_hz = properties::nominal_sample_rate(mic_dev)?;

        // 3. The aggregate itself, wrapped in RAII immediately so every later
        // `?` destroys it.
        let id = create_measure_aggregate(
            &CFString::from_str(&out_uid),
            &CFString::from_str(&mic_uid),
            config.drift_compensation,
        )?;
        let mut aggregate = MeasureAggregate {
            id,
            torn_down: false,
        };
        let sample_rate_hz = match properties::nominal_sample_rate(aggregate.id) {
            Ok(rate) => rate,
            Err(e) => {
                for err in aggregate.teardown() {
                    log::error!("create cleanup: {err}");
                }
                return Err(e.into());
            }
        };
        if (sample_rate_hz - mic_nominal_rate_hz).abs() > 0.5 {
            // The spec's OPEN rate-reconciliation item, surfaced as data,
            // not silently absorbed: with drift compensation the HAL
            // resamples, without it the capture is on the mic clock at the
            // mic rate. Either way the session must know.
            log::warn!(
                "measure aggregate at {sample_rate_hz} Hz but mic '{mic_uid}' is nominally \
                 {mic_nominal_rate_hz} Hz (rate reconciliation — see measurement-safety spec)"
            );
        }
        let frames_per_block = match properties::buffer_frame_size(aggregate.id) {
            Ok(frames) => frames as usize,
            Err(e) => {
                for err in aggregate.teardown() {
                    log::error!("create cleanup: {err}");
                }
                return Err(e.into());
            }
        };

        // 4. Bounded SPSC ring + counters + the realtime capture closure.
        let ring_capacity = (sample_rate_hz as usize).max(MIN_RING_RATE) * RING_SECONDS;
        let (producer, consumer) = RingBuffer::<f32>::new(ring_capacity);
        let counters = Arc::new(InnerCounters::default());
        // The stimulus ring is built unconditionally so the IOProc's output
        // half is fixed at registration time — the HAL callback cannot grow a
        // new branch later, and a caller that never takes the sink simply
        // never pushes, leaving the aggregate silent exactly as before.
        let (stimulus_producer, stimulus_consumer) = RingBuffer::<f32>::new(ring_capacity);
        let stimulus_inner = Arc::new(StimulusInner::default());
        let cb = capture_callback(
            producer,
            Arc::clone(&counters),
            frames_per_block.max(MIN_SCRATCH_FRAMES),
            Some(StimulusPath {
                consumer: stimulus_consumer,
                inner: Arc::clone(&stimulus_inner),
                routing: config.routing,
            }),
        );

        // 5. Register + start the IOProc on the aggregate. From here on the
        // locals `io` (drops: stop -> destroy IOProc) then `aggregate`
        // (drops: destroy aggregate) unwind in invariant order on any `?`.
        let mut io = IoProcHandle::register(aggregate.id, cb)?;
        io.start()?;

        // 6. Listeners: a mic dying or the default output changing
        // mid-measurement must refuse the capture, not degrade it.
        let (tx, rx) = mpsc::channel();
        let listeners = vec![
            PropertyListener::watch(
                kAudioObjectSystemObject as u32,
                kAudioHardwarePropertyDefaultOutputDevice,
                tx.clone(),
            )?,
            PropertyListener::watch(mic_dev, kAudioDevicePropertyDeviceIsAlive, tx)?,
        ];

        // 7. Close the rebuild-window race (the backend.rs step-7 precedent):
        // the default output — or the mic — may have changed between resolving
        // the devices (step 2) / building the aggregate against them (step 3)
        // and arming the listeners (step 6). A change in that window fired no
        // listener and would leave this capture bound to the WRONG output route
        // (or a dead mic) with no event ever arriving. Re-query now that the
        // listeners are armed; on any mismatch REFUSE (the MS-22 posture) so the
        // caller rebuilds — mirroring backend.rs, which queues a synthetic
        // rebuild event, except that `create` returns a Result so the rebuild
        // signal is a structured error. The still-live locals drop in the
        // invariant teardown order on the early return — `listeners` first, then
        // `io` (AudioDeviceStop → destroy IOProc), then `aggregate` (destroy
        // aggregate device) — the same teardown every other create error path
        // runs, so nothing (aggregate / IOProc / listeners) leaks.
        let current_out = properties::default_output_device()?;
        if current_out != out_dev {
            return Err(MeasureAggregateError::DefaultOutputChangedDuringBuild);
        }
        // The mic's UID must still resolve to the SAME device: a mic that died
        // or was unplugged in the window fails to resolve (0) or maps elsewhere
        // — the same condition the mic-is-alive listener armed above watches.
        if properties::translate_uid_to_device(&mic_uid)? != mic_dev {
            return Err(MeasureAggregateError::MicChangedDuringBuild {
                uid: mic_uid.clone(),
            });
        }

        log::debug!(
            "measure aggregate up: id={} mic='{mic_uid}' ({mic_nominal_rate_hz} Hz) \
             out='{out_uid}' rate={sample_rate_hz} drift={}",
            aggregate.id,
            config.drift_compensation
        );

        Ok(MicCapture {
            listeners,
            events: Some(rx),
            io: Some(io),
            aggregate: Some(aggregate),
            consumer,
            counters,
            drift_compensation: config.drift_compensation,
            fatal: None,
            frames_per_block,
            mic_nominal_rate_hz,
            mic_uid,
            sample_rate_hz,
            stimulus: Some(StimulusOutput {
                capacity: ring_capacity,
                format: StreamFormat {
                    channels: 1,
                    frames_per_block,
                    sample_rate_hz,
                },
                inner: stimulus_inner,
                producer: stimulus_producer,
                stopped: false,
            }),
            torn_down: false,
        })
    }

    /// Take the one-clock [`StimulusSink`](paraeq_measure::StimulusSink) for
    /// this aggregate. `None` on the second call.
    ///
    /// Once, by construction: two sinks on one SPSC ring would interleave two
    /// stimuli into the same output stream at unknown relative timing, and the
    /// ring is single-producer regardless. The session takes it at `begin` and
    /// owns it for the run.
    pub fn take_stimulus_sink(&mut self) -> Option<StimulusOutput> {
        self.stimulus.take()
    }

    /// Snapshot the realtime-side counters.
    pub fn counters(&self) -> CaptureCounters {
        CaptureCounters {
            callbacks: self.counters.callbacks.load(Ordering::Relaxed),
            dropped_samples: self.counters.dropped.load(Ordering::Relaxed),
            invalid_samples: self.counters.invalid.load(Ordering::Relaxed),
        }
    }

    /// Whether the mic sub-device was composed with drift compensation.
    pub fn drift_compensation(&self) -> bool {
        self.drift_compensation
    }

    /// The mic's own nominal sample rate — compare against
    /// [`Self::sample_rate_hz`] to see the rate-reconciliation situation.
    pub fn mic_nominal_rate_hz(&self) -> f64 {
        self.mic_nominal_rate_hz
    }

    /// The captured mic's HAL UID.
    pub fn mic_uid(&self) -> &str {
        &self.mic_uid
    }

    /// The aggregate's nominal sample rate — the clock domain of every
    /// captured sample when drift compensation is on.
    pub fn sample_rate_hz(&self) -> f64 {
        self.sample_rate_hz
    }

    /// The full teardown in invariant order; idempotent; errors collected.
    /// Order (the `TapBackend::stop` precedent): listeners first (nothing
    /// should queue events for a system being torn down), then the IOProc
    /// (`AudioDeviceStop` → `AudioDeviceDestroyIOProcID`, inside
    /// `IoProcHandle::stop`), then `AudioHardwareDestroyAggregateDevice`.
    fn stop_inner(&mut self) -> Vec<CaError> {
        if self.torn_down {
            return Vec::new();
        }
        self.torn_down = true;
        self.listeners.clear();
        self.events = None;
        if let Some(mut io) = self.io.take() {
            io.stop();
        }
        match self.aggregate.take() {
            Some(mut aggregate) => aggregate.teardown(),
            None => Vec::new(),
        }
    }
}

impl CaptureSource for MicCapture {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: self.frames_per_block,
            sample_rate_hz: self.sample_rate_hz,
        }
    }

    /// Drain captured mic samples into `block` (short reads are legal — the
    /// session decides what an underrun means). A fatal event — the mic died,
    /// the default output changed — tears the whole system down and returns
    /// an error, on this call and every later one: MS-22 refuses rather than
    /// hands back a silence-padded capture.
    fn capture(&mut self, block: &mut [f64]) -> Result<usize, MeasureError> {
        if let Some(msg) = &self.fatal {
            return Err(MeasureError::Capture(msg.clone()));
        }
        if self.torn_down {
            return Err(MeasureError::Capture("capture source is stopped".into()));
        }
        let mut fatal: Option<String> = None;
        if let Some(rx) = &self.events {
            while let Ok(event) = rx.try_recv() {
                if let Some(msg) = map_capture_event(event.selector, &self.mic_uid) {
                    fatal = Some(msg);
                    break;
                }
            }
        }
        if let Some(msg) = fatal {
            // Full teardown NOW: a dead mic must not leave a live aggregate
            // behind while the session decides what to do.
            let _ = self.stop_inner();
            self.fatal = Some(msg.clone());
            return Err(MeasureError::Capture(msg));
        }
        Ok(drain_ring(&mut self.consumer, block))
    }

    /// Full teardown; idempotent (the seam contract). Teardown OSStatus
    /// failures are logged where they occur AND folded into the returned
    /// error, the `TapBackend::stop` shape.
    fn stop(&mut self) -> Result<(), MeasureError> {
        let errors = self.stop_inner();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(MeasureError::Capture(format!(
                "teardown: {}",
                errors
                    .iter()
                    .map(CaError::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )))
        }
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        // Errors are logged inside stop_inner; nothing more a Drop can do.
        let _ = self.stop_inner();
    }
}

/// RAII owner of the aggregate device id (the `TapSystem` shape minus the
/// tap). Destroy is guarded, logged, and collected — never silent.
struct MeasureAggregate {
    id: AudioObjectID,
    torn_down: bool,
}

impl MeasureAggregate {
    fn teardown(&mut self) -> Vec<CaError> {
        if self.torn_down {
            return Vec::new();
        }
        self.torn_down = true;
        let mut errors = Vec::new();
        // SAFETY: `id` was created by AudioHardwareCreateAggregateDevice and
        // is destroyed at most once (guarded by `torn_down`).
        let status = unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
        if let Err(e) = check(status, "AudioHardwareDestroyAggregateDevice (measure)") {
            log::error!("measure aggregate teardown: {e}");
            errors.push(e);
        }
        errors
    }
}

impl Drop for MeasureAggregate {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

/// Frames of deinterleave scratch to preallocate at minimum (the backend.rs
/// constant): a callback delivering more frames than scratch capacity has its
/// tail dropped and counted, never allocated for.
const MIN_SCRATCH_FRAMES: usize = 4096;

/// Ring depth in seconds of audio. Bounds memory (16 s @ 48 kHz mono f32 =
/// 3 MB) while giving a polling consumer generous slack — a 5.5 s sweep
/// capture fits whole even if the consumer stalls.
const RING_SECONDS: usize = 16;

/// Floor for the ring-sizing rate, in case the HAL reports a nonsense
/// nominal rate.
const MIN_RING_RATE: usize = 8_000;

/// Build the realtime capture closure. REALTIME LANE: no allocation, no
/// locks, no logging — counts go through atomics, samples through the SPSC
/// ring, and every non-finite sample is zeroed at this boundary.
fn capture_callback(
    mut producer: Producer<f32>,
    counters: Arc<InnerCounters>,
    max_frames: usize,
    stimulus: Option<StimulusPath>,
) -> IoCallback {
    let mut scratch = vec![0.0f32; max_frames];
    let mut stimulus = stimulus;
    Box::new(move |mut block: IoBlock<'_>| {
        // Zero ALL output buffers first (the backend.rs order): unwritten HAL
        // output space must never leak stale samples, and with no stimulus
        // armed the measurement aggregate contributes silence to the output
        // device's mix.
        for (buf, _) in block.output.buffers_mut() {
            buf.fill(0.0);
        }
        if let Some(path) = stimulus.as_mut() {
            path.fill(&mut block.output);
        }

        counters.callbacks.fetch_add(1, Ordering::Relaxed);

        // The input list carries the mic's stream(s) only: the output
        // sub-device is composed with input-channels: 0 (see
        // create_measure_aggregate). Capture is MONO — channel 0 of the
        // first populated buffer (module docs).
        let mut dropped = 0u64;
        let mut invalid = 0u64;
        for (data, buf_channels) in block.input.buffers() {
            let bc = buf_channels.max(1);
            let frames = data.len() / bc;
            if frames == 0 {
                continue;
            }
            // Oversized block: keep what fits in scratch, count the tail as
            // dropped. (backend.rs skips whole blocks because its OUTPUT
            // must stay consistent; a capture path wants maximal data plus
            // a visible drop count.)
            let n = frames.min(scratch.len());
            dropped += (frames - n) as u64;
            invalid += deinterleave_sanitize_channel(data, bc, 0, &mut scratch[..n]);
            for &v in &scratch[..n] {
                if producer.push(v).is_err() {
                    dropped += 1;
                }
            }
            break;
        }
        counters.dropped.fetch_add(dropped, Ordering::Relaxed);
        counters.invalid.fetch_add(invalid, Ordering::Relaxed);
    })
}

/// The realtime half of the stimulus path: drains the mono stimulus ring into
/// the aggregate's output buffers, one frame at a time, honouring the routing.
///
/// Lives on the realtime thread inside the IOProc closure. No locks, no
/// allocation, no logging — the same contract `capture_callback`'s input half
/// keeps.
struct StimulusPath {
    consumer: Consumer<f32>,
    inner: Arc<StimulusInner>,
    routing: StimulusRouting,
}

impl StimulusPath {
    /// Write one IO cycle's worth of stimulus.
    ///
    /// The output list may arrive as one interleaved N-channel buffer or as N
    /// mono buffers (the DGR Labs layout gotcha `ioproc.rs` documents), and
    /// this handles both: one mono sample is pulled per **frame** and written
    /// to that frame's routed channels. Pulling per sample instead would play
    /// the stimulus at N× speed on an interleaved device.
    ///
    /// Only the FIRST populated output buffer is driven. A HAL output list can
    /// carry more than one stream, and writing the same stimulus into all of
    /// them would play it twice into the same acoustic path at unknown
    /// relative gain.
    fn fill(&mut self, output: &mut crate::ioproc::BufferListMut<'_>) {
        // `stop` flushes rather than fading: the session ramps the stimulus to
        // zero BEFORE tearing down (MS-14), so by the time this fires there is
        // nothing left at level to click.
        if self.inner.flush.swap(false, Ordering::Relaxed) {
            let mut flushed = 0u64;
            while self.consumer.pop().is_ok() {
                flushed += 1;
            }
            self.inner.flushed.fetch_add(flushed, Ordering::Relaxed);
            return;
        }
        let mut underrun = 0u64;
        for (buf, channels) in output.buffers_mut() {
            let channels = channels.max(1);
            let frames = buf.len() / channels;
            if frames == 0 {
                continue;
            }
            for frame in 0..frames {
                let Ok(sample) = self.consumer.pop() else {
                    // Silence, not the previous sample: a held sample is a DC
                    // step into the driver and a defect in the measurement.
                    underrun += (frames - frame) as u64;
                    break;
                };
                match self.routing {
                    StimulusRouting::Both => {
                        for ch in 0..channels {
                            buf[frame * channels + ch] = sample;
                        }
                    }
                    StimulusRouting::Only(ch) if ch < channels => {
                        buf[frame * channels + ch] = sample;
                    }
                    // A routing the device cannot honour plays silence rather
                    // than falling back to channel 0: silence is an obvious
                    // failure, a wrong channel is a plausible wrong answer.
                    StimulusRouting::Only(_) => {}
                }
            }
            break;
        }
        if underrun > 0 {
            self.inner.underrun.fetch_add(underrun, Ordering::Relaxed);
        }
    }
}

/// The non-realtime half of the stimulus path: `paraeq-measure`'s
/// [`StimulusSink`](paraeq_measure::StimulusSink), writing into the SAME
/// aggregate the mic is captured from.
///
/// **One clock domain, which is the entire point.** The safety spec's
/// preferred resolution to the two-clock problem is an aggregate containing
/// both the output device and the mic with drift compensation enabled; a sink
/// on a *separate* output stream would put play and record back on different
/// crystals and hand the gating path an untrustworthy `t = 0`. So this writes
/// into the aggregate's output side rather than opening its own device — the
/// route `measure_aggregate`'s module header reserved for it.
///
/// Obtained from [`MicCapture::take_stimulus_sink`], once. Dropping it leaves
/// the aggregate outputting silence.
pub struct StimulusOutput {
    capacity: usize,
    format: StreamFormat,
    inner: Arc<StimulusInner>,
    producer: Producer<f32>,
    stopped: bool,
}

impl StimulusOutput {
    /// Snapshot the realtime-side counters.
    pub fn counters(&self) -> StimulusCounters {
        StimulusCounters {
            flushed_samples: self.inner.flushed.load(Ordering::Relaxed),
            underrun_frames: self.inner.underrun.load(Ordering::Relaxed),
        }
    }

    /// Frames currently queued ahead of the device.
    pub fn queued_frames(&self) -> usize {
        self.capacity - self.producer.slots()
    }
}

impl paraeq_measure::StimulusSink for StimulusOutput {
    fn format(&self) -> StreamFormat {
        self.format.clone()
    }

    /// Push one block and **wait for the device to take it** — the seam's
    /// pacing contract, and the thing MS-14's abort model depends on.
    ///
    /// `seam.rs` states it directly: a sink that accepted the whole sweep into
    /// a deep queue and returned immediately would make the session's
    /// poll-the-abort-handle-per-block cadence fictional, and the 5 ms ramp
    /// would arrive after seconds of already-queued full-level audio. So this
    /// returns only once the queue is back within about one block.
    ///
    /// The `level` argument is provenance, not a gain: the buffer arrives
    /// already scaled by `AssembledStimulus::emit_to`, and multiplying by it
    /// here would double-apply the solved level.
    fn emit(
        &mut self,
        block: &[f64],
        level: paraeq_measure::SweepLevel,
    ) -> Result<(), MeasureError> {
        let _ = level;
        if self.stopped {
            return Err(MeasureError::Sink("stimulus sink is stopped".to_owned()));
        }
        let block_frames = self.format.frames_per_block.max(1);
        let mut written = 0usize;
        while written < block.len() {
            if self.producer.is_abandoned() {
                return Err(MeasureError::Sink(
                    "measurement aggregate went away mid-emit".to_owned(),
                ));
            }
            let mut pushed_any = false;
            while written < block.len() {
                // The realtime side never blocks, so a full ring means the
                // device has not consumed yet — wait rather than drop. Dropping
                // would put a hole in the stimulus, which is a measurement
                // defect, not a glitch.
                if self.producer.push(block[written] as f32).is_err() {
                    break;
                }
                written += 1;
                pushed_any = true;
            }
            if written >= block.len() {
                break;
            }
            if !pushed_any {
                std::thread::sleep(block_sleep(&self.format));
            }
        }
        // Pace: hold until the device is within about one block of caught up.
        while self.queued_frames() > block_frames {
            if self.producer.is_abandoned() {
                return Err(MeasureError::Sink(
                    "measurement aggregate went away mid-emit".to_owned(),
                ));
            }
            std::thread::sleep(block_sleep(&self.format));
        }
        Ok(())
    }

    /// Idempotent teardown. Arms the realtime flush so queued audio is
    /// **dropped, not played out at level**, and refuses further emission.
    ///
    /// The stimulus is already at zero when a session calls this — `sweep`
    /// ramps before terminating (MS-14) and every other caller has nothing
    /// playing — so dropping the queue cannot click.
    fn stop(&mut self) -> Result<(), MeasureError> {
        self.stopped = true;
        self.inner.flush.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// How long to sleep while waiting on the device: a quarter of a block, so the
/// wait costs at most ~25% of a block's latency and never busy-spins a core.
fn block_sleep(format: &StreamFormat) -> std::time::Duration {
    let rate = if format.sample_rate_hz > 0.0 {
        format.sample_rate_hz
    } else {
        48_000.0
    };
    let seconds = format.frames_per_block.max(1) as f64 / rate / 4.0;
    std::time::Duration::from_secs_f64(seconds.clamp(1e-4, 0.05))
}

/// Compose the private measurement aggregate: `{ output (inputs: 0), mic
/// (outputs: 0, drift per config) }`, main sub-device (clock master) = the
/// output device. Mirrors `tap.rs::create_aggregate` key-for-key, minus the
/// tap list.
fn create_measure_aggregate(
    out_uid: &CFString,
    mic_uid: &CFString,
    drift_compensation: bool,
) -> Result<AudioObjectID, CaError> {
    let key = |c: &std::ffi::CStr| CFString::from_str(c.to_str().unwrap());

    // Output sub-device entry: { channels-in: 0, uid: <output UID> }.
    // input-channels 0 excludes a mic-capable default output's own input
    // streams (AirPods / USB headsets) so the IOProc's input list carries
    // ONLY the named mic — the same hardening tap.rs applies, for the same
    // reason (stream identity + no spurious TCC surface).
    let sub_out: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[
            &*key(kAudioSubDeviceInputChannelsKey),
            &*key(kAudioSubDeviceUIDKey),
        ],
        &[
            CFNumber::new_i32(0).as_ref() as &CFType, // CFNumber, NOT CFBoolean
            out_uid.as_ref() as &CFType,
        ],
    );
    // Mic sub-device entry: { drift: 0|1, channels-out: 0, uid: <mic UID> }.
    // Unlike the sub-TAP drift key (CFBoolean in tap.rs), the sub-DEVICE
    // drift key is documented as a CFNumber (0 = no compensation) —
    // AudioHardwareBase.h, kAudioSubDeviceDriftCompensationKey. The output
    // device is the clock master (MainSubDevice below), so compensation
    // belongs on the mic. output-channels 0 symmetrically excludes a mic's
    // monitor output streams.
    let sub_mic: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[
            &*key(kAudioSubDeviceDriftCompensationKey),
            &*key(kAudioSubDeviceOutputChannelsKey),
            &*key(kAudioSubDeviceUIDKey),
        ],
        &[
            CFNumber::new_i32(i32::from(drift_compensation)).as_ref() as &CFType,
            CFNumber::new_i32(0).as_ref() as &CFType,
            mic_uid.as_ref() as &CFType,
        ],
    );
    let sub_devs =
        CFArray::from_objects(&[sub_out.as_ref() as &CFType, sub_mic.as_ref() as &CFType]);
    let agg_uid = CFString::from_str(&format!("com.paraeq.measure.{}", std::process::id()));
    let name = CFString::from_str("paraeq-measure-agg");

    let dict: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[
            &*key(kAudioAggregateDeviceNameKey),
            &*key(kAudioAggregateDeviceUIDKey),
            &*key(kAudioAggregateDeviceMainSubDeviceKey),
            &*key(kAudioAggregateDeviceIsPrivateKey),
            &*key(kAudioAggregateDeviceIsStackedKey),
            &*key(kAudioAggregateDeviceSubDeviceListKey),
        ],
        &[
            name.as_ref() as &CFType,
            agg_uid.as_ref() as &CFType,
            out_uid.as_ref() as &CFType, // clock master: the output device
            CFBoolean::new(true).as_ref() as &CFType, // private
            CFBoolean::new(false).as_ref() as &CFType, // stacked
            sub_devs.as_ref() as &CFType,
        ],
    );

    let mut agg: AudioObjectID = 0;
    // AudioHardwareCreateAggregateDevice takes the untyped `&CFDictionary`
    // (default Opaque/Opaque params), not `CFDictionary<CFString, CFType>`.
    let dict_ref: &CFDictionary = dict.as_ref();
    // SAFETY: `dict_ref` is a valid composition dictionary kept alive across
    // the call; `agg` is a live out-param.
    let status = unsafe { AudioHardwareCreateAggregateDevice(dict_ref, (&mut agg).into()) };
    check(
        status,
        "AudioHardwareCreateAggregateDevice (measure: MS-22 refuses on failure)",
    )?;
    Ok(agg)
}

/// Map a listener event to the fatal-refusal message it implies for a running
/// capture, if any. Pure — unit-tested without a HAL.
fn map_capture_event(selector: u32, mic_uid: &str) -> Option<String> {
    #[allow(non_upper_case_globals)] // Apple constant naming in patterns
    match selector {
        kAudioDevicePropertyDeviceIsAlive => {
            Some(format!("mic device '{mic_uid}' died mid-capture"))
        }
        kAudioHardwarePropertyDefaultOutputDevice => Some(
            "default output device changed mid-capture — the aggregate no longer matches the \
             playback route"
                .to_string(),
        ),
        _ => None,
    }
}

/// Drain up to `block.len()` mic samples from the ring, widening to `f64`.
/// Returns the frames written; 0 is a legal short read. Pure with respect to
/// the HAL — unit-tested against a plain `rtrb` ring.
pub fn drain_ring(consumer: &mut Consumer<f32>, block: &mut [f64]) -> usize {
    let mut written = 0;
    while written < block.len() {
        match consumer.pop() {
            Ok(v) => {
                block[written] = f64::from(v);
                written += 1;
            }
            Err(_) => break,
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use objc2_core_audio_types::{AudioBuffer, AudioBufferList};

    use super::*;
    use crate::ioproc::{BufferList, BufferListMut};

    /// One `AudioBuffer` over live f32 storage (the tests/test_buffers.rs
    /// helper): exposes `storage.len()` samples across `channels` channels.
    fn buffer_over(storage: &mut [f32], channels: u32) -> AudioBuffer {
        AudioBuffer {
            mNumberChannels: channels,
            mDataByteSize: (storage.len() * 4) as u32,
            mData: storage.as_mut_ptr().cast::<c_void>(),
        }
    }

    /// Invoke the realtime capture closure over a hand-built input/output
    /// buffer-list pair — no HAL involved (the tests/test_buffers.rs
    /// technique). `input` and `output` must each describe live, aligned f32
    /// storage that outlives this call, and their storages must not overlap.
    fn run_block(cb: &mut IoCallback, input: &mut AudioBufferList, output: &mut AudioBufferList) {
        // SAFETY: both lists point at live, aligned, exclusively-owned f32
        // storage that outlives `block`; each holds one in-bounds buffer and
        // the input/output storages are distinct allocations (non-overlapping).
        let block = unsafe {
            IoBlock {
                input: BufferList::new(NonNull::from(&mut *input)),
                output: BufferListMut::new(NonNull::from(&mut *output)),
                in_sample_time: 0.0,
                out_sample_time: 0.0,
            }
        };
        cb(block);
    }

    #[test]
    fn default_config_is_drift_compensated_default_input() {
        let config = MeasureAggregateConfig::default();
        assert!(
            config.drift_compensation,
            "MS-22: drift comp is the default"
        );
        assert_eq!(config.mic, MicSelector::DefaultInput);
    }

    // Selector validation runs before any FFI, so this reaches no HAL and
    // passes on a CI box with no audio devices (the volume.rs precedent).
    #[test]
    fn empty_mic_uid_refused_before_touching_the_hal() {
        let config = MeasureAggregateConfig {
            mic: MicSelector::Uid(String::new()),
            ..MeasureAggregateConfig::default()
        };
        match MicCapture::create(config) {
            Err(MeasureAggregateError::EmptyMicUid) => {}
            Err(other) => panic!("expected EmptyMicUid, got {other:?}"),
            Ok(_) => panic!("expected EmptyMicUid, got Ok"),
        }
    }

    #[test]
    fn drain_ring_preserves_order_and_widens_exactly() {
        let (mut tx, mut rx) = RingBuffer::<f32>::new(8);
        for v in [0.5f32, -0.25, 1.0e-30, -0.0] {
            tx.push(v).expect("ring has room");
        }
        let mut block = [7.7f64; 4];
        assert_eq!(drain_ring(&mut rx, &mut block), 4);
        // f32 -> f64 widening is exact; -0.0 must stay -0.0 (bit-exact).
        assert_eq!(block[0], 0.5);
        assert_eq!(block[1], -0.25);
        assert_eq!(block[2], 1.0e-30f32 as f64);
        assert_eq!(block[3].to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn drain_ring_short_read_returns_what_is_there() {
        let (mut tx, mut rx) = RingBuffer::<f32>::new(8);
        tx.push(1.0).expect("room");
        tx.push(2.0).expect("room");
        let mut block = [0.0f64; 5];
        assert_eq!(drain_ring(&mut rx, &mut block), 2);
        assert_eq!(&block[..2], &[1.0, 2.0]);
    }

    #[test]
    fn drain_ring_empty_ring_reads_zero_frames() {
        let (_tx, mut rx) = RingBuffer::<f32>::new(4);
        let mut block = [0.0f64; 4];
        assert_eq!(drain_ring(&mut rx, &mut block), 0);
    }

    #[test]
    fn drain_ring_empty_block_reads_zero_frames() {
        let (mut tx, mut rx) = RingBuffer::<f32>::new(4);
        tx.push(1.0).expect("room");
        let mut block: [f64; 0] = [];
        assert_eq!(drain_ring(&mut rx, &mut block), 0);
        // The queued sample must still be there for the next, real read.
        let mut real = [0.0f64; 1];
        assert_eq!(drain_ring(&mut rx, &mut real), 1);
        assert_eq!(real[0], 1.0);
    }

    #[test]
    fn mic_death_maps_to_a_fatal_message_naming_the_mic() {
        let msg = map_capture_event(kAudioDevicePropertyDeviceIsAlive, "UMIK-1-UID")
            .expect("device-is-alive must be fatal");
        assert!(msg.contains("UMIK-1-UID"), "message names the mic: {msg}");
    }

    #[test]
    fn default_output_change_maps_to_a_fatal_message() {
        assert!(
            map_capture_event(kAudioHardwarePropertyDefaultOutputDevice, "mic").is_some(),
            "a default-output change mid-measurement must refuse, not degrade"
        );
    }

    #[test]
    fn unrelated_selectors_are_not_fatal() {
        // An arbitrary non-watched selector must not kill a capture.
        assert_eq!(map_capture_event(0x1234_5678, "mic"), None);
    }

    #[test]
    fn capture_callback_zeroes_a_dirty_output_buffer() {
        // The aggregate contributes silence to the output mix: a non-zeroed
        // output plays GARBAGE at measurement volume — a real safety hole.
        // Every output sample must be zeroed regardless of the dirty state the
        // HAL buffer arrives in.
        let (producer, _consumer) = RingBuffer::<f32>::new(64);
        let counters = Arc::new(InnerCounters::default());
        let mut cb = capture_callback(producer, Arc::clone(&counters), 64, None);

        // Input: one interleaved stereo buffer, 2 frames (L0 R0 L1 R1).
        let mut in_storage = vec![0.5f32, -0.5, 0.25, -0.25];
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 2)],
        };
        // Output: one stereo buffer pre-dirtied with a 7.0 sentinel.
        let mut out_storage = vec![7.0f32; 6];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 2)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        assert!(
            out_storage.iter().all(|&s| s == 0.0),
            "every output sample must be zeroed, got {out_storage:?}"
        );
    }

    // ─────────────────── the Stage-5 stimulus output path ───────────────────

    /// A callback with a stimulus ring armed, plus the producer to feed it.
    fn stimulus_cb(routing: StimulusRouting) -> (IoCallback, Producer<f32>, Arc<StimulusInner>) {
        let (_cap_tx, _cap_rx) = RingBuffer::<f32>::new(1024);
        let (stim_tx, stim_rx) = RingBuffer::<f32>::new(1024);
        let inner = Arc::new(StimulusInner::default());
        let cb = capture_callback(
            _cap_tx,
            Arc::new(InnerCounters::default()),
            64,
            Some(StimulusPath {
                consumer: stim_rx,
                inner: Arc::clone(&inner),
                routing,
            }),
        );
        (cb, stim_tx, inner)
    }

    /// An empty input list: these tests are about the output half only.
    fn silent_input() -> Vec<f32> {
        vec![0.0f32; 8]
    }

    #[test]
    fn the_stimulus_is_pulled_once_per_frame_not_once_per_sample() {
        // The DGR Labs layout gotcha, as a rate bug: a stereo stream can arrive
        // as ONE interleaved 2-channel buffer, and pulling per sample there
        // would play the stimulus at 2x speed.
        let (mut cb, mut tx, _) = stimulus_cb(StimulusRouting::Both);
        for v in [0.1f32, 0.2, 0.3, 0.4] {
            tx.push(v).expect("room");
        }
        let mut in_storage = silent_input();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        // 4 frames x 2 channels interleaved.
        let mut out_storage = vec![9.0f32; 8];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 2)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        assert_eq!(
            out_storage,
            vec![0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.4, 0.4],
            "one mono sample per FRAME, replicated across the frame's channels"
        );
    }

    #[test]
    fn routing_to_one_channel_leaves_the_other_silent() {
        let (mut cb, mut tx, _) = stimulus_cb(StimulusRouting::Only(1));
        for v in [0.5f32, 0.6] {
            tx.push(v).expect("room");
        }
        let mut in_storage = silent_input();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        let mut out_storage = vec![9.0f32; 4];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 2)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        assert_eq!(
            out_storage,
            vec![0.0, 0.5, 0.0, 0.6],
            "a stereo coupler measures one side at a time"
        );
    }

    #[test]
    fn a_routing_the_device_cannot_honour_plays_silence_not_channel_zero() {
        // A wrong channel is a plausible wrong answer; silence is an obvious
        // failure. Prefer the obvious one.
        let (mut cb, mut tx, _) = stimulus_cb(StimulusRouting::Only(5));
        for v in [0.5f32, 0.6] {
            tx.push(v).expect("room");
        }
        let mut in_storage = silent_input();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        let mut out_storage = vec![9.0f32; 4];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 2)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        assert!(out_storage.iter().all(|&s| s == 0.0), "got {out_storage:?}");
    }

    #[test]
    fn an_empty_ring_writes_silence_and_counts_the_underrun() {
        // Silence, not the last sample held: a held sample is a DC step into
        // the driver and a defect in the measurement.
        let (mut cb, mut tx, inner) = stimulus_cb(StimulusRouting::Both);
        tx.push(0.5f32).expect("room");
        let mut in_storage = silent_input();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        let mut out_storage = vec![9.0f32; 4];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 1)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        assert_eq!(out_storage, vec![0.5, 0.0, 0.0, 0.0]);
        assert_eq!(inner.underrun.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn an_unarmed_stimulus_leaves_the_output_silent_exactly_as_before() {
        // A caller that never takes the sink gets the pre-Stage-5 behaviour.
        let (producer, _consumer) = RingBuffer::<f32>::new(64);
        let mut cb = capture_callback(producer, Arc::new(InnerCounters::default()), 64, None);
        let mut in_storage = silent_input();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        let mut out_storage = vec![7.0f32; 6];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 2)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        assert!(out_storage.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_flush_drops_queued_audio_and_outputs_silence() {
        // `stop` must not flush the queue at level — that is the click the
        // whole fade design exists to prevent.
        let (mut cb, mut tx, inner) = stimulus_cb(StimulusRouting::Both);
        for _ in 0..8 {
            tx.push(0.9f32).expect("room");
        }
        inner.flush.store(true, Ordering::Relaxed);
        let mut in_storage = silent_input();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        let mut out_storage = vec![9.0f32; 4];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 1)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        assert!(out_storage.iter().all(|&s| s == 0.0), "got {out_storage:?}");
        assert_eq!(inner.flushed.load(Ordering::Relaxed), 8);
        assert!(
            !inner.flush.load(Ordering::Relaxed),
            "the flush is one-shot; a later block plays normally again"
        );
    }

    #[test]
    fn only_the_first_populated_output_buffer_is_driven() {
        // A HAL output list can carry more than one stream; writing the same
        // stimulus into all of them plays it twice into one acoustic path at
        // unknown relative gain.
        let (mut cb, mut tx, _) = stimulus_cb(StimulusRouting::Both);
        for v in [0.3f32, 0.4] {
            tx.push(v).expect("room");
        }
        let mut in_storage = silent_input();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        let mut first = vec![9.0f32; 2];
        let mut second = vec![9.0f32; 2];
        // A two-entry `AudioBufferList`. `AudioBufferList` declares
        // `mBuffers: [AudioBuffer; 1]` and the HAL over-allocates the trailing
        // array, so a two-buffer list needs one extra `AudioBuffer` directly
        // after it. A `#[repr(C)]` pair reproduces that layout *and* its
        // alignment — a `Vec<u8>` scratch buffer would only be byte-aligned,
        // and casting it to `*mut AudioBufferList` would be UB.
        #[repr(C)]
        struct TwoBufferList {
            list: AudioBufferList,
            extra: AudioBuffer,
        }
        let mut two = TwoBufferList {
            list: AudioBufferList {
                mNumberBuffers: 2,
                mBuffers: [buffer_over(&mut first, 1)],
            },
            extra: buffer_over(&mut second, 1),
        };
        // SAFETY: `two` is a live, correctly aligned `#[repr(C)]` value whose
        // layout is exactly a 2-entry AudioBufferList; both of its buffers
        // point at distinct live f32 storages that outlive the block, and
        // `in_list` likewise.
        let block = unsafe {
            IoBlock {
                input: BufferList::new(NonNull::from(&mut in_list)),
                output: BufferListMut::new(NonNull::from(&mut two.list)),
                in_sample_time: 0.0,
                out_sample_time: 0.0,
            }
        };
        cb(block);

        assert_eq!(first, vec![0.3, 0.4], "the first stream carries it");
        assert_eq!(second, vec![0.0, 0.0], "and nothing else does");
    }

    #[test]
    fn capture_callback_extracts_interleaved_channel_zero_into_the_ring() {
        // Mono capture is channel 0 of the first populated interleaved buffer.
        let (producer, mut consumer) = RingBuffer::<f32>::new(64);
        let counters = Arc::new(InnerCounters::default());
        let mut cb = capture_callback(producer, Arc::clone(&counters), 64, None);

        // 4 frames x 2 channels interleaved: channel 0 = [1, 2, 3, 4].
        let mut in_storage = vec![1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0];
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 2)],
        };
        let mut out_storage = vec![0.0f32; 8];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 2)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        let mut drained = [0.0f64; 8];
        let n = drain_ring(&mut consumer, &mut drained);
        assert_eq!(n, 4, "four frames of channel 0 landed in the ring");
        assert_eq!(&drained[..4], &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(counters.callbacks.load(Ordering::Relaxed), 1);
        assert_eq!(
            counters.dropped.load(Ordering::Relaxed),
            0,
            "nothing dropped"
        );
        assert_eq!(
            counters.invalid.load(Ordering::Relaxed),
            0,
            "no non-finite input"
        );
    }

    #[test]
    fn capture_callback_counts_the_dropped_tail_of_an_oversized_block() {
        // A block delivering MORE frames than the scratch capacity keeps what
        // fits and counts the tail as dropped — never allocating. The ring is
        // roomy so the ONLY drops come from the oversized tail, letting this
        // assert the tail count EXACTLY. (This is the assertion that kills the
        // surviving mutant `dropped += (frames - n)` -> `dropped += 0`.)
        const SCRATCH: usize = 4; // tiny scratch to force the oversize path
        let (producer, mut consumer) = RingBuffer::<f32>::new(64);
        let counters = Arc::new(InnerCounters::default());
        let mut cb = capture_callback(producer, Arc::clone(&counters), SCRATCH, None);

        // Mono, 10 frames: n = min(10, 4) = 4 fit, 6 dropped as the tail.
        let mut in_storage: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        let mut out_storage = vec![0.0f32; 4];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 1)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        assert_eq!(
            counters.dropped.load(Ordering::Relaxed),
            6,
            "exactly the 6-frame tail beyond the 4-frame scratch is dropped"
        );
        // The 4 that fit still land, in order.
        let mut drained = [0.0f64; 8];
        let n = drain_ring(&mut consumer, &mut drained);
        assert_eq!(n, 4);
        assert_eq!(&drained[..4], &[0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn capture_callback_counts_ring_overflow() {
        // A ring too small to hold the block: the overflow samples are dropped
        // and counted; the realtime side never blocks. Scratch is roomy so the
        // ONLY drops come from the ring, and every input frame is accounted for
        // (captured or counted-dropped).
        let (producer, mut consumer) = RingBuffer::<f32>::new(4); // tiny ring
        let counters = Arc::new(InnerCounters::default());
        let mut cb = capture_callback(producer, Arc::clone(&counters), 64, None); // roomy scratch

        let mut in_storage: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let mut in_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut in_storage, 1)],
        };
        let mut out_storage = vec![0.0f32; 4];
        let mut out_list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [buffer_over(&mut out_storage, 1)],
        };

        run_block(&mut cb, &mut in_list, &mut out_list);

        let mut drained = [0.0f64; 16];
        let received = drain_ring(&mut consumer, &mut drained);
        let dropped = counters.dropped.load(Ordering::Relaxed) as usize;
        assert!(dropped > 0, "an overflowing ring must count drops");
        assert_eq!(
            received + dropped,
            10,
            "every input frame is captured or counted dropped \
             (received {received}, dropped {dropped})"
        );
    }
}
