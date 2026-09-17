//! Output-only rendering to the PHYSICAL default output device — the
//! verification stimulus's route to the air.
//!
//! This is what the `paraeq-stimulus` child process runs. Unlike
//! [`StimulusOutput`](crate::measure_aggregate::StimulusOutput) it opens its
//! own device and touches no mic: a child that created an aggregate over the
//! user's microphone would raise a second TCC prompt mid-wizard, and
//! measurement-safety's risk table calls a spurious mic prompt a **safety**
//! issue rather than a cosmetic one — "a novice trained to dismiss a spurious
//! prompt will dismiss the real one".
//!
//! **Why this does not reopen the two-clock problem.**
//! `measure_aggregate`'s header objects to a sink "on a separate stream" that
//! "would put play and record back on different crystals". That objection is
//! about ParaEQ opening a SECOND OUTPUT STREAM while capturing. Here a
//! different process renders to the same physical device that is already the
//! measurement aggregate's clock master ("main sub-device (clock master) = the
//! output device"), so the audio lands on the same crystal. What verification
//! loses is not the crystal, it is the play cursor — a timing-knowledge
//! problem, which the parent solves with timing markers baked into the WAV,
//! never with an IPC timestamp.
//!
//! **The device this opens is a PRIVATE SINGLE-SUB-DEVICE AGGREGATE** wrapping
//! the physical output, composed with `kAudioSubDeviceInputChannelsKey: 0` —
//! see [`create_render_aggregate`]. Registering a bare IOProc on a mic-capable
//! default output (AirPods, a USB headset) may count as microphone access; the
//! zero-input-channels wrapper is the fix that already ships twice in this
//! crate, in `tap.rs` and in `measure_aggregate.rs`, for exactly this reason.
//! The wrapper's only sub-device IS the physical output, so it is its own clock
//! master and the paragraph above still holds.
//!
//! Test tier: none of the four oracle tiers applies — platform FFI with no
//! numerical oracle (the `volume.rs` precedent). The pure parts (the
//! composition dictionary's keys, the routing write, the ring drain) get unit
//! tests with no HAL; HAL behaviour gets `#[ignore]` hardware tests in
//! `tests/test_render.rs`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceIsAlive, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectSystemObject, kAudioSubDeviceInputChannelsKey, kAudioSubDeviceUIDKey,
    AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice, AudioObjectID,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use paraeq_measure::{MeasureError, RenderSink, StreamFormat};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::error::{check, CaError};
use crate::ioproc::{BufferListMut, IoBlock, IoCallback, IoProcHandle};
use crate::listeners::{ListenerEvent, PropertyListener};
use crate::measure_aggregate::{write_frame, StimulusCounters, StimulusRouting, STALL_SECONDS};
use crate::properties;

/// Which physical output the render aggregate wraps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderTarget {
    /// Whatever is the default output right now.
    DefaultOutput,
    /// A named device, by HAL UID. The parent names the device it taps, so the
    /// two sides cannot drift onto different routes.
    Uid(String),
}

/// How the renderer is built. Deliberately carries no level, no rate and no
/// buffer size: the WAV is the level, the device's nominal rate is the rate,
/// and `set_buffer_frame_size` is device-GLOBAL — calling it would renegotiate
/// the engine's own geometry mid-verification.
#[derive(Clone, Debug)]
pub struct RenderConfig {
    /// Ring depth in blocks. Small on purpose: [`RenderSink::write`] is paced,
    /// so a deep queue would only make the abort ramp arrive later.
    pub ring_capacity_blocks: usize,
    pub routing: StimulusRouting,
    pub target: RenderTarget,
}

impl Default for RenderConfig {
    fn default() -> Self {
        RenderConfig {
            ring_capacity_blocks: 8,
            routing: StimulusRouting::Both,
            target: RenderTarget::DefaultOutput,
        }
    }
}

/// Why a renderer could not be opened. Structured, because the child maps each
/// one to a distinct exit code and the parent's refusal names it.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no device has UID '{uid}'")]
    DeviceNotFound { uid: String },
    #[error("the physical output '{uid}' changed while the render device was being built")]
    DeviceChangedDuringBuild { uid: String },
    #[error("a device UID was requested but it was empty")]
    EmptyUid,
    #[error(transparent)]
    Hal(#[from] CaError),
    #[error("render device '{device_uid}' reports no output channels")]
    NoOutputStreams { device_uid: String },
}

