//! The verification pass, mock-driven: no engine, no device, no process.
//!
//! Tier 3 (analytic/policy) per the four-tier convention. Gate order, refusal
//! timing, the level book and the teardown ladder are product policy with no
//! oracle. What is pinned here: every gate refuses for its own reason and
//! **before a process is spawned** where it says it does, `L_verify` is
//! `L_measure + preamp_db` and never louder than the baseline at any bin, the
//! MS-11 margin is applied exactly once across the two levels, the two witness
//! windows refuse rather than assume, the WAV the helper plays is the
//! assembled stimulus bit for bit, and every exit path — command, drop, panic
//! — runs the full restore sequence with the ramp request before any kill.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use paraeq_dsp::peq::{EQBand, FilterType, ParametricEQ};
use paraeq_dsp::targets::TransducerClass;
use paraeq_dsp::two_clock::MarkerLayout;
use paraeq_measure::{
    assemble_bracketed, assemble_sweep, classify_exit, exit_diagnostic, expected_render_device_uid,
    margined_emit_dbfs, CalSensitivity, CalSummary, CaptureSource, EngineControl, EngineFacts,
    EngineStatusKind, GainPin, HelperExit, HelperExitKind, HelperProcess, HelperRouting,
    MeasureError, MeasurementDiagnostic as D, MeasurementLeaseToken, SessionEvent, StimulusKind,
    StreamFormat, SweepLevel, SweepShape, TapActivity, TapStatus, VerificationPass, VerifyError,
    VerifyPhase, VerifyPlan, VerifyRequest, VerifySeam, VerifyTiming, VolumeControl,
    CAL_ERROR_MARGIN_DB, CAPTURE_FLOOR_DBFS,
};

const PRE_VOLUME: f64 = 0.4;
const RATE: u32 = 48_000;
const RATE_F: f64 = 48_000.0;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ── Scratch directory ─────────────────────────────────────────────────────