/// The render aggregate's composition, as plain data.
///
/// Built and asserted with NO HAL call: the mic-prompt hazard is a KEY, and a
/// key is testable on any machine. [`create_render_aggregate`] turns this into
/// the CoreFoundation dictionary and does nothing else with it, so a test over
/// this struct is a test of what the HAL is handed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderAggregateComposition {
    /// MUST be 0. `kAudioSubDeviceInputChannelsKey: 0` excludes a mic-capable
    /// default output's own input streams so the IOProc's input list is empty —
    /// the same hardening `tap.rs` and `measure_aggregate.rs` apply, for the
    /// same reason (no spurious TCC surface).
    pub input_channels: i32,
    /// Never appears in the user's device list.
    pub is_private: bool,
    pub is_stacked: bool,
    /// Clock master. It IS the physical output, which is what keeps the render
    /// path on the measurement aggregate's crystal.
    pub main_sub_device_uid: String,
    pub name: String,
    /// The single sub-device. There is exactly one, and it is the physical
    /// output.
    pub sub_device_uid: String,
    /// `com.paraeq.render.<pid>` — per-process, so the parent's teardown can
    /// assert this exact device is gone after the child exits.
    pub uid: String,
}

/// The composition for a render aggregate wrapping `out_uid`.
///
/// `kAudioAggregateDeviceTapAutoStartKey` and `kAudioAggregateDeviceTapListKey`
/// are DELIBERATELY ABSENT: this aggregate carries no tap. It mirrors
/// `create_measure_aggregate` key for key, minus the mic sub-device and minus
/// the tap list.
pub fn render_aggregate_composition(out_uid: &str) -> RenderAggregateComposition {
    RenderAggregateComposition {
        input_channels: 0,
        is_private: true,
        is_stacked: false,
        main_sub_device_uid: out_uid.to_owned(),
        name: "paraeq-render-agg".to_owned(),
        sub_device_uid: out_uid.to_owned(),
        uid: format!("com.paraeq.render.{}", std::process::id()),
    }
}

/// Create the private single-sub-device aggregate described by
/// [`render_aggregate_composition`].
///
/// This is the ONE new HAL entry point the verification path adds, and it is
/// the same call in the same shape as the two that already ship
/// (`tap.rs::create_aggregate`, `measure_aggregate::create_measure_aggregate`).
/// Neither of those is reusable: the first demands a tap UUID and composes a
/// tap list, the second is private and demands a mic UID.
///
/// Prefer this over `kAudioDevicePropertyIOProcStreamUsage`, which is the
/// other way to keep the IOProc off the device's input streams: that property
/// is a C variable-length-array struct, i.e. the hardest kind of unsafe Rust to
/// get right, against a key whose fix already ships twice here.
pub(crate) fn create_render_aggregate(out_uid: &CFString) -> Result<AudioObjectID, CaError> {
    let key = |c: &std::ffi::CStr| CFString::from_str(c.to_str().unwrap());
    let composition = render_aggregate_composition(&out_uid.to_string());

    // Sub-device entry: { channels-in: 0, uid: <output UID> }.
    let sub_dev: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[
            &*key(kAudioSubDeviceInputChannelsKey),
            &*key(kAudioSubDeviceUIDKey),
        ],
        &[
            // CFNumber, NOT CFBoolean — the sub-device channel keys are counts.
            CFNumber::new_i32(composition.input_channels).as_ref() as &CFType,
            out_uid.as_ref() as &CFType,
        ],
    );
    let sub_devs = CFArray::from_objects(&[sub_dev.as_ref() as &CFType]);
    let agg_uid = CFString::from_str(&composition.uid);
    let name = CFString::from_str(&composition.name);

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
            // Clock master: the physical output device.
            out_uid.as_ref() as &CFType,
            CFBoolean::new(composition.is_private).as_ref() as &CFType,
            CFBoolean::new(composition.is_stacked).as_ref() as &CFType,
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
        "AudioHardwareCreateAggregateDevice (render: the verification helper's output device)",
    )?;
    Ok(agg)
}

/// RAII owner of one render aggregate (the `TapSystem` / `MeasureAggregate`
/// shape). Destroy is guarded, logged and collected — never silent.
///
/// **This is the resource the whole teardown ladder exists for.** A child that
/// dies without running `Drop` — SIGKILL, or SIGTERM with no handler installed
/// — leaves this aggregate wrapping the user's output device.
pub struct RenderAggregate {
    id: AudioObjectID,
    torn_down: bool,
    uid: String,
}

impl RenderAggregate {
    /// Create the aggregate over the physical output device with UID `out_uid`.
    pub fn create(out_uid: &str) -> Result<RenderAggregate, CaError> {
        let id = create_render_aggregate(&CFString::from_str(out_uid))?;
        Ok(RenderAggregate {
            id,
            torn_down: false,
            uid: render_aggregate_composition(out_uid).uid,
        })
    }

    pub fn id(&self) -> AudioObjectID {
        self.id
    }

    /// The UID this aggregate was composed with, `com.paraeq.render.<pid>`.
    /// The parent's teardown asserts no device with this UID survives the
    /// child.
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// Destroy the aggregate. Idempotent; every failure is logged AND
    /// collected. There is no recovery from a failed destroy, but it must be
    /// visible — the failure mode is a device left on the user's system.
    pub fn teardown(&mut self) -> Vec<CaError> {
        if self.torn_down {
            return Vec::new();
        }
        self.torn_down = true;
        let mut errors = Vec::new();
        // SAFETY: `id` was created by AudioHardwareCreateAggregateDevice and is
        // destroyed at most once (guarded by `torn_down`).
        let status = unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
        if let Err(e) = check(status, "AudioHardwareDestroyAggregateDevice (render)") {
            log::error!("render aggregate teardown: {e}");
            errors.push(e);
        }
        errors
    }
}

impl Drop for RenderAggregate {
    fn drop(&mut self) {
        // Errors are logged inside teardown; nothing more a Drop can do.
        let _ = self.teardown();
    }
}

/// Realtime-side state shared with [`DeviceRenderer`]. Atomics only: the
/// realtime lane never blocks, allocates or logs.
#[derive(Debug, Default)]
pub(crate) struct RenderInner {
    /// Total IOProc invocations — the child's stall witness (exit code 5).
    callbacks: AtomicU64,
    /// One-shot: drop whatever is queued instead of playing it out at level.
    flush: AtomicBool,
    flushed: AtomicU64,
    /// Set by [`RenderSink::ramp_out`]. After it, an empty ring is EXPECTED
    /// (the ramp has played and silence follows), so underruns stop counting —
    /// a counter that rose on purpose would not be a fault signal any more.
    ramping: AtomicBool,
    underrun: AtomicU64,
}

/// The renderer's realtime fill: drain the ring one mono sample per FRAME and
/// write it through the shared [`write_frame`].
///
/// A second realtime output path is a real cost, and it is paid deliberately:
/// the in-process path pulls from an aggregate the mic is also captured from,
/// and this one pulls from a device-only IOProc in another process. What the
/// two must not do is disagree about ROUTING or about what an empty ring means,
/// and that is what `write_frame` and
/// `the_renderer_and_the_stimulus_path_write_identical_frames_for_identical_input`
/// pin.
pub(crate) struct RenderFill {
    consumer: Consumer<f32>,
    inner: Arc<RenderInner>,
    routing: StimulusRouting,
}

impl RenderFill {
    pub(crate) fn new(
        consumer: Consumer<f32>,
        inner: Arc<RenderInner>,
        routing: StimulusRouting,
    ) -> RenderFill {
        RenderFill {
            consumer,
            inner,
            routing,
        }
    }