/// A scratch directory under the crate's own `target/`, unique to one test in
/// one process. Same shape and same reason as `test_store.rs`'s:
/// `CARGO_TARGET_TMPDIR` is one directory shared by every integration-test
/// binary of this crate, and the same binary runs twice (once per feature
/// set), so a fixed name races.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(test: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("verify-{test}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self { path }
    }

    fn wav(&self) -> PathBuf {
        self.path.join("verify.wav")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ── Mock kit ──────────────────────────────────────────────────────────────

/// One shared journal across every mock, because the things under test here
/// are ORDERS — the lease before the first engine read, the ramp request
/// before any kill, the volume restore before the lease release.
#[derive(Clone, Default)]
struct Journal(Arc<Mutex<Vec<String>>>);

impl Journal {
    fn record(&self, call: &str) {
        lock(&self.0).push(call.to_owned());
    }

    fn calls(&self) -> Vec<String> {
        lock(&self.0).clone()
    }

    fn index_of(&self, call: &str) -> Option<usize> {
        self.calls().iter().position(|c| c == call)
    }
}

/// Everything the realtime witness needs to be scripted: audio only flows
/// after `play`, and only once the capture has served `wake_at` frames — which
/// is how "silent through the lead-in, awake at the first marker" is
/// expressed.
#[derive(Clone)]
struct Flow {
    frames_served: Arc<AtomicU64>,
    played: Arc<AtomicBool>,
    wake_at: Arc<AtomicU64>,
}

impl Default for Flow {
    fn default() -> Self {
        Self {
            frames_served: Arc::new(AtomicU64::new(0)),
            played: Arc::new(AtomicBool::new(false)),
            wake_at: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Flow {
    fn flowing(&self) -> bool {
        self.played.load(Ordering::SeqCst)
            && self.frames_served.load(Ordering::SeqCst) >= self.wake_at.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct Facts {
    bands_dropped: usize,
    bypass: bool,
    clipped_base: u64,
    clipped_delta_after_play: u64,
    correction_installed: bool,
    correction_rate_mismatch_hz: Option<f64>,
    engaged: bool,
    gain_db: f32,
    installed_preamp_lin: Option<f32>,
    latency_ms: Option<f64>,
    sections_substituted: usize,
    status_kind: EngineStatusKind,
    stream_rate_hz: Option<f64>,
    /// `None` models "no realtime block to read" — before the first start and
    /// after a teardown.
    tap_readable: bool,
}

impl Default for Facts {
    fn default() -> Self {
        Self {
            bands_dropped: 0,
            bypass: false,
            clipped_base: 0,
            clipped_delta_after_play: 0,
            correction_installed: true,
            correction_rate_mismatch_hz: None,
            engaged: true,
            gain_db: 0.0,
            installed_preamp_lin: None,
            latency_ms: Some(20.0),
            sections_substituted: 0,
            status_kind: EngineStatusKind::Idle,
            stream_rate_hz: Some(RATE_F),
            tap_readable: true,
        }
    }
}

#[derive(Clone)]
struct MockEngine {
    callbacks: Arc<AtomicU64>,
    facts: Arc<Mutex<Facts>>,
    flow: Flow,
    journal: Journal,
    nonzero: Arc<AtomicU64>,
}

impl MockEngine {
    fn new(facts: Facts, flow: Flow, journal: Journal) -> Self {
        Self {
            callbacks: Arc::new(AtomicU64::new(0)),
            facts: Arc::new(Mutex::new(facts)),
            flow,
            journal,
            nonzero: Arc::new(AtomicU64::new(0)),
        }
    }

    fn set_gain(&self, db: f32) {
        lock(&self.facts).gain_db = db;
    }
}

impl EngineFacts for MockEngine {
    fn bands_dropped(&self) -> usize {
        self.journal.record("facts.bands_dropped");
        lock(&self.facts).bands_dropped
    }

    fn bypass(&self) -> bool {
        self.journal.record("facts.bypass");
        lock(&self.facts).bypass
    }

    fn clipped_samples(&self) -> u64 {
        self.journal.record("facts.clipped_samples");
        let facts = lock(&self.facts);
        if self.flow.played.load(Ordering::SeqCst) {
            facts.clipped_base + facts.clipped_delta_after_play
        } else {
            facts.clipped_base
        }
    }

    fn correction_installed(&self) -> bool {
        self.journal.record("facts.correction_installed");
        lock(&self.facts).correction_installed
    }

    fn correction_rate_mismatch_hz(&self) -> Option<f64> {
        self.journal.record("facts.correction_rate_mismatch_hz");
        lock(&self.facts).correction_rate_mismatch_hz
    }

    fn engine_engaged(&self) -> bool {
        self.journal.record("facts.engine_engaged");
        lock(&self.facts).engaged
    }

    fn gain_db(&self) -> f32 {
        self.journal.record("facts.gain_db");
        lock(&self.facts).gain_db
    }

    fn installed_preamp_lin(&self) -> Option<f32> {
        self.journal.record("facts.installed_preamp_lin");
        lock(&self.facts).installed_preamp_lin
    }

    fn latency_ms(&self) -> Option<f64> {
        lock(&self.facts).latency_ms
    }

    fn sections_substituted(&self) -> usize {
        lock(&self.facts).sections_substituted
    }

    fn status_kind(&self) -> EngineStatusKind {
        lock(&self.facts).status_kind
    }

    fn stream_rate_hz(&self) -> Option<f64> {
        self.journal.record("facts.stream_rate_hz");
        lock(&self.facts).stream_rate_hz
    }

    fn tap_activity(&self) -> Option<TapActivity> {
        if !lock(&self.facts).tap_readable {
            return None;
        }
        // Every read advances `callbacks`, because a mic-capable default
        // output cycles the IOProc even with nothing playing — which is
        // exactly why `callbacks` is the wrong counter for the witness.
        let callbacks = self.callbacks.fetch_add(1, Ordering::SeqCst) + 1;
        let nonzero = if self.flow.flowing() {
            self.nonzero.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            self.nonzero.load(Ordering::SeqCst)
        };
        Some(TapActivity {
            callbacks,
            nonzero_blocks: nonzero,
        })
    }
}

#[derive(Clone)]
struct MockControl {
    engine: MockEngine,
    journal: Journal,
    /// Set to make the pin silently fail to land, so the read-back gate has
    /// something to catch.
    ignore_pin: Arc<AtomicBool>,
    lease_fails: Arc<AtomicBool>,
    leases_live: Arc<AtomicU64>,
}

impl MockControl {
    fn new(engine: MockEngine, journal: Journal) -> Self {
        Self {
            engine,
            journal,
            ignore_pin: Arc::new(AtomicBool::new(false)),
            lease_fails: Arc::new(AtomicBool::new(false)),
            leases_live: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl EngineControl for MockControl {
    fn acquire_measurement_lease(
        &mut self,
    ) -> Result<Box<dyn MeasurementLeaseToken>, MeasureError> {
        self.journal.record("control.acquire_lease");
        if self.lease_fails.load(Ordering::SeqCst) {
            return Err(MeasureError::Sink(
                "a measurement lease is already outstanding".to_owned(),
            ));
        }
        self.leases_live.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(MockLease {
            journal: self.journal.clone(),
            live: Arc::clone(&self.leases_live),
        }))
    }

    fn pin_gain_db(&mut self, db: f32) -> Result<GainPin, MeasureError> {
        self.journal.record("control.pin_gain");
        let previous = self.engine.gain_db();
        if !self.ignore_pin.load(Ordering::SeqCst) {
            self.engine.set_gain(db);
        }
        let engine = self.engine.clone();
        let journal = self.journal.clone();
        Ok(GainPin::new(
            previous,
            db,
            Box::new(move |restore_to| {
                journal.record("control.restore_gain");
                engine.set_gain(restore_to);
                Ok(())
            }),
        ))
    }
}

struct MockLease {
    journal: Journal,
    live: Arc<AtomicU64>,
}

impl MeasurementLeaseToken for MockLease {
    fn is_held(&self) -> bool {
        true
    }
}

impl Drop for MockLease {
    fn drop(&mut self) {
        self.journal.record("control.release_lease");
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct MockTap {
    excluded: Arc<AtomicBool>,
}

impl MockTap {
    fn new(excluded: bool) -> Self {
        Self {
            excluded: Arc::new(AtomicBool::new(excluded)),
        }
    }
}

impl TapStatus for MockTap {
    fn self_excluded(&self) -> bool {
        self.excluded.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct MockVolume {
    journal: Journal,
    scalar: f64,
    sets: Arc<Mutex<Vec<f64>>>,
}

impl MockVolume {
    fn new(scalar: f64, journal: Journal) -> Self {
        Self {
            journal,
            scalar,
            sets: Arc::default(),
        }
    }

    fn sets(&self) -> Vec<f64> {
        lock(&self.sets).clone()
    }
}

impl VolumeControl for MockVolume {
    fn volume(&self) -> Result<f64, MeasureError> {
        Ok(self.scalar)
    }

    fn set_volume(&mut self, scalar: f64) -> Result<(), MeasureError> {
        self.journal.record("volume.set");
        lock(&self.sets).push(scalar);
        Ok(())
    }
}

/// Serves a prepared recording block by block, counting frames so the witness
/// windows can be scripted against the file's own timeline.
#[derive(Clone)]
struct MockCapture {
    flow: Flow,
    journal: Journal,
    offset: Arc<AtomicU64>,
    recording: Arc<Vec<f64>>,
    stops: Arc<AtomicU64>,
}

impl MockCapture {
    fn new(recording: Vec<f64>, flow: Flow, journal: Journal) -> Self {
        Self {
            flow,
            journal,
            offset: Arc::new(AtomicU64::new(0)),
            recording: Arc::new(recording),
            stops: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl CaptureSource for MockCapture {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: 512,
            sample_rate_hz: RATE_F,
        }
    }

    fn capture(&mut self, block: &mut [f64]) -> Result<usize, MeasureError> {
        let start = self.offset.load(Ordering::SeqCst) as usize;
        if start >= self.recording.len() {
            return Ok(0);
        }
        let end = (start + block.len()).min(self.recording.len());
        let got = end - start;
        block[..got].copy_from_slice(&self.recording[start..end]);
        self.offset.store(end as u64, Ordering::SeqCst);
        self.flow.frames_served.store(end as u64, Ordering::SeqCst);
        Ok(got)
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        self.journal.record("capture.stop");
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The child's scripted stdout, plus a journal of every rung the ladder drove.
#[derive(Clone)]
struct ChildScript {
    /// Lines the child will emit, in order. An empty queue is EOF.
    lines: Arc<Mutex<Vec<String>>>,
    /// When true, `read_line` never reports EOF — the wedged child the
    /// teardown ladder exists for.
    never_exits: Arc<AtomicBool>,
    exit: Arc<Mutex<HelperExit>>,
    flow: Flow,
    journal: Journal,
    pid: u32,
}

impl ChildScript {
    fn new(flow: Flow, journal: Journal) -> Self {
        Self {
            lines: Arc::new(Mutex::new(Vec::new())),
            never_exits: Arc::new(AtomicBool::new(false)),
            exit: Arc::new(Mutex::new(HelperExit {
                code: Some(0),
                signalled: false,
            })),
            flow,
            journal,
            pid: 4242,
        }
    }

    fn push(&self, line: String) {
        lock(&self.lines).push(line);
    }
}

struct MockChild {
    script: ChildScript,
    reaped: Option<HelperExit>,
}

impl HelperProcess for MockChild {
    fn pid(&self) -> u32 {
        self.script.pid
    }

    fn request_abort(&mut self) -> Result<(), MeasureError> {
        self.script.journal.record("helper.request_abort");
        Ok(())
    }

    fn request_terminate(&mut self) -> Result<(), MeasureError> {
        self.script.journal.record("helper.request_terminate");
        Ok(())
    }

    fn kill(&mut self) -> Result<(), MeasureError> {
        self.script.journal.record("helper.kill");
        lock(&self.script.exit).signalled = true;
        self.script.never_exits.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn reap(&mut self) -> Result<HelperExit, MeasureError> {
        self.script.journal.record("helper.reap");
        if let Some(cached) = self.reaped {
            return Ok(cached);
        }
        let exit = *lock(&self.script.exit);
        self.reaped = Some(exit);
        Ok(exit)
    }

    fn send_play(&mut self) -> Result<(), MeasureError> {
        self.script.journal.record("helper.send_play");
        self.script.flow.played.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn read_line(&mut self, _deadline: Duration) -> Result<Option<String>, MeasureError> {
        let mut lines = lock(&self.script.lines);
        if lines.is_empty() {
            drop(lines);
            if self.script.never_exits.load(Ordering::SeqCst) {
                // A wedged child keeps talking. Sleep so the ladder's deadline
                // is a real deadline rather than a spin.
                std::thread::sleep(Duration::from_millis(1));
                return Ok(Some(
                    r#"{"event":"progress","frames_emitted":1}"#.to_owned(),
                ));
            }
            return Ok(None);
        }
        Ok(Some(lines.remove(0)))
    }
}

#[derive(Clone)]
struct MockHelper {
    /// Where to copy the WAV the parent handed over, at the moment it is
    /// handed over. That is the only instant it provably exists: the teardown
    /// ladder deletes it, which is what a per-run file is for.
    copy_to: Arc<Mutex<Option<PathBuf>>>,
    fail_spawn: Arc<AtomicBool>,
    script: ChildScript,
    spawns: Arc<AtomicU64>,
}

impl MockHelper {
    fn new(script: ChildScript) -> Self {
        Self {
            copy_to: Arc::new(Mutex::new(None)),
            fail_spawn: Arc::new(AtomicBool::new(false)),
            script,
            spawns: Arc::new(AtomicU64::new(0)),
        }
    }

    fn spawns(&self) -> u64 {
        self.spawns.load(Ordering::SeqCst)
    }
}

impl paraeq_measure::StimulusHelper for MockHelper {
    fn spawn(
        &mut self,
        wav_path: &std::path::Path,
        _device_uid: &str,
        _routing: HelperRouting,
    ) -> Result<Box<dyn HelperProcess>, MeasureError> {
        self.script.journal.record("helper.spawn");
        self.spawns.fetch_add(1, Ordering::SeqCst);
        if self.fail_spawn.load(Ordering::SeqCst) {
            return Err(MeasureError::Sink("no such helper binary".to_owned()));
        }
        assert!(
            wav_path.exists(),
            "the parent must have written the WAV before spawning"
        );
        if let Some(destination) = lock(&self.copy_to).as_ref() {
            std::fs::copy(wav_path, destination).expect("copy the handed-over WAV");
        }
        Ok(Box::new(MockChild {
            script: self.script.clone(),
            reaped: None,
        }))
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────────

fn healthy_cal() -> CalSummary {
    CalSummary::validate(
        TransducerClass::OverEar,
        "UMIK-1 #7001234 (umik-7001234.txt)".to_owned(),
        CalSensitivity::Parsed(-18.0),
        1.0,
    )
    .expect("healthy cal validates")
}

fn peaking(fc: f64, gain_db: f64, q: f64) -> EQBand {
    EQBand {
        filter_type: FilterType::Peaking,
        fc,
        gain_db,
        q,
    }
}

/// One channel, one +6 dB boost: `preamp_db ≈ −6`, so `L_verify` sits 6 dB
/// below `L_measure` and the level book has something to be wrong about.
fn boost_plan() -> VerifyPlan {
    VerifyPlan {
        bands: vec![vec![peaking(1_000.0, 6.0, 1.0)]],
        design_rate_hz: RATE_F,
        preamp_db: -6.0,
    }
}

fn cut_plan() -> VerifyPlan {
    VerifyPlan {
        bands: vec![vec![peaking(1_000.0, -6.0, 1.0)]],
        design_rate_hz: RATE_F,
        preamp_db: 0.0,
    }
}

/// The engine's own recomputation, done the way the gate does it: the plan's
/// bands, at the LIVE rate, folded `min` across channels.
fn recomputed_preamp_db(plan: &VerifyPlan, rate_hz: f64) -> f64 {
    plan.bands
        .iter()
        .map(|channel| {
            ParametricEQ {
                bands: channel.clone(),
                sample_rate: rate_hz,
            }
            .preamp_db()
        })
        .fold(0.0f64, f64::min)
}

/// The engine's own `preamp_lin`, transcribed — non-finite and non-negative
/// are unity.
fn armed_preamp_lin(db: f64) -> f32 {
    if !db.is_finite() || db >= 0.0 {
        1.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

/// A compact bracket. The shipped 2.20 s layout costs an O(N·M) correlation
/// per test and buys no extra coverage of anything under test here.
fn layout() -> MarkerLayout {
    MarkerLayout {
        guard_gap_s: 0.02,
        lead_in_s: 0.03,
        marker_s: 0.01,
        markers_per_end: 2,
        pair_gap_s: 0.02,
        tail_s: 0.03,
    }
}

fn shape() -> SweepShape {
    SweepShape {
        duration_s: 0.20,
        f_end_hz: 8_000.0,
        f_start_hz: 200.0,
        sample_rate_hz: RATE,
    }
}

fn fast_timing() -> VerifyTiming {
    VerifyTiming {
        abort_slack_ms: 1.0,
        capture_tail_s: 0.02,
        helper_done_ms: 2_000,
        helper_ready_ms: 2_000,
        sigterm_deadline_ms: 15,
        tap_poll_ms: 1,
        window_a_ms: 5,
        window_b_ms: 20,
    }
}

/// `L_measure` for these tests: the OverEar class cap, with a MATCHING gain
/// read-back so the MS-11 margin is not live unless a test makes it so.
fn l_measure() -> SweepLevel {
    SweepLevel::new(-20.0, TransducerClass::OverEar).expect("a legal level")
}

/// The `ready` line the child emits, at the rates the fences expect.
fn ready_line(routing: &str, rate: f64, sub_rate: f64) -> String {
    format!(
        r#"{{"event":"ready","channels":2,"device_uid":"out-1","frames_per_block":512,"render_device_uid":"com.paraeq.render.4242","routing":"{routing}","sample_rate_hz":{rate},"sub_device_sample_rate_hz":{sub_rate}}}"#
    )
}

fn done_line() -> String {
    r#"{"event":"done","clamped":0,"frames_emitted":123,"sanitized":0,"underrun_frames":0}"#
        .to_owned()
}

/// What the mic would hear: the verification file, through the correction the
/// plan describes, at the armed preamp. Built from the same primitives the
/// pass uses, so the correction-filtered matched-filter template it builds is
/// the template that actually matches.
fn recording(level: SweepLevel, plan: &VerifyPlan, layout: &MarkerLayout, pad_s: f64) -> Vec<f64> {
    let shape = shape();
    let sweep = assemble_sweep(
        shape.duration_s,
        shape.sample_rate_hz,
        shape.f_start_hz,
        shape.f_end_hz,
        level,
    )
    .expect("a legal sweep");
    let (bracketed, _span) =
        assemble_bracketed(sweep, layout, StimulusKind::VerificationSweep).expect("brackets");
    let eq = ParametricEQ {
        bands: plan.bands[0].clone(),
        sample_rate: RATE_F,
    };
    let gain = f64::from(armed_preamp_lin(recomputed_preamp_db(plan, RATE_F)));
    let mut out = eq.apply_offline(bracketed.samples(), RATE_F);
    for v in out.iter_mut() {
        *v *= gain;
    }
    out.extend(std::iter::repeat_n(0.0, (pad_s * RATE_F) as usize));
    out
}

/// Everything a test needs to drive one pass and then look at what happened.
struct Rig {
    capture: MockCapture,
    control: MockControl,
    engine: MockEngine,
    flow: Flow,
    helper: MockHelper,
    journal: Journal,
    request: VerifyRequest,
    script: ChildScript,
    tap: MockTap,
    volume: MockVolume,
}

impl Rig {
    fn new(test: &str, scratch: &Scratch, plan: VerifyPlan) -> Rig {
        let journal = Journal::default();
        let flow = Flow::default();
        let layout = layout();
        let preamp = recomputed_preamp_db(&plan, RATE_F);
        let level =
            SweepLevel::new(l_measure().dbfs_rms() + preamp, TransducerClass::OverEar).unwrap();
        let facts = Facts {
            gain_db: 0.0,
            installed_preamp_lin: Some(armed_preamp_lin(preamp)),
            ..Facts::default()
        };
        let engine = MockEngine::new(facts, flow.clone(), journal.clone());
        let control = MockControl::new(engine.clone(), journal.clone());
        let script = ChildScript::new(flow.clone(), journal.clone());
        script.push(ready_line("0", RATE_F, RATE_F));
        script.push(done_line());
        let helper = MockHelper::new(script.clone());
        let capture = MockCapture::new(
            recording(level, &plan, &layout, 0.05),
            flow.clone(),
            journal.clone(),
        );
        let volume = MockVolume::new(PRE_VOLUME, journal.clone());
        let _ = test;
        let request = VerifyRequest {
            baseline_routing: HelperRouting::Only(0),
            cal: healthy_cal(),
            device_uid: "out-1".to_owned(),
            input_gain_read_back: 1.0,
            l_measure: l_measure(),
            l_measure_projected_spl_db: 84.0,
            layout,
            noise_floor_dbfs: -90.0,
            plan,
            position_index: 0,
            routing: HelperRouting::Only(0),
            sweep: shape(),
            timing: fast_timing(),
            wav_path: scratch.wav(),
        };
        Rig {
            capture,
            control,
            engine,
            flow,
            helper,
            journal,
            request,
            script,
            tap: MockTap::new(true),
            volume,
        }
    }

    fn seam(&self) -> VerifySeam {
        VerifySeam {
            capture: Box::new(self.capture.clone()),
            control: Box::new(self.control.clone()),
            engine: Box::new(self.engine.clone()),
            helper: Box::new(self.helper.clone()),
            tap: Box::new(self.tap.clone()),
            volume: Box::new(self.volume.clone()),
        }
    }

    fn arm(&self) -> Result<VerificationPass, paraeq_measure::ArmingFailure> {
        VerificationPass::arm(self.request_clone(), self.seam())
    }

    /// `VerifyRequest` is not `Clone` (a `CalSummary` is, but the struct is
    /// deliberately not, so a request cannot be reused across two passes by
    /// accident). Rebuild it instead.
    fn request_clone(&self) -> VerifyRequest {
        VerifyRequest {
            baseline_routing: self.request.baseline_routing,
            cal: self.request.cal.clone(),
            device_uid: self.request.device_uid.clone(),
            input_gain_read_back: self.request.input_gain_read_back,
            l_measure: self.request.l_measure,
            l_measure_projected_spl_db: self.request.l_measure_projected_spl_db,
            layout: self.request.layout,
            noise_floor_dbfs: self.request.noise_floor_dbfs,
            plan: self.request.plan.clone(),
            position_index: self.request.position_index,
            routing: self.request.routing,
            sweep: self.request.sweep,
            timing: self.request.timing,
            wav_path: self.request.wav_path.clone(),
        }
    }

    /// Arm and acknowledge — the shortest legal path to a spawn.
    fn acknowledged(&self) -> VerificationPass {
        let mut pass = self.arm().expect("a healthy rig arms");
        let projected = pass.projected_spl_db();
        pass.acknowledge("External Headphone Amp", projected)
            .expect("ack records");
        pass
    }
}

fn refusal_of(error: &VerifyError) -> D {
    match error {
        VerifyError::Refused(refusal) => refusal.diagnostic(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// ── Gates 1 and 2 ─────────────────────────────────────────────────────────

/// R15's falsifier. The verification pre-roll REQUIRES a quiet machine, and
/// the shipped watchdog only reports `Running` while nonzero input is
/// advancing — so a gate written against the literal word would refuse a
/// perfectly healthy engine on every run. Only the three states in which no
/// audio can be processed at all refuse, and none of them spawns anything.
#[test]
fn verify_refuses_before_spawning_only_for_the_three_disengaged_states() {
    let scratch = Scratch::new("disengaged");
    let disengaged = [
        EngineStatusKind::AutoDisabledNoInput,
        EngineStatusKind::Failed,
        EngineStatusKind::Stopped,
    ];
    for kind in [
        EngineStatusKind::AutoDisabledNoInput,
        EngineStatusKind::Failed,
        EngineStatusKind::Idle,
        EngineStatusKind::InputSilent,
        EngineStatusKind::NoInputDetected,
        EngineStatusKind::Running,
        EngineStatusKind::Starting,
        EngineStatusKind::Stopped,
    ] {
        let rig = Rig::new("disengaged", &scratch, boost_plan());
        {
            let mut facts = lock(&rig.engine.facts);
            facts.status_kind = kind;
            facts.engaged = !disengaged.contains(&kind);
        }
        match rig.arm() {
            Ok(_pass) => assert!(
                !disengaged.contains(&kind),
                "{kind:?} must refuse, and it armed"
            ),
            Err(failure) => {
                assert!(
                    disengaged.contains(&kind),
                    "{kind:?} is engaged and must arm, got {:?}",
                    failure.error
                );
                assert_eq!(refusal_of(&failure.error), D::EngineNotRunning);
            }
        }
        assert_eq!(rig.helper.spawns(), 0, "{kind:?}: nothing may be spawned");
    }

    // And bypass refuses in every case: a bypassed chain measures the
    // uncorrected path a second time.
    for kind in [EngineStatusKind::Running, EngineStatusKind::Idle] {
        let rig = Rig::new("disengaged", &scratch, boost_plan());
        {
            let mut facts = lock(&rig.engine.facts);
            facts.status_kind = kind;
            facts.bypass = true;
        }
        let failure = rig.arm().expect_err("bypass refuses");
        assert_eq!(refusal_of(&failure.error), D::EngineNotRunning);
        assert_eq!(rig.helper.spawns(), 0);
    }
}

/// The regression test for the exact contradiction R15 resolves: status `Idle`
/// with a stationary realtime counter is BOTH the engine state gate 2 must
/// accept AND the quiet machine Window A requires. One test, both halves.
#[test]
fn a_quiet_idle_engine_is_engaged_and_verification_arms() {
    let scratch = Scratch::new("quiet-idle");
    let rig = Rig::new("quiet-idle", &scratch, boost_plan());
    lock(&rig.engine.facts).status_kind = EngineStatusKind::Idle;
    let pass = rig.acknowledged();
    assert_eq!(pass.phase(), VerifyPhase::Acknowledged);

    // Window A is satisfied by the SAME stationarity: nothing has played, so
    // `nonzero_blocks` cannot advance.
    assert!(!rig.flow.flowing());
    let mut pass = pass;
    pass.run().expect("a quiet idle engine verifies");
}

/// R15(b): the lease suspends the fail-open watchdog, and the auto-disable it
/// guards against is STICKY, so a lease taken after the first engine read
/// covers a window that has already closed.
#[test]
fn the_lease_is_acquired_before_any_engine_fact_is_read() {
    let scratch = Scratch::new("lease-first");
    let rig = Rig::new("lease-first", &scratch, boost_plan());
    let _pass = rig.acknowledged();
    let calls = rig.journal.calls();
    let lease = calls
        .iter()
        .position(|c| c == "control.acquire_lease")
        .expect("the lease is acquired");
    let first_fact = calls
        .iter()
        .position(|c| c.starts_with("facts."))
        .expect("some engine fact is read");
    assert!(
        lease < first_fact,
        "the lease must precede every engine read: {calls:?}"
    );
}

/// A lease that cannot be acquired is NOT given a diagnostic of its own: the
/// implementation's own message distinguishes "the engine is gone" from "one
/// measurement at a time", and those have different remedies. Nothing is
/// spawned and no engine fact is read.
#[test]
fn a_refused_lease_stops_the_pass_before_any_engine_fact_is_read() {
    let scratch = Scratch::new("lease-refused");
    let rig = Rig::new("lease-refused", &scratch, boost_plan());
    rig.control.lease_fails.store(true, Ordering::SeqCst);
    let failure = rig.arm().expect_err("a refused lease stops the pass");
    assert!(
        matches!(failure.error, VerifyError::Seam(_)),
        "got {:?}",
        failure.error
    );
    assert!(
        !rig.journal.calls().iter().any(|c| c.starts_with("facts.")),
        "no engine fact may be read without a lease: {:?}",
        rig.journal.calls()
    );
    assert_eq!(rig.helper.spawns(), 0);
}

/// R16 (b2). A cascade the engine could only partially install is not the
/// plan's cascade, and predicting the plan's response over it would blame the
/// chain for our own prediction fault.
#[test]
fn a_dropped_band_refuses_before_spawning() {
    let scratch = Scratch::new("bands-dropped");
    let rig = Rig::new("bands-dropped", &scratch, boost_plan());
    lock(&rig.engine.facts).bands_dropped = 2;
    let failure = rig.arm().expect_err("a dropped band refuses");
    match refusal_of(&failure.error) {
        D::VerificationBandsDropped { dropped, rate_hz } => {
            assert_eq!(dropped, 2, "the refusal names the count");
            assert_eq!(rate_hz, RATE_F, "and the live rate");
        }
        other => panic!("expected VerificationBandsDropped, got {other:?}"),
    }
    assert_eq!(rig.helper.spawns(), 0, "zero spawns");
}

/// R16 (b3), and it is written against the defect it exists to prevent: the
/// engine computes its preamp at the LIVE rate, `decide()` computed the plan's
/// at the DESIGN rate, and comparing against the plan's number would refuse a
/// correct engine whenever the two rates differ.
#[test]
fn the_preamp_gate_compares_against_the_live_rate_recomputation_not_the_plan() {
    let scratch = Scratch::new("preamp-live-rate");
    let mut plan = boost_plan();
    // The plan was designed at 44.1 kHz and carries that rate's preamp; the
    // engine runs at 48 kHz.
    plan.design_rate_hz = 44_100.0;
    plan.preamp_db = recomputed_preamp_db(&plan, 44_100.0);
    let live = recomputed_preamp_db(&plan, RATE_F);
    assert_ne!(
        plan.preamp_db, live,
        "the two rates must disagree, or this test proves nothing"
    );

    let rig = Rig::new("preamp-live-rate", &scratch, plan);
    // The engine arms the LIVE-rate number, which is what it actually does.
    lock(&rig.engine.facts).installed_preamp_lin = Some(armed_preamp_lin(live));
    rig.arm().expect("the live-rate recomputation agrees");

    // And a chain arming the PLAN's number — i.e. the naive compare — refuses.
    let rig = Rig::new("preamp-live-rate", &scratch, {
        let mut p = boost_plan();
        p.design_rate_hz = 44_100.0;
        p.preamp_db = recomputed_preamp_db(&p, 44_100.0);
        p
    });
    lock(&rig.engine.facts).installed_preamp_lin = Some(armed_preamp_lin(-3.0));
    let failure = rig.arm().expect_err("a disagreeing preamp refuses");
    assert!(matches!(
        refusal_of(&failure.error),
        D::VerificationPreampMismatch { .. }
    ));
    assert_eq!(rig.helper.spawns(), 0);
}

/// `None` means NO CORRECTION IS INSTALLED, and it must never be defaulted to
/// 1.0: a silently-unity preamp is precisely the failure verification exists
/// to catch.
#[test]
fn an_installed_preamp_of_none_with_a_correction_installed_refuses() {
    let scratch = Scratch::new("preamp-none");
    let rig = Rig::new("preamp-none", &scratch, boost_plan());
    {
        let mut facts = lock(&rig.engine.facts);
        facts.correction_installed = true;
        facts.installed_preamp_lin = None;
    }
    let failure = rig.arm().expect_err("a None preamp refuses");
    assert!(matches!(
        refusal_of(&failure.error),
        D::VerificationPreampMismatch { .. }
    ));
    assert_eq!(rig.helper.spawns(), 0);
}

/// The chain is running coefficients designed for another rate — the wizard's
/// own named failure, "stale coefficients after a rate change". Refused before
/// spawning, and with the code whose doc carries both clauses.
#[test]
fn a_correction_rate_mismatch_refuses_before_spawning() {
    let scratch = Scratch::new("rate-mismatch");
    let rig = Rig::new("rate-mismatch", &scratch, boost_plan());
    lock(&rig.engine.facts).correction_rate_mismatch_hz = Some(44_100.0);
    let failure = rig.arm().expect_err("a rate mismatch refuses");
    assert!(matches!(
        refusal_of(&failure.error),
        D::OutputRateChangedDuringVerify { .. }
    ));
    assert_eq!(rig.helper.spawns(), 0);
}

// ── Gate 3 ────────────────────────────────────────────────────────────────

/// The inversion, written down so nobody copies MS-6 into the wrong place:
/// `self_excluded` is NOT re-checked for the VERIFICATION capture — the tap
/// must SEE the helper, which is never excluded either way. What this gate
/// protects is the BASELINE the verification capture will be differenced
/// against: if exclusion has vanished, `measured_uncorrected` was itself
/// corrected and the subtraction is garbage.
#[test]
fn a_lost_self_exclusion_witness_refuses_before_spawning() {
    let scratch = Scratch::new("self-exclusion");
    let rig = Rig::new("self-exclusion", &scratch, boost_plan());
    rig.tap.excluded.store(false, Ordering::SeqCst);
    let failure = rig.arm().expect_err("a lost witness refuses");
    assert_eq!(refusal_of(&failure.error), D::SelfExclusionUnavailable);
    assert_eq!(rig.helper.spawns(), 0);
}

// ── Gate 4: the user's trim ───────────────────────────────────────────────

/// T4. `G_user = +10` against a `+6` dB peak boost would make the verification
/// sweep LOUDER than the baseline the level ladder validated — a safety
/// failure, not an accounting one. So it is pinned to 0.0, READ BACK, and
/// restored on every exit path.
#[test]
fn the_user_gain_stage_is_pinned_and_restored() {
    let scratch = Scratch::new("gain-pin");
    let rig = Rig::new("gain-pin", &scratch, boost_plan());
    rig.engine.set_gain(10.0);

    {
        let mut pass = rig.acknowledged();
        assert_eq!(rig.engine.gain_db(), 0.0, "pinned for the pass");
        pass.run().expect("the pass runs");
    }
    assert_eq!(rig.engine.gain_db(), 10.0, "restored on command exit");

    // Drop, without a run.
    rig.engine.set_gain(10.0);
    {
        let _pass = rig.acknowledged();
        assert_eq!(rig.engine.gain_db(), 0.0);
    }
    assert_eq!(rig.engine.gain_db(), 10.0, "restored on drop");

    // And under a panic, which is the path `Drop` exists for.
    rig.engine.set_gain(10.0);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _pass = rig.acknowledged();
        panic!("injected: verification blew up with the trim pinned");
    }));
    assert!(unwind.is_err(), "the injected panic must propagate");
    assert_eq!(rig.engine.gain_db(), 10.0, "restored during unwinding");
}

/// A mock that ignores the pin must be caught by the read-back, with zero
/// spawns. Never assume the write landed.
#[test]
fn a_pin_that_does_not_land_refuses_with_zero_spawns() {
    let scratch = Scratch::new("gain-unpinned");
    let rig = Rig::new("gain-unpinned", &scratch, boost_plan());
    rig.engine.set_gain(10.0);
    rig.control.ignore_pin.store(true, Ordering::SeqCst);
    let failure = rig.arm().expect_err("an unpinned trim refuses");
    match refusal_of(&failure.error) {
        D::VerificationGainNotPinned { read_back_db } => {
            assert_eq!(read_back_db, 10.0, "the refusal names what it saw")
        }
        other => panic!("expected VerificationGainNotPinned, got {other:?}"),
    }
    assert_eq!(rig.helper.spawns(), 0);
}

// ── Gate 5: the level book ────────────────────────────────────────────────

/// `L_verify = L_measure + preamp_db`, constructed through `SweepLevel::new`
/// so the class cap and the absolute ceiling both bind, and never above
/// `L_measure`.
#[test]
fn l_verify_is_l_measure_minus_the_realized_peak_boost() {
    let scratch = Scratch::new("l-verify");

    let rig = Rig::new("l-verify", &scratch, boost_plan());
    let pass = rig.arm().expect("arms");
    let level = pass.level().expect("armed implies a level").dbfs_rms();
    assert!(
        (level - (l_measure().dbfs_rms() - 6.0)).abs() < 0.05,
        "a +6 dB realized peak must cost exactly 6 dB, got {level}"
    );
    assert!(level <= l_measure().dbfs_rms());

    // A pure cut needs no headroom, so the level is unchanged.
    let rig = Rig::new("l-verify", &scratch, cut_plan());
    let pass = rig.arm().expect("arms");
    let level = pass.level().expect("armed implies a level").dbfs_rms();
    assert_eq!(
        level,
        l_measure().dbfs_rms(),
        "a pure-cut correction costs nothing"
    );
}

/// T2, the safety half, and the SECOND assertion is the one that matters: it
/// fails if anyone "optimizes away" MS-19's reduction on the grounds that the
/// preamp already cancels the boost. A safety interlock may not be
/// conditioned on the hypothesis it is testing — in exactly the failure case
/// this pass hunts for (the preamp not applied), the reduction is the only
/// thing keeping the stimulus at or below the validated baseline.
///
/// A deterministic grid rather than a proptest, because `proptest` is not a
/// dependency of this crate and a seeded sweep over filter type × gain × Q is
/// the same coverage without one.
#[test]
fn the_verification_stimulus_is_never_louder_than_the_baseline_at_any_bin() {
    for filter_type in [
        FilterType::HighShelf,
        FilterType::LowShelf,
        FilterType::Peaking,
    ] {
        for gain_db in [-20.0, -6.0, -0.5, 0.0, 0.5, 6.0, 12.0, 20.0] {
            for q in [0.5, 1.0, 4.0, 20.0] {
                for fc in [40.0, 1_000.0, 12_000.0] {
                    let bands = vec![EQBand {
                        filter_type,
                        fc,
                        gain_db,
                        q,
                    }];
                    let eq = ParametricEQ {
                        bands: bands.clone(),
                        sample_rate: RATE_F,
                    };
                    let preamp_db = eq.preamp_db();
                    let grid = eq.preamp_grid();
                    let response = eq.realized_response(&grid, RATE_F);
                    // `L_verify − L_measure` is exactly `preamp_db`.
                    let delta = preamp_db;
                    for h in response {
                        // The healthy chain: the level reduction plus the
                        // chain's own preamp plus the band response.
                        assert!(
                            delta + preamp_db + h <= 1e-6,
                            "{filter_type:?} {gain_db} dB Q{q} @{fc}: healthy chain \
                             {}",
                            delta + preamp_db + h
                        );
                        // The PREAMP-DROPPED chain — the bug under test. This
                        // is the assertion MS-19's reduction exists for.
                        assert!(
                            delta + h <= 1e-6,
                            "{filter_type:?} {gain_db} dB Q{q} @{fc}: preamp-dropped \
                             chain {}",
                            delta + h
                        );
                    }
                }
            }
        }
    }
}

/// R22 (b)'s falsifier, and it fails in BOTH directions. Driven with a
/// NON-MATCHING pinned gain so the MS-11 margin is live, over a correct
/// engine.
#[test]
fn the_cal_margin_is_applied_exactly_once_across_the_two_levels() {
    let scratch = Scratch::new("margin-once");
    let solved_dbfs_rms = -20.0;
    let cal = healthy_cal();
    // A read-back that disagrees with the cal's reference gain: the case the
    // margin exists for, and the COMMON one.
    let read_back = 0.5;
    let pin = cal.pin_gain(read_back);
    assert!(!pin.matches(), "the margin must be live for this test");
    // `install_solve` is this function's one caller, and `L_measure` is its
    // result — post-margin by construction.
    let l_measure_dbfs = margined_emit_dbfs(solved_dbfs_rms, pin);
    assert_eq!(l_measure_dbfs, solved_dbfs_rms - CAL_ERROR_MARGIN_DB);

    let mut rig = Rig::new("margin-once", &scratch, boost_plan());
    rig.request.input_gain_read_back = read_back;
    rig.request.l_measure =
        SweepLevel::new(l_measure_dbfs, TransducerClass::OverEar).expect("a legal level");
    let pass = rig.arm().expect("arms");
    let l_verify = pass.level().expect("armed implies a level").dbfs_rms();
    let preamp_db = recomputed_preamp_db(&rig.request.plan, RATE_F);

    // (i) `K` is EXACTLY `−preamp_db`, hence a zero residual. A second
    // `margined_emit_dbfs` call on the verification side breaks this by
    // `+CAL_ERROR_MARGIN_DB`.
    let k = l_measure_dbfs - l_verify;
    assert!(
        (k - -preamp_db).abs() < 1e-12,
        "K must be exactly −preamp_db, got {k} vs {}",
        -preamp_db
    );
    // (ii) The margin IS in `L_verify`, inherited rather than re-applied.
    // Skipping it on both sides breaks this by `−CAL_ERROR_MARGIN_DB`.
    assert!(
        (l_verify - (solved_dbfs_rms - CAL_ERROR_MARGIN_DB + preamp_db)).abs() < 1e-12,
        "L_verify must carry the margin exactly once, got {l_verify}"
    );
}

/// The control: the margin is not supposed to move `K` at all. It moves BOTH
/// levels together, which is the whole point.
#[test]
fn a_matching_pinned_gain_produces_the_same_k() {
    let scratch = Scratch::new("margin-control");
    let rig = Rig::new("margin-control", &scratch, boost_plan());
    let pin = rig.request.cal.pin_gain(rig.request.input_gain_read_back);
    assert!(pin.matches(), "this rig's read-back matches");
    let pass = rig.arm().expect("arms");
    let l_verify = pass.level().expect("a level").dbfs_rms();
    let preamp_db = recomputed_preamp_db(&rig.request.plan, RATE_F);
    let k = rig.request.l_measure.dbfs_rms() - l_verify;
    assert!(
        (k - -preamp_db).abs() < 1e-12,
        "K is −preamp_db with or without the margin, got {k}"
    );
}

// ── Gate 6: the SNR budget ────────────────────────────────────────────────

/// A level that cannot produce a meaningful residual is refused BEFORE
/// spawning, rather than played and then reported as a shape failure. The one
/// remedy that would fix it is the one MS-8 forbids.
#[test]
fn a_level_below_the_snr_budget_refuses_before_spawning() {
    let scratch = Scratch::new("snr-budget");
    let mut rig = Rig::new("snr-budget", &scratch, boost_plan());
    rig.request.noise_floor_dbfs = -30.0;
    let failure = rig.arm().expect_err("an unusable level refuses");
    match refusal_of(&failure.error) {
        D::VerificationLevelBelowSnrBudget { projected_snr_db } => assert!(
            projected_snr_db < 30.0,
            "the refusal names the shortfall, got {projected_snr_db}"
        ),
        other => panic!("expected VerificationLevelBelowSnrBudget, got {other:?}"),
    }
    assert_eq!(rig.helper.spawns(), 0);
    assert!(
        rig.volume.sets().contains(&PRE_VOLUME),
        "the volume is restored even on the cheapest refusal"
    );
}

// ── MS-18 ─────────────────────────────────────────────────────────────────

/// MS-18's own named test: "a state-machine unit test asserting the sweep
/// state is unreachable without the ack". The verification pass is a real
/// sweep into a transducer at up to `L_measure`, so MS-18 binds it exactly as
/// it binds the Direct path.
#[test]
fn the_verification_sweep_state_is_unreachable_without_the_ack() {
    let scratch = Scratch::new("ms18");
    let rig = Rig::new("ms18", &scratch, boost_plan());
    let mut pass = rig.arm().expect("arms");
    assert_eq!(pass.phase(), VerifyPhase::Armed);

    let error = pass.run().unwrap_err();
    assert!(
        matches!(
            error,
            VerifyError::WrongPhase {
                actual: VerifyPhase::Armed,
                ..
            }
        ),
        "got {error:?}"
    );
    assert_eq!(rig.helper.spawns(), 0, "no ack, no process");

    // An ack that names no device, and a stale one, both refuse.
    let mut pass = rig.arm().expect("arms");
    assert!(matches!(
        pass.acknowledge("   ", pass.projected_spl_db()),
        Err(VerifyError::AckNamesNoDevice)
    ));
    let stale = pass.projected_spl_db() + 5.0;
    assert!(matches!(
        pass.acknowledge("Amp", stale),
        Err(VerifyError::AckSplStale { .. })
    ));
    assert_eq!(pass.phase(), VerifyPhase::Armed);
    assert_eq!(rig.helper.spawns(), 0);
}

/// E2's second half, homed. `Direct` captures are tap-excluded BY DESIGN, so
/// the tap sees zeros, and `NoInputDetected` means "no nonzero sample ever
/// captured since start" — a literal reading of "a genuine TCC silent failure
/// is still a Refuse" would refuse 100 % of wizard runs.
///
/// Nothing in this crate gates a Direct capture on an engine status at all —
/// `MeasurementSession` reads no `EngineFacts` — so the only place the
/// reinterpretation could go wrong is here, and here `NoInputDetected` is an
/// ENGAGED state that arms and runs.
#[test]
fn no_input_detected_does_not_block_a_direct_capture() {
    let scratch = Scratch::new("no-input");
    let rig = Rig::new("no-input", &scratch, boost_plan());
    lock(&rig.engine.facts).status_kind = EngineStatusKind::NoInputDetected;
    let mut pass = rig.acknowledged();
    pass.run()
        .expect("NoInputDetected is engaged, not disqualifying");
    // And the stronger statement, which is what actually homes E2's second
    // half: `MeasurementSession` — the Direct capture path — reads NO engine
    // fact at all, so there is no `NoInputDetected` gate for it to carry.
    let source = include_str!("../src/session.rs");
    assert!(
        !source.contains("EngineFacts"),
        "the Direct capture path must read no engine fact"
    );
}

// ── Gate 8 and gate 9 ─────────────────────────────────────────────────────

/// The fence reads the device the CHILD actually opened, echoed in `ready` —
/// not the default output. Without that the child's own rate assert and the
/// parent's fence read different devices, and the child exits 3 on every run
/// while the failure reads as a WAV bug.
#[test]
fn a_render_device_at_the_wrong_rate_refuses_and_names_which_rate() {
    let scratch = Scratch::new("rate-fence");
    for (render, sub) in [(44_100.0, RATE_F), (RATE_F, 44_100.0)] {
        let rig = Rig::new("rate-fence", &scratch, boost_plan());
        lock(&rig.script.lines).clear();
        rig.script.push(ready_line("0", render, sub));
        rig.script.push(done_line());
        let mut pass = rig.acknowledged();
        let error = pass.run().unwrap_err();
        match refusal_of(&error) {
            D::OutputRateChangedDuringVerify {
                expected_hz,
                observed_hz,
            } => {
                assert_eq!(expected_hz, RATE_F);
                assert_eq!(observed_hz, 44_100.0, "the refusal names WHICH rate");
            }
            other => panic!("expected OutputRateChangedDuringVerify, got {other:?}"),
        }
        assert_eq!(
            rig.journal.index_of("helper.send_play"),
            None,
            "never played"
        );
    }
}

/// Gate 9's echoed half: the child must have understood the channel it was
/// asked for. Refused before `play`.
#[test]
fn a_helper_that_echoes_the_wrong_channel_refuses_before_play() {
    let scratch = Scratch::new("routing-echo");
    let rig = Rig::new("routing-echo", &scratch, boost_plan());
    lock(&rig.script.lines).clear();
    rig.script.push(ready_line("1", RATE_F, RATE_F));
    rig.script.push(done_line());
    let mut pass = rig.acknowledged();
    let error = pass.run().unwrap_err();
    assert_eq!(refusal_of(&error), D::HelperRoutingMismatch);
    assert_eq!(rig.journal.index_of("helper.send_play"), None);
}

/// R20's pre-spawn half. A `Both` capture heard the SUM of every output
/// channel, and a sum of differently-corrected channels is not the response of
/// any one band set. The parent holds the plan and the routing at gate time,
/// so there is no reason to burn a sweep discovering it.
#[test]
fn a_both_routing_over_divergent_bands_refuses_before_spawning() {
    let scratch = Scratch::new("routing-both");
    let divergent = VerifyPlan {
        bands: vec![
            vec![peaking(1_000.0, 6.0, 1.0)],
            vec![peaking(2_000.0, 6.0, 1.0)],
        ],
        design_rate_hz: RATE_F,
        preamp_db: -6.0,
    };
    let mut rig = Rig::new("routing-both", &scratch, divergent);
    rig.request.baseline_routing = HelperRouting::Both;
    rig.request.routing = HelperRouting::Both;
    let failure = rig.arm().expect_err("divergent bands refuse");
    assert_eq!(refusal_of(&failure.error), D::HelperRoutingMismatch);
    assert_eq!(rig.helper.spawns(), 0, "zero spawns");

    // The other half of the rule: identical band sets under `Both` are fine,
    // so the refusal is not over-broad.
    let identical = VerifyPlan {
        bands: vec![
            vec![peaking(1_000.0, 6.0, 1.0)],
            vec![peaking(1_000.0, 6.0, 1.0)],
        ],
        design_rate_hz: RATE_F,
        preamp_db: -6.0,
    };
    let mut rig = Rig::new("routing-both", &scratch, identical);
    rig.request.baseline_routing = HelperRouting::Both;
    rig.request.routing = HelperRouting::Both;
    lock(&rig.script.lines).clear();
    rig.script.push(ready_line("both", RATE_F, RATE_F));
    rig.script.push(done_line());
    rig.arm()
        .expect("identical bands under Both are predictable");
}

/// A routing that does not match the baseline's at this position cannot be
/// differenced against it.
#[test]
fn a_routing_that_disagrees_with_the_baseline_refuses_before_spawning() {
    let scratch = Scratch::new("routing-baseline");
    let mut rig = Rig::new("routing-baseline", &scratch, boost_plan());
    rig.request.baseline_routing = HelperRouting::Only(1);
    let failure = rig.arm().expect_err("a routing disagreement refuses");
    assert_eq!(refusal_of(&failure.error), D::HelperRoutingMismatch);
    assert_eq!(rig.helper.spawns(), 0);
}

// ── The E2 witness windows ────────────────────────────────────────────────

/// Window A. The tap is global and excludes only ParaEQ, so any other
/// application's audio lands in the corrected capture and is measured as part
/// of the correction's result.
#[test]
fn concurrent_system_audio_refuses_before_play() {
    let scratch = Scratch::new("window-a");
    let rig = Rig::new("window-a", &scratch, boost_plan());
    // Something else is already playing when the pre-roll starts.
    rig.flow.played.store(true, Ordering::SeqCst);
    let mut pass = rig.acknowledged();
    let error = pass.run().unwrap_err();
    assert_eq!(refusal_of(&error), D::SystemAudioNotQuiet);
}

/// Window B opens at the FIRST MARKER, not at `play`: the file starts with the
/// lead-in's silence, and a window opened at `play` and closed a second later
/// would close before any audio arrived and refuse a healthy run.
#[test]
fn the_tap_witness_window_opens_at_the_first_marker_not_at_play() {
    let scratch = Scratch::new("window-b");

    // Silent through the lead-in, awake at the first marker: PASSES.
    let rig = Rig::new("window-b", &scratch, boost_plan());
    let lead_in = rig.request.layout.lead_in_frames(RATE_F) as u64;
    rig.flow.wake_at.store(lead_in, Ordering::SeqCst);
    let mut pass = rig.acknowledged();
    pass.run()
        .expect("a tap that wakes at the first marker is a healthy tap");

    // Never wakes: REFUSES, and with the TCC code rather than a generic one.
    let rig = Rig::new("window-b", &scratch, boost_plan());
    rig.flow.wake_at.store(u64::MAX, Ordering::SeqCst);
    let mut pass = rig.acknowledged();
    let error = pass.run().unwrap_err();
    assert_eq!(refusal_of(&error), D::TapSilentDuringVerification);
}

/// "Cannot witness" is not "witnessed nothing". `tap_activity()` is `None`
/// before the first start and after a teardown — there is no realtime block to
/// read — and treating that as a pass would bless a run nobody observed.
#[test]
fn a_tap_activity_of_none_refuses_rather_than_assuming() {
    let scratch = Scratch::new("tap-none");
    let rig = Rig::new("tap-none", &scratch, boost_plan());
    lock(&rig.engine.facts).tap_readable = false;
    let mut pass = rig.acknowledged();
    let error = pass.run().unwrap_err();
    assert_eq!(refusal_of(&error), D::TapSilentDuringVerification);
}

// ── The capture: MS-21 ────────────────────────────────────────────────────

/// R23. The verification capture runs through the SHIPPED `record` with its
/// own `CaptureMeter`, and the meter's numbers ride out on the outcome so a
/// railed microphone is visible instead of silently producing a garbage
/// residual.
#[test]
fn the_verification_capture_is_metered_and_carries_its_stats() {
    let scratch = Scratch::new("metered");
    let rig = Rig::new("metered", &scratch, boost_plan());
    let mut pass = rig.acknowledged();
    let outcome = pass.run().expect("the pass runs");

    assert!(
        outcome.capture.peak_dbfs > CAPTURE_FLOOR_DBFS,
        "a real capture has a real peak, got {}",
        outcome.capture.peak_dbfs
    );
    assert!(
        outcome.capture.peak_dbfs.is_finite() && outcome.capture.rms_dbfs.is_finite(),
        "neither statistic may be -inf: a non-finite f64 does not round-trip"
    );
    assert!(
        outcome.capture.rms_dbfs > CAPTURE_FLOOR_DBFS,
        "and a real RMS, got {}",
        outcome.capture.rms_dbfs
    );
    // NOT `rms < peak`: `peak_dbfs` is a DECAYING meter read at the END of the
    // run, after the file's tail of silence, while the RMS is over the whole
    // captured buffer. The bundle field is documented as the decayed peak, and
    // asserting a buffer-max relationship would pin the wrong statistic.
    assert_eq!(
        outcome.capture.clipped_samples, 0,
        "a clean capture clips nothing"
    );
}

/// A digitally silent capture cannot put `-inf` on a wire. It is floored, and
/// then refused one step later for want of a credible marker train — which is
/// the honest failure.
#[test]
fn a_silent_capture_is_floored_and_then_refused() {
    let scratch = Scratch::new("silent");
    let rig = Rig::new("silent", &scratch, boost_plan());
    let silence = vec![0.0; rig.capture.recording.len()];
    let capture = MockCapture::new(silence, rig.flow.clone(), rig.journal.clone());
    let seam = VerifySeam {
        capture: Box::new(capture),
        control: Box::new(rig.control.clone()),
        engine: Box::new(rig.engine.clone()),
        helper: Box::new(rig.helper.clone()),
        tap: Box::new(rig.tap.clone()),
        volume: Box::new(rig.volume.clone()),
    };
    let mut pass = VerificationPass::arm(rig.request_clone(), seam).expect("arms");
    let projected = pass.projected_spl_db();
    pass.acknowledge("Amp", projected).expect("ack");
    let error = pass.run().unwrap_err();
    assert_eq!(refusal_of(&error), D::VerificationMarkersNotCredible);
}

/// REW's 30 %-of-a-block rule, reusing the SHIPPED `InputClipping = 12`. The
/// falsifier for blaming the correction for a railed mic: it must NOT come
/// back as a residual failure and must NOT come back as chain clipping.
#[test]
fn a_clipped_verification_capture_refuses_with_input_clipping() {
    let scratch = Scratch::new("clipped");
    let rig = Rig::new("clipped", &scratch, boost_plan());
    let railed = vec![1.0; rig.capture.recording.len()];
    let capture = MockCapture::new(railed, rig.flow.clone(), rig.journal.clone());
    let seam = VerifySeam {
        capture: Box::new(capture),
        control: Box::new(rig.control.clone()),
        engine: Box::new(rig.engine.clone()),
        helper: Box::new(rig.helper.clone()),
        tap: Box::new(rig.tap.clone()),
        volume: Box::new(rig.volume.clone()),
    };
    let mut pass = VerificationPass::arm(rig.request_clone(), seam).expect("arms");
    let projected = pass.projected_spl_db();
    pass.acknowledge("Amp", projected).expect("ack");
    let error = pass.run().unwrap_err();
    let refusal = refusal_of(&error);
    assert_eq!(refusal, D::InputClipping);
    assert_ne!(refusal.code(), 35, "not VerificationChainClipped");
}

/// Both directions, so a later reader cannot collapse two facts into one: a
/// nonzero ENGINE clip delta with a clean capture reports 35 only; a railed
/// capture with a clean engine reports 12 only.
#[test]
fn chain_clipping_and_capture_clipping_are_reported_separately() {
    let scratch = Scratch::new("two-clips");

    let rig = Rig::new("two-clips", &scratch, boost_plan());
    {
        let mut facts = lock(&rig.engine.facts);
        facts.clipped_base = 100;
        facts.clipped_delta_after_play = 7;
    }
    let mut pass = rig.acknowledged();
    let error = pass.run().unwrap_err();
    match refusal_of(&error) {
        D::VerificationChainClipped { count } => {
            assert_eq!(count, 7, "the DELTA across the window, not the total")
        }
        other => panic!("expected VerificationChainClipped, got {other:?}"),
    }

    // The converse is the test above; assert here only that the two codes are
    // different facts with different numbers and different advice.
    assert_ne!(
        D::VerificationChainClipped { count: 7 }.code(),
        D::InputClipping.code()
    );
    assert_ne!(
        D::VerificationChainClipped { count: 7 }.fix_easy(),
        D::InputClipping.fix_easy()
    );
}

// ── The WAV ───────────────────────────────────────────────────────────────

/// "Bit for bit" is a meaningful claim only because the f64 → f32 cast is the
/// ONLY transformation between the assembled buffer and the file, and both
/// sides apply it once. A quantizing format would move `L_verify`, which the
/// level book makes exact.
#[test]
fn the_wav_the_helper_plays_is_the_assembled_stimulus_bit_for_bit() {
    let scratch = Scratch::new("wav");
    let rig = Rig::new("wav", &scratch, boost_plan());
    // The helper copies the file at the instant it is handed it, which is the
    // only instant it provably exists: the teardown ladder deletes it.
    let copy = scratch.path.join("handed-over.wav");
    *lock(&rig.helper.copy_to) = Some(copy.clone());
    let mut pass = rig.acknowledged();
    pass.run().expect("runs");
    assert!(copy.exists(), "the child was handed a WAV");

    let mut reader = hound::WavReader::open(&copy).expect("the WAV opens");
    let spec = reader.spec();
    assert_eq!(spec.bits_per_sample, 32);
    assert_eq!(spec.channels, 1, "the stimulus is always mono");
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(spec.sample_rate, RATE);
    let written: Vec<f32> = reader
        .samples::<f32>()
        .collect::<Result<_, _>>()
        .expect("all samples read");

    // Rebuild what the pass assembled, from the same primitives.
    let preamp = recomputed_preamp_db(&rig.request.plan, RATE_F);
    let level = SweepLevel::new(l_measure().dbfs_rms() + preamp, TransducerClass::OverEar).unwrap();
    let shape = shape();
    let sweep = assemble_sweep(
        shape.duration_s,
        shape.sample_rate_hz,
        shape.f_start_hz,
        shape.f_end_hz,
        level,
    )
    .expect("a legal sweep");
    let (bracketed, _) =
        assemble_bracketed(sweep, &rig.request.layout, StimulusKind::VerificationSweep)
            .expect("brackets");
    let expected: Vec<f32> = bracketed.samples().iter().map(|&v| v as f32).collect();
    assert_eq!(written, expected, "the cast is the only transformation");
}

/// The per-run file is deleted by teardown, on every exit path.
#[test]
fn the_verification_wav_does_not_outlive_the_pass() {
    let scratch = Scratch::new("wav-cleanup");
    let rig = Rig::new("wav-cleanup", &scratch, boost_plan());
    let path = rig.request.wav_path.clone();
    {
        let mut pass = rig.acknowledged();
        pass.run().expect("runs");
    }
    assert!(!path.exists(), "teardown deletes the WAV it wrote");
}

// ── The teardown ladder ───────────────────────────────────────────────────

/// The single most important new safety test. The POLITE rung goes first —
/// the child runs the shared 5 ms raised cosine rather than stopping, because
/// a hard stop is itself a full-scale click — then SIGTERM, which is a SECOND
/// ramp request and not a stop, and only then a kill, which is reported.
#[test]
fn abort_writes_the_ramp_command_before_any_kill() {
    let scratch = Scratch::new("ladder");
    let rig = Rig::new("ladder", &scratch, boost_plan());
    rig.script.never_exits.store(true, Ordering::SeqCst);
    // Refuse mid-run with a live child: the tap never wakes, so Window B
    // refuses while the helper is still "playing".
    rig.flow.wake_at.store(u64::MAX, Ordering::SeqCst);

    let mut pass = rig.acknowledged();
    let error = pass.run().unwrap_err();
    assert_eq!(refusal_of(&error), D::TapSilentDuringVerification);
    // `run` already tore down on the refusal path, so the ladder has run and
    // the log is complete without dropping the pass.
    let log = pass.log().events().to_vec();

    let calls = rig.journal.calls();
    let abort = rig
        .journal
        .index_of("helper.request_abort")
        .expect("the ramp request is rung 1");
    let terminate = rig
        .journal
        .index_of("helper.request_terminate")
        .expect("SIGTERM is rung 1b");
    let kill = rig
        .journal
        .index_of("helper.kill")
        .expect("SIGKILL is last");
    let reap = rig
        .journal
        .index_of("helper.reap")
        .expect("and then a reap");
    assert!(
        abort < terminate && terminate < kill && kill < reap,
        "rungs out of order: {calls:?}"
    );

    // A kill is reported rather than being silent, because a kill IS the
    // full-scale click the ramp exists to prevent.
    assert!(
        log.iter().any(|e| matches!(
            e,
            SessionEvent::Warning {
                diagnostic: D::HelperKilledAfterRampDeadline
            }
        )),
        "the kill must be recorded: {log:?}"
    );
}

/// Every exit path — command, drop, panic — runs the full sequence, in order:
/// helper down and reaped, capture stopped, volume restored, trim restored,
/// and the lease released LAST, because releasing it re-arms the fail-open
/// watchdog and nothing that still needs the engine may run after that.
#[test]
fn every_exit_path_stops_the_helper_reaps_it_and_restores_volume_and_gain() {
    let scratch = Scratch::new("every-exit");

    // (a) command exit.
    let rig = Rig::new("every-exit", &scratch, boost_plan());
    rig.engine.set_gain(-4.0);
    {
        let mut pass = rig.acknowledged();
        pass.run().expect("runs");
    }
    assert_teardown_order(&rig.journal);
    assert_eq!(rig.engine.gain_db(), -4.0);
    assert!(rig.volume.sets().contains(&PRE_VOLUME));
    assert_eq!(
        rig.control.leases_live.load(Ordering::SeqCst),
        0,
        "the lease is released"
    );

    // (b) drop without running.
    let rig = Rig::new("every-exit", &scratch, boost_plan());
    drop(rig.acknowledged());
    assert_eq!(rig.control.leases_live.load(Ordering::SeqCst), 0);
    assert!(rig.volume.sets().contains(&PRE_VOLUME));

    // (c) panic injection.
    let rig = Rig::new("every-exit", &scratch, boost_plan());
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut pass = rig.acknowledged();
        let _ = pass.run();
        panic!("injected: the caller blew up holding a live pass");
    }));
    assert!(unwind.is_err());
    assert_eq!(rig.control.leases_live.load(Ordering::SeqCst), 0);
    assert!(rig.volume.sets().contains(&PRE_VOLUME));
}

fn assert_teardown_order(journal: &Journal) {
    let calls = journal.calls();
    let reap = journal
        .index_of("helper.reap")
        .expect("the child is reaped");
    let stop = journal
        .index_of("capture.stop")
        .expect("the capture is stopped");
    let volume = journal.index_of("volume.set").expect("volume is restored");
    let gain = journal
        .index_of("control.restore_gain")
        .expect("the trim is restored");
    let lease = journal
        .index_of("control.release_lease")
        .expect("the lease is released");
    assert!(
        reap < stop && stop < volume && volume < gain && gain < lease,
        "MS-14 order violated: {calls:?}"
    );
}

/// The falsifier for the `HelperProcess` contract itself. Every rung runs on
/// every exit path, so a rung that panicked on a dead child would abort the
/// ladder BEFORE the render device is destroyed — which is the exact failure
/// the ladder exists to prevent.
#[test]
fn every_helper_process_method_is_safe_to_call_after_the_child_has_exited() {
    let journal = Journal::default();
    let flow = Flow::default();
    let script = ChildScript::new(flow, journal);
    let mut child = MockChild {
        script,
        reaped: None,
    };
    // Drain to EOF, then reap: the child is gone.
    while child.read_line(Duration::from_millis(1)).unwrap().is_some() {}
    let first = child.reap().expect("the first reap");

    // Every method, twice, on a corpse.
    for _ in 0..2 {
        child.request_abort().expect("abort on a dead child");
        child
            .request_terminate()
            .expect("terminate on a dead child");
        child.kill().expect("kill on a dead child");
        child.send_play().expect("play on a dead child");
        assert_eq!(
            child.reap().expect("reap is idempotent"),
            first,
            "a second reap returns the cached status rather than blocking"
        );
        let _ = child.read_line(Duration::from_millis(1));
        let _ = child.pid();
    }
}

/// A child that had to be killed leaves its private render aggregate behind,
/// because SIGKILL bypasses `Drop`. Asserting only the reap is not enough: a
/// reaped child that leaked a device still satisfies "no zombie" while leaving
/// a private device wrapping the user's output.
#[test]
fn a_killed_child_reports_a_possibly_leaked_render_device() {
    let scratch = Scratch::new("leak");
    let rig = Rig::new("leak", &scratch, boost_plan());
    rig.script.never_exits.store(true, Ordering::SeqCst);
    rig.flow.wake_at.store(u64::MAX, Ordering::SeqCst);
    let mut pass = rig.acknowledged();
    let _ = pass.run();
    let log = pass.log().events().to_vec();

    let leaked = log.iter().any(|e| match e {
        SessionEvent::SinkFault { error } => error.contains("RenderDeviceLeaked"),
        SessionEvent::Terminated {
            diagnostic: Some(D::RenderDeviceLeaked { .. }),
        } => true,
        _ => false,
    });
    assert!(leaked, "a killed child's device must be reported: {log:?}");
    assert_eq!(
        expected_render_device_uid(4242),
        "com.paraeq.render.4242",
        "the UID rule lives in one place"
    );
}

/// A helper that cannot be spawned at all is a refusal, not a panic, and
/// nothing has played.
#[test]
fn a_helper_that_cannot_be_spawned_refuses() {
    let scratch = Scratch::new("no-helper");
    let rig = Rig::new("no-helper", &scratch, boost_plan());
    rig.helper.fail_spawn.store(true, Ordering::SeqCst);
    let mut pass = rig.acknowledged();
    let error = pass.run().unwrap_err();
    assert_eq!(refusal_of(&error), D::HelperUnavailable);
    assert_eq!(rig.journal.index_of("helper.send_play"), None);
}

// ── The exit-code table ───────────────────────────────────────────────────

/// The child's exit codes are a closed table on its side, and this is the
/// parent's half of the contract: every one maps to a decision, and a number
/// that quietly changed meaning would move a refusal onto the wrong cause.
#[test]
fn exit_code_mapping_is_exhaustive() {
    let all = [
        HelperExitKind::Ok,
        HelperExitKind::BadArgs,
        HelperExitKind::Wav,
        HelperExitKind::DeviceNotFound,
        HelperExitKind::Stalled,
        HelperExitKind::BackstopRefused,
        HelperExitKind::NotDefaultOutput,
        HelperExitKind::Signalled,
        HelperExitKind::Unknown(101),
    ];
    for kind in all {
        // Every kind has a decision; only the two clean ones have none.
        let diagnostic = exit_diagnostic(kind);
        match kind {
            HelperExitKind::Ok | HelperExitKind::Signalled => assert!(
                diagnostic.is_none(),
                "{kind:?} is not itself a failure: the ladder reports a kill"
            ),
            _ => {
                let d = diagnostic.unwrap_or_else(|| panic!("{kind:?} has no decision"));
                assert!(d.is_blocking(), "{kind:?} maps to a blocking diagnostic");
            }
        }
    }

    // And the numbers themselves, which are the contract.
    for (code, expected) in [
        (0, HelperExitKind::Ok),
        (2, HelperExitKind::BadArgs),
        (3, HelperExitKind::Wav),
        (4, HelperExitKind::DeviceNotFound),
        (5, HelperExitKind::Stalled),
        (6, HelperExitKind::BackstopRefused),
        (7, HelperExitKind::NotDefaultOutput),
        (99, HelperExitKind::Unknown(99)),
    ] {
        assert_eq!(
            classify_exit(HelperExit {
                code: Some(code),
                signalled: false
            }),
            expected,
            "exit {code}"
        );
    }
    // `signalled` wins over any code: a killed child's code says nothing about
    // what it managed to clean up.
    assert_eq!(
        classify_exit(HelperExit {
            code: Some(0),
            signalled: true
        }),
        HelperExitKind::Signalled
    );
    assert_eq!(
        classify_exit(HelperExit {
            code: None,
            signalled: false
        }),
        HelperExitKind::Signalled
    );
}

/// A child that exits with a failure code refuses, carrying that code.
#[test]
fn a_helper_exit_code_becomes_a_refusal_that_names_it() {
    let scratch = Scratch::new("exit-code");
    let rig = Rig::new("exit-code", &scratch, boost_plan());
    *lock(&rig.script.exit) = HelperExit {
        code: Some(6),
        signalled: false,
    };
    let mut pass = rig.acknowledged();
    let error = pass.run().unwrap_err();
    assert_eq!(refusal_of(&error), D::HelperFailed { exit_code: 6 });
}

// ── MS-23 ─────────────────────────────────────────────────────────────────

/// The verification pass writes into the SAME `SessionLog` type, not a second
/// one — which is what `SessionLog::push` became `pub(crate)` for.
#[test]
fn the_verification_pass_writes_into_the_same_session_log_type() {
    let scratch = Scratch::new("ms23");
    let rig = Rig::new("ms23", &scratch, boost_plan());
    let mut pass = rig.acknowledged();
    pass.run().expect("runs");
    let log: paraeq_measure::SessionLog = pass.finish();
    let events = log.events();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::CalLoaded { .. })),
        "MS-23's cal columns"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::Acknowledged { .. })),
        "MS-18's acknowledgement"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::VerifyLevelled { .. })),
        "MS-23's final level, cap and class"
    );
    assert!(
        matches!(events.last(), Some(SessionEvent::Terminated { .. })),
        "the terminating event is always last: {events:?}"
    );
    // A pass runs no rungs and no solve, which is why `VerifyLevelled` exists
    // instead of `SolveInstalled`.
    assert!(!events
        .iter()
        .any(|e| matches!(e, SessionEvent::RungMeasured { .. })));
    assert!(!events
        .iter()
        .any(|e| matches!(e, SessionEvent::SolveInstalled { .. })));
}

/// R22 (a). MS-23 requires "cal file identity + gain read-back", and
/// `CalLoaded` is the ONLY variant that carries either — `VerifyLevelled`
/// carries neither. The derivation is the same `cal.pin_gain(read_back)` call
/// `MeasurementSession::begin` makes: one cal identity, no second source of
/// truth.
#[test]
fn the_verification_log_records_cal_identity_and_the_gain_read_back() {
    let scratch = Scratch::new("cal-log");
    let mut rig = Rig::new("cal-log", &scratch, boost_plan());
    rig.request.input_gain_read_back = 0.75;
    let pass = rig.arm().expect("arms");
    let events = pass.log().events().to_vec();
    let cal = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::CalLoaded {
                file_identity,
                gain_matches,
                input_gain_read_back,
                reference_input_gain,
                ..
            } => Some((
                file_identity.clone(),
                *gain_matches,
                *input_gain_read_back,
                *reference_input_gain,
            )),
            _ => None,
        })
        .expect("CalLoaded is in the log");
    assert_eq!(cal.0, "UMIK-1 #7001234 (umik-7001234.txt)");
    assert_eq!(cal.2, 0.75, "the read-back, verbatim");
    assert_eq!(cal.3, 1.0, "the cal's reference gain");
    assert!(
        !cal.1,
        "0.75 against 1.0 is a mismatch, so the margin is live"
    );
}

/// `VerifyLevelled` carries MS-23's "final level, cap, class" for a pass that
/// runs no rungs, and its level is the SWEEP-SPAN RMS convention
/// `SweepStarted` already uses.
#[test]
fn verify_levelled_carries_the_final_level_cap_and_class() {
    let scratch = Scratch::new("levelled");
    let rig = Rig::new("levelled", &scratch, boost_plan());
    let pass = rig.arm().expect("arms");
    let levelled = pass
        .log()
        .events()
        .iter()
        .find_map(|e| match e {
            SessionEvent::VerifyLevelled {
                class,
                gain_db_pinned,
                l_measure_dbfs_rms,
                l_verify_dbfs_rms,
                peak_correction_gain_db,
                spl_cap_db,
            } => Some((
                *class,
                *gain_db_pinned,
                *l_measure_dbfs_rms,
                *l_verify_dbfs_rms,
                *peak_correction_gain_db,
                *spl_cap_db,
            )),
            _ => None,
        })
        .expect("VerifyLevelled is in the log");
    assert_eq!(levelled.0, TransducerClass::OverEar);
    assert_eq!(levelled.1, 0.0, "the trim is pinned to zero for the pass");
    assert_eq!(levelled.2, l_measure().dbfs_rms());
    assert_eq!(
        levelled.3,
        pass.level().expect("a level").dbfs_rms(),
        "the logged level is the level"
    );
    assert!(
        (levelled.4 - 6.0).abs() < 0.05,
        "peak_correction_gain_db EXCLUDES the preamp, got {}",
        levelled.4
    );
    assert_eq!(levelled.5, 100.0, "the OverEar hard cap");
}

// ── The abort budget ──────────────────────────────────────────────────────

/// E8's number. Nobody can make the last term zero across a process boundary
/// plus the engine round trip, so the budget is COMPUTED and REPORTED per run
/// rather than asserted — and the mitigating fact is that `L_verify` is always
/// at or below `L_measure`, so exposure during those milliseconds is bounded
/// below the Direct path's.
#[test]
fn abort_reaches_acoustic_silence_within_the_stated_budget() {
    let scratch = Scratch::new("budget");
    let rig = Rig::new("budget", &scratch, boost_plan());
    lock(&rig.engine.facts).latency_ms = Some(42.0);
    let mut pass = rig.acknowledged();
    let outcome = pass.run().expect("runs");

    // one 512-frame block at 48 kHz + 5 ms ramp + 42 ms latency + 1 ms slack
    let block_ms = 512.0 / RATE_F * 1000.0;
    let expected = block_ms + 5.0 + 42.0 + 1.0;
    assert!(
        (outcome.abort_acoustic_budget_ms - expected).abs() < 1e-9,
        "got {} want {expected}",
        outcome.abort_acoustic_budget_ms
    );
    assert!(
        outcome.level_dbfs <= l_measure().dbfs_rms(),
        "the mitigating fact: L_verify is never above L_measure"
    );
}

// ── The outcome shape ─────────────────────────────────────────────────────

/// The outcome maps one to one onto `paraeq_decide::bundle::Verification`, and
/// this is the measure-side half of that documented twin: the field names and
/// the meanings must match, because `paraeq-measure` may not depend on
/// `paraeq-decide` and the compiler cannot check it.
#[test]
fn the_outcome_carries_everything_a_verification_bundle_needs() {
    let scratch = Scratch::new("outcome");
    let rig = Rig::new("outcome", &scratch, boost_plan());
    let mut pass = rig.acknowledged();
    let outcome = pass.run().expect("runs");

    assert_eq!(
        outcome.gain_db, 0.0,
        "pinned, and carried so decide() can refuse rather than trust"
    );
    assert!(
        (outcome.installed_preamp_db - -6.0).abs() < 0.05,
        "the ENGINE's armed preamp, not the plan's: {}",
        outcome.installed_preamp_db
    );
    assert_eq!(outcome.running_rate_hz, RATE_F, "the LIVE rate");
    assert_eq!(outcome.position_index, 0);
    assert_eq!(outcome.routing, HelperRouting::Only(0));
    assert!(
        (outcome.level_dbfs - (l_measure().dbfs_rms() - 6.0)).abs() < 0.05,
        "L_verify"
    );
    let fit = outcome.two_clock.expect("a fit formed");
    assert!(fit.skew_ppm.abs() < 5.0, "an unskewed mock reads ~0 ppm");
    assert!(fit.residual_peak_samples.is_finite());
    assert!(outcome.ir.samples.iter().all(|v| v.is_finite()));

    // The twin is documented on both sides, and the doc is the only thing
    // keeping them in step.
    let source = include_str!("../src/verify.rs");
    assert!(
        source.contains("paraeq_decide::bundle::Verification"),
        "the outcome must name the shape it maps onto"
    );
    assert!(
        source.contains("paraeq_decide::bundle::CaptureStats"),
        "and so must the capture-stats twin"
    );
}

/// A phase machine that runs one way only, `require()` on every transition.
#[test]
fn the_phase_machine_runs_one_way_only() {
    let scratch = Scratch::new("phases");
    let rig = Rig::new("phases", &scratch, boost_plan());
    let mut pass = rig.arm().expect("arms");
    assert_eq!(pass.phase(), VerifyPhase::Armed);
    let projected = pass.projected_spl_db();
    pass.acknowledge("Amp", projected).expect("ack");
    assert_eq!(pass.phase(), VerifyPhase::Acknowledged);
    // A second acknowledgement is out of order.
    assert!(matches!(
        pass.acknowledge("Amp", projected),
        Err(VerifyError::WrongPhase { .. })
    ));
    pass.run().expect("runs");
    assert_eq!(pass.phase(), VerifyPhase::Terminated);
    // And a second run is out of order too.
    assert!(matches!(pass.run(), Err(VerifyError::WrongPhase { .. })));
}

// ── The child's protocol ──────────────────────────────────────────────────

/// A flat-JSON scanner, not a JSON library: `paraeq-measure` depends on
/// `paraeq-dsp`, `hound` and `thiserror` and nothing else. What matters is
/// that it REFUSES what it cannot represent rather than skipping it — a parser
/// that ignored the part it did not understand would hand a gate a field from
/// the wrong nesting level.
#[test]
fn the_status_line_parser_refuses_what_it_cannot_represent() {
    let ready = ready_line("0", 48_000.0, 44_100.0);
    let fields = paraeq_measure::verify::parse_line(&ready).expect("a ready line parses");
    assert_eq!(
        paraeq_measure::verify::str_field(&fields, "event"),
        Some("ready")
    );
    assert_eq!(
        paraeq_measure::verify::num_field(&fields, "sample_rate_hz"),
        Some(48_000.0)
    );
    assert_eq!(
        paraeq_measure::verify::num_field(&fields, "sub_device_sample_rate_hz"),
        Some(44_100.0)
    );
    // A string field is not a number and a number is not a string: asking for
    // the wrong one answers `None` rather than coercing.
    assert_eq!(
        paraeq_measure::verify::num_field(&fields, "device_uid"),
        None
    );
    assert_eq!(paraeq_measure::verify::str_field(&fields, "channels"), None);

    for bad in [
        r#"{"event":"ready","nested":{"a":1}}"#,
        r#"{"event":"ready","list":[1,2]}"#,
        r#"{"event":"ready","missing":null}"#,
        r#"{"event":"ready""#,
        r#"not json at all"#,
        r#"{"event":"ready"} trailing"#,
    ] {
        assert!(
            paraeq_measure::verify::parse_line(bad).is_none(),
            "{bad} must be refused, not partially parsed"
        );
    }

    // Escapes a device UID can legitimately carry.
    let line = r#"{"event":"ready","device_uid":"AppleHDA:\"Built-in\"\\1"}"#;
    let fields = paraeq_measure::verify::parse_line(line).expect("escapes parse");
    assert_eq!(
        paraeq_measure::verify::str_field(&fields, "device_uid"),
        Some(r#"AppleHDA:"Built-in"\1"#)
    );
}

/// The child's MS-4 guard counts are the parent's warnings. A verified
/// stimulus produces zeros, so a nonzero count is an upstream defect the child
/// contained rather than one it caused.
#[test]
fn the_childs_guard_counts_become_the_shipped_emit_warnings() {
    let scratch = Scratch::new("guard-counts");
    let rig = Rig::new("guard-counts", &scratch, boost_plan());
    lock(&rig.script.lines).clear();
    rig.script.push(ready_line("0", RATE_F, RATE_F));
    rig.script.push(
        r#"{"event":"done","clamped":3,"frames_emitted":123,"sanitized":2,"underrun_frames":0}"#
            .to_owned(),
    );
    let mut pass = rig.acknowledged();
    let outcome = pass.run().expect("runs");
    assert!(outcome.warnings.contains(&D::EmitClamped { count: 3 }));
    assert!(outcome
        .warnings
        .contains(&D::EmitNonFiniteSanitized { count: 2 }));
    for w in &outcome.warnings {
        assert!(!w.is_blocking(), "{w:?} must not block a usable capture");
    }
}