    /// Write one IO cycle's worth of stimulus. Same three rules as the
    /// in-process path, for the same reasons: one mono sample per FRAME (not
    /// per sample, which would play at N× speed on an interleaved device);
    /// only the FIRST populated output buffer is driven (writing into all of
    /// them plays the stimulus twice into one acoustic path at unknown relative
    /// gain); an empty ring writes SILENCE, never the previous sample, because
    /// a held sample is a DC step into the driver.
    pub(crate) fn fill(&mut self, output: &mut BufferListMut<'_>) {
        if self.inner.flush.swap(false, Ordering::Relaxed) {
            let mut flushed = 0u64;
            while self.consumer.pop().is_ok() {
                flushed += 1;
            }
            self.inner.flushed.fetch_add(flushed, Ordering::Relaxed);
            return;
        }
        let ramping = self.inner.ramping.load(Ordering::Relaxed);
        let mut underrun = 0u64;
        for (buf, channels) in output.buffers_mut() {
            let channels = channels.max(1);
            let frames = buf.len() / channels;
            if frames == 0 {
                continue;
            }
            for frame in 0..frames {
                let Ok(sample) = self.consumer.pop() else {
                    if !ramping {
                        underrun += (frames - frame) as u64;
                    }
                    break;
                };
                write_frame(buf, channels, frame, self.routing, sample);
            }
            break;
        }
        if underrun > 0 {
            self.inner.underrun.fetch_add(underrun, Ordering::Relaxed);
        }
    }
}

/// Build the realtime render closure. REALTIME LANE: no allocation, no locks,
/// no logging.
fn render_callback(mut fill: RenderFill, inner: Arc<RenderInner>) -> IoCallback {
    Box::new(move |mut block: IoBlock<'_>| {
        inner.callbacks.fetch_add(1, Ordering::Relaxed);
        // Zero ALL output buffers first (the backend.rs order): unwritten HAL
        // output space must never leak stale samples.
        for (buf, _) in block.output.buffers_mut() {
            buf.fill(0.0);
        }
        fill.fill(&mut block.output);
    })
}

/// RAII owner of one render IOProc on one private render aggregate.
///
/// Field declaration order is the drop-order backup for the teardown invariant
/// (the `MicCapture` / `TapBackend` shape): listeners first, then `io` (stop +
/// destroy IOProc), then `aggregate` (destroy the aggregate device). Rust drops
/// fields in declaration order, so even a bare `Drop` runs the invariant order.
pub struct DeviceRenderer {
    listeners: Vec<PropertyListener>,
    events: Option<Receiver<ListenerEvent>>,
    io: Option<IoProcHandle>,
    aggregate: Option<RenderAggregate>,
    capacity: usize,
    /// The PHYSICAL output device's UID — the route the parent taps.
    device_uid: String,
    format: StreamFormat,
    inner: Arc<RenderInner>,
    producer: Producer<f32>,
    /// `ramp_out` has been called: the next `write` is the ramp and the last
    /// audio this sink accepts.
    ramp_armed: bool,
    ramp_written: bool,
    /// The AGGREGATE's UID, `com.paraeq.render.<pid>`. The child echoes it so
    /// the parent's rate fence reads the same device the child opened.
    render_device_uid: String,
    stopped: bool,
    /// The physical output's own nominal rate, echoed beside the aggregate's:
    /// nothing establishes that a private aggregate's nominal rate equals its
    /// single sub-device's, so a rate refusal must be able to say WHICH one
    /// disagreed.
    sub_device_sample_rate_hz: f64,
    torn_down: bool,
}

impl DeviceRenderer {
    /// Resolve the device, wrap it, register and start the IOProc.
    ///
    /// Order copies `MicCapture::create`'s discipline: selector validation
    /// BEFORE any HAL call, then resolve, then build, then arm listeners, then
    /// close the rebuild window. Every `?` past the aggregate's creation
    /// destroys it, because the local is RAII and locals drop in reverse
    /// creation order.
    pub fn open(config: RenderConfig) -> Result<DeviceRenderer, RenderError> {
        // 1. Selector validation BEFORE any HAL call (the volume.rs shape).
        if let RenderTarget::Uid(uid) = &config.target {
            if uid.is_empty() {
                return Err(RenderError::EmptyUid);
            }
        }

        // 2. Resolve the physical output; refuse rather than guess.
        let out_dev = match &config.target {
            RenderTarget::DefaultOutput => properties::default_output_device()?,
            RenderTarget::Uid(uid) => properties::translate_uid_to_device(uid)?,
        };
        if out_dev == 0 {
            return Err(RenderError::DeviceNotFound {
                uid: match &config.target {
                    RenderTarget::DefaultOutput => "<default output>".to_owned(),
                    RenderTarget::Uid(uid) => uid.clone(),
                },
            });
        }
        let device_uid = properties::device_uid(out_dev)?;
        let sub_device_sample_rate_hz = properties::nominal_sample_rate(out_dev)?;

        // 3. The private wrapper, in RAII immediately so every later `?`
        //    destroys it (R4: a bare IOProc on a mic-capable output may count
        //    as microphone access).
        let mut aggregate = RenderAggregate::create(&device_uid)?;
        let render_device_uid = aggregate.uid().to_owned();
        let render_dev = aggregate.id();

        let sample_rate_hz = match properties::nominal_sample_rate(render_dev) {
            Ok(rate) => rate,
            Err(e) => return Err(teardown_on_create_error(&mut aggregate, e)),
        };
        let frames_per_block = match properties::buffer_frame_size(render_dev) {
            Ok(frames) => frames as usize,
            Err(e) => return Err(teardown_on_create_error(&mut aggregate, e)),
        };
        let channels = match properties::output_channel_count(render_dev) {
            Ok(channels) => channels as usize,
            Err(e) => return Err(teardown_on_create_error(&mut aggregate, e)),
        };
        if channels == 0 {
            for err in aggregate.teardown() {
                log::error!("render open cleanup: {err}");
            }
            return Err(RenderError::NoOutputStreams {
                device_uid: render_device_uid,
            });
        }

        // 4. Bounded SPSC ring + counters + the realtime closure. Shallow on
        //    purpose: `write` is paced, so depth buys nothing and costs abort
        //    latency.
        let capacity = frames_per_block.max(1) * config.ring_capacity_blocks.max(2) * channels;
        let (producer, consumer) = RingBuffer::<f32>::new(capacity);
        let inner = Arc::new(RenderInner::default());
        let cb = render_callback(
            RenderFill::new(consumer, Arc::clone(&inner), config.routing),
            Arc::clone(&inner),
        );

        // 5. Register + start. From here the locals `io` (stop -> destroy
        //    IOProc) then `aggregate` (destroy aggregate) unwind in invariant
        //    order on any `?`.
        let mut io = IoProcHandle::register(render_dev, cb)?;
        io.start()?;

        // 6. Listeners: the physical output dying, or the default output
        //    changing, must surface — a verification sweep into a route that
        //    moved is a wrong answer, not a glitch.
        let (tx, rx) = mpsc::channel();
        let listeners = vec![
            PropertyListener::watch(
                kAudioObjectSystemObject as u32,
                kAudioHardwarePropertyDefaultOutputDevice,
                tx.clone(),
            )?,
            PropertyListener::watch(out_dev, kAudioDevicePropertyDeviceIsAlive, tx)?,
        ];

        // 7. Close the rebuild-window race (the backend.rs / measure_aggregate
        //    step-7 precedent): the physical output may have changed between
        //    resolving it and arming the listeners, and a change in that window
        //    fired no listener. Re-query now that they are armed; refuse on a
        //    mismatch so the caller rebuilds. The still-live locals drop in the
        //    invariant order on the early return.
        if properties::translate_uid_to_device(&device_uid)? != out_dev {
            return Err(RenderError::DeviceChangedDuringBuild { uid: device_uid });
        }

        log::debug!(
            "render device up: agg={render_dev} uid='{render_device_uid}' \
             out='{device_uid}' rate={sample_rate_hz} sub_rate={sub_device_sample_rate_hz} \
             channels={channels} frames={frames_per_block}"
        );

        Ok(DeviceRenderer {
            listeners,
            events: Some(rx),
            io: Some(io),
            aggregate: Some(aggregate),
            capacity,
            device_uid,
            format: StreamFormat {
                channels,
                frames_per_block,
                sample_rate_hz,
            },
            inner,
            producer,
            ramp_armed: false,
            ramp_written: false,
            render_device_uid,
            stopped: false,
            sub_device_sample_rate_hz,
            torn_down: false,
        })
    }

    /// Realtime IOProc invocations since `open`. Zero for longer than
    /// [`STALL_SECONDS`] means the device stopped cycling, which the child
    /// reports rather than waiting out.
    pub fn callbacks(&self) -> u64 {
        self.inner.callbacks.load(Ordering::Relaxed)
    }

    pub fn counters(&self) -> StimulusCounters {
        StimulusCounters {
            flushed_samples: self.inner.flushed.load(Ordering::Relaxed),
            underrun_frames: self.inner.underrun.load(Ordering::Relaxed),
        }
    }

    /// The PHYSICAL output device's UID.
    pub fn device_uid(&self) -> &str {
        &self.device_uid
    }

    /// Listener events (default-output change, device death) the caller may
    /// drain between blocks.
    pub fn events(&self) -> Option<&Receiver<ListenerEvent>> {
        self.events.as_ref()
    }

    pub fn queued_frames(&self) -> usize {
        self.capacity - self.producer.slots()
    }

    /// The AGGREGATE's UID, `com.paraeq.render.<pid>`.
    pub fn render_device_uid(&self) -> &str {
        &self.render_device_uid
    }

    /// The physical output device's own nominal rate — compare against
    /// [`StreamFormat::sample_rate_hz`] to see whether the wrapper and its sole
    /// sub-device agree.
    pub fn sub_device_sample_rate_hz(&self) -> f64 {
        self.sub_device_sample_rate_hz
    }

    /// The full teardown in invariant order; idempotent; errors collected.
    /// Listeners first (nothing should queue events for a system being torn
    /// down), then the IOProc (stop → destroy), then the aggregate.
    fn stop_inner(&mut self) -> Vec<CaError> {
        if self.torn_down {
            return Vec::new();
        }
        self.torn_down = true;
        self.stopped = true;
        // Drop whatever is queued rather than playing it out at level. By the
        // time a caller reaches here the ramp has played, so there is nothing
        // left at level to drop — and if there is, dropping it is still the
        // lesser click.
        self.inner.flush.store(true, Ordering::Relaxed);
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

impl RenderSink for DeviceRenderer {
    fn format(&self) -> StreamFormat {
        self.format.clone()
    }

    /// Arm the abort: the NEXT block written is the 5 ms raised cosine, it is
    /// the last audio this sink accepts, and everything after it is silence.
    ///
    /// Never a hard stop — "A hard stop is itself a full-scale click" — and
    /// never a re-render. The ramp's SAMPLES are the caller's, multiplied by
    /// the one shared envelope; what this call does is make sure nothing at
    /// level can follow them.
    fn ramp_out(&mut self) {
        self.ramp_armed = true;
        self.inner.ramping.store(true, Ordering::Relaxed);
    }

    /// Full teardown; idempotent. Drops queued audio rather than flushing it at
    /// level, then stops the IOProc and destroys the render aggregate.
    ///
    /// Returns nothing on purpose: this runs on every exit path including
    /// panic, and a rung that could fail into a `?` is a rung that can abort
    /// the ladder before the device is destroyed. Failures are logged and
    /// collected inside.
    fn stop(&mut self) {
        for err in self.stop_inner() {
            log::error!("render stop: {err}");
        }
    }

    /// Push one block and WAIT for the device to take it.
    ///
    /// Paced exactly as `StimulusSink::emit` is and for the same MS-14 reason:
    /// a sink that accepted the whole sweep into a deep queue and returned
    /// immediately would make the caller's poll-per-block abort cadence
    /// fictional. Applies NO gain: the block arrives already at level.
    fn write(&mut self, block: &[f32]) -> Result<(), MeasureError> {
        if self.stopped {
            return Err(MeasureError::Sink("render sink is stopped".to_owned()));
        }
        if self.ramp_armed {
            if self.ramp_written {
                return Err(MeasureError::Sink(
                    "the abort ramp has already been written; this sink accepts no more audio"
                        .to_owned(),
                ));
            }
            self.ramp_written = true;
        }
        let mut written = 0usize;
        let mut stall = StallBound::new(&self.format);
        while written < block.len() {
            if self.producer.is_abandoned() {
                return Err(MeasureError::Sink(
                    "the render device went away mid-write".to_owned(),
                ));
            }
            let before = written;
            while written < block.len() {
                // The realtime side never blocks, so a full ring means the
                // device has not consumed yet — wait rather than drop. Dropping
                // would put a hole in the stimulus, which is a measurement
                // defect, not a glitch.
                if self.producer.push(block[written]).is_err() {
                    break;
                }
                written += 1;
            }
            if written >= block.len() {
                break;
            }
            stall.observe(written != before)?;
        }
        // Pace: hold until the device is within about one block of caught up.
        let block_frames = self.format.frames_per_block.max(1);
        let mut stall = StallBound::new(&self.format);
        let mut previous = self.queued_frames();
        while self.queued_frames() > block_frames {
            if self.producer.is_abandoned() {
                return Err(MeasureError::Sink(
                    "the render device went away mid-write".to_owned(),
                ));
            }
            let now = self.queued_frames();
            stall.observe(now < previous)?;
            previous = now;
        }
        Ok(())
    }
}

impl Drop for DeviceRenderer {
    fn drop(&mut self) {
        // Errors are logged inside stop_inner; nothing more a Drop can do. This
        // is the rung that keeps a panicking helper from leaving a private
        // aggregate wrapping the user's output device.
        for err in self.stop_inner() {
            log::error!("render drop: {err}");
        }
    }
}

/// Destroy a partially-built aggregate and wrap the error that caused it. The
/// `MicCapture::create` shape: a create that fails destroys everything it built.
fn teardown_on_create_error(aggregate: &mut RenderAggregate, cause: CaError) -> RenderError {
    for err in aggregate.teardown() {
        log::error!("render open cleanup: {err}");
    }
    RenderError::Hal(cause)
}

/// Bounds how long [`RenderSink::write`] waits on a device that has stopped
/// consuming.
///
/// **Without a bound this is a hang, and the hang is the dangerous kind.** A
/// blocked write means the helper never reaches its abort ramp, never runs its
/// teardown, and never destroys its render aggregate — so a stalled device
/// would leave a private aggregate wrapping the user's output.
///
/// Progress, not elapsed time, is the trigger: a slow device is fine, a
/// STOPPED one is not. The bound itself is
/// [`STALL_SECONDS`](crate::measure_aggregate::STALL_SECONDS), hoisted out of
/// the in-process path so the two give up at the same moment and the child's
/// exit code 5 means one thing.
struct StallBound {
    idle: usize,
    limit: usize,
    sleep: std::time::Duration,
}

impl StallBound {
    fn new(format: &StreamFormat) -> StallBound {
        let sleep = block_sleep(format);
        StallBound {
            idle: 0,
            limit: ((STALL_SECONDS / sleep.as_secs_f64()).ceil() as usize).max(8),
            sleep,
        }
    }

    /// Record whether this iteration made progress, then wait. `Err` once the
    /// device has been stationary for [`STALL_SECONDS`].
    fn observe(&mut self, progressed: bool) -> Result<(), MeasureError> {
        if progressed {
            self.idle = 0;
        } else {
            self.idle += 1;
            if self.idle >= self.limit {
                return Err(MeasureError::Sink(format!(
                    "the render device stopped consuming the stimulus for {STALL_SECONDS:.1} s — \
                     abandoning the write so the helper can ramp, tear down and destroy its \
                     render device"
                )));
            }
        }
        std::thread::sleep(self.sleep);
        Ok(())
    }
}

/// How long to sleep while waiting on the device: a quarter of a block, so the
/// wait costs at most ~25% of a block's latency and never busy-spins a core.
/// The same rule the in-process path uses.
fn block_sleep(format: &StreamFormat) -> std::time::Duration {
    let rate = if format.sample_rate_hz > 0.0 {
        format.sample_rate_hz
    } else {
        48_000.0
    };
    let seconds = format.frames_per_block.max(1) as f64 / rate / 4.0;
    std::time::Duration::from_secs_f64(seconds.clamp(1e-4, 0.05))
}
