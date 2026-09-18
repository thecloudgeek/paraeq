//! `hal_render_probe` — the Stage-6 **B0 hardware spike**: three questions, one
//! sitting, each answered by one line the owner can read.
//!
//! Every spike question below is one the repo cannot answer headlessly and one
//! that changes a module's SHAPE rather than a constant, which is why this is a
//! standalone binary run by hand rather than an `#[ignore]` test: it has to run
//! as a foreign process (a test harness process is the same process that owns
//! the tap, and the tap excludes its own pid by design — see `tap.rs`), and its
//! primary readout for question 1 is a human watching for a macOS permission
//! dialog.
//!
//! ```sh
//! # build the probe once, then run the three modes in this order
//! cargo build --release -p paraeq-coreaudio --examples
//! cargo run --release -p paraeq-coreaudio --example hal_render_probe -- --bare
//! cargo run --release -p paraeq-coreaudio --example hal_render_probe -- --wrapped
//! cargo run --release -p paraeq-coreaudio --example hal_render_probe -- --coexist
//! ```
//!
//! # The three questions
//!
//! **Q1 (spike S2) — does a bare output IOProc raise a microphone permission
//! prompt on a mic-capable default output, and is the child's render tapped at
//! all?** `--bare` registers an IOProc straight on the physical default output;
//! `--wrapped` registers it on the private single-sub-device aggregate
//! (`kAudioSubDeviceInputChannelsKey: 0`) that `render.rs` ships. Run BOTH, on
//! AirPods or a USB headset, with a fresh TCC state (`tccutil reset` first).
//! This is **owner question E9**, and the prompt half is an observation only a
//! human can make. `--wrapped` also prints whether the wrapper's nominal rate
//! equals its sole sub-device's — spike S2(d), which decides whether the
//! helper's two-rate `ready` line is mandatory or merely defensive.
//!
//! **Q2 — does a GLOBAL process tap capture a child rendering to the default
//! output?** Both `--bare` and `--wrapped` answer it, because both spawn this
//! same binary again as a child (a different pid, so the tap does not exclude
//! it) and count nonzero input blocks on the tap aggregate while it renders.
//! Every tap-inclusion claim in every spec rests on `afplay`, which is an
//! AudioQueue client rather than a raw `AudioDeviceIOProc` client, and nothing
//! in the repo tests the raw case. **This is the premise of the whole
//! verification feature.**
//!
//! **Q3 (spike S4) — can the tap aggregate and the MS-22 measurement aggregate
//! coexist on one default output device?** `--coexist` brings up the tap
//! aggregate, then creates the measurement aggregate on the same device, drives
//! audio through both, and tears each down in turn to show the survivor still
//! works. This is **owner questions E6 and E12** at the HAL layer;
//! `tests/test_measure_hardware.rs::tap_and_measurement_aggregates_coexist_on_one_output_device`
//! is the same question through the live engine and its measurement lease.
//!
//! # Reading the output
//!
//! Every answer is one line beginning `PROBE <question>: ` and continuing with
//! `PASS`, `FAIL` or `OBSERVE`. `OBSERVE` appears exactly once — the mic-prompt
//! line — because no API reports whether macOS showed a dialog; that line spells
//! out which observation is the pass and which is the fail. Everything else is
//! machine-decided. Numbers that are data rather than verdicts (rates, counter
//! deltas) are printed beside the verdict, never asserted on: the harness's
//! print-don't-assert discipline.
//!
//! # The fallbacks, recorded here so a FAIL has a next step
//!
//! | line | if it FAILs | fallback |
//! |---|---|---|
//! | `tap-capture[bare]` and `tap-capture[wrapped]` both FAIL | a raw IOProc child is not tapped at all | an AudioQueue client (`queue_render.rs`, ~6 FFI calls, buffers ≤ 5 ms so the abort ramp still lands at 5 ms granularity) replaces `DeviceRenderer`'s IOProc |
//! | `tap-capture[wrapped]` FAILs but `tap-capture[bare]` PASSes | the private wrapper is not tapped | the choice is between a spurious mic prompt and no feature — **escalate as E9**, do not ship past it |
//! | `mic-prompt[bare]` observed as a prompt | a bare IOProc counts as microphone access | the wrapper is MANDATORY, which is what `render.rs` already ships; nothing changes |
//! | `mic-prompt[wrapped]` observed as a prompt | the wrapper does not close the TCC surface | the hazard is live and unmitigated — **escalate as E9** |
//! | `wrapper-rate` FAIL | a private aggregate's nominal rate ≠ its sub-device's | the helper's two-rate `ready` line is mandatory rather than defensive, and the parent's rate fence must name WHICH rate disagreed |
//! | `coexist-create` FAIL | the two aggregates cannot both be up | capture-side ladder A → B2 → B1: B2 is a mic-only aggregate + software resample + `Warn(TwoClock)`; B1 puts the mic in the TAP aggregate and is better physics but rebuilds a live tap mid-session — **owner question E12** |
//! | `coexist-teardown-*` FAIL | tearing one down breaks the other | same ladder, and the verification teardown order becomes load-bearing rather than tidy |
//!
//! # What this probe never does
//!
//! It plays a quiet tone (see [`PROBE_AMPLITUDE`]) and it never touches the
//! system volume. It never leaves a tap, an aggregate or a child process behind:
//! every HAL object is an RAII local and the child is wrapped in a guard whose
//! `Drop` kills and reaps it, so a panic unwinds through the same teardown a
//! clean exit runs. Ctrl-C sets a stop flag that every loop polls, for the same
//! reason.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use paraeq_coreaudio::ioproc::{IoCallback, IoProcHandle};
use paraeq_coreaudio::measure_aggregate::{MeasureAggregateConfig, MicCapture, MicSelector};
use paraeq_coreaudio::properties;
use paraeq_coreaudio::render::RenderAggregate;
use paraeq_coreaudio::tap::TapSystem;
use paraeq_measure::CaptureSource;

/// Peak amplitude of the probe tone, linear (≈ −30 dBFS).
///
/// Quiet on purpose. The questions here are "is it captured" and "does it
/// prompt", neither of which is a level question, and a probe that is louder
/// than it needs to be is a probe someone runs on headphones once.
const PROBE_AMPLITUDE: f32 = 0.03;

/// Probe tone frequency, Hz. Mid-band, so it survives any transducer the rig
/// has and is unmistakable to the ear.
const PROBE_TONE_HZ: f64 = 1_000.0;

/// How long the child renders, seconds. Long enough for the tap aggregate's
/// IOProc to engage (the measured worst case is ~4–5 s) plus a witness window.
const RENDER_SECONDS: u64 = 8;

/// How long to wait for the child's `CHILD-READY` line before giving up on it.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// The witness window: how long we watch the tap's nonzero-block counter while
/// the child is definitely rendering.
const WITNESS_WINDOW: Duration = Duration::from_secs(3);

/// A sample counts as nonzero above this. The same idea as the engine's
/// realtime `nonzero_blocks` witness: "audio is flowing", not "audio is loud".
const NONZERO_EPSILON: f32 = 1e-6;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .expect("install Ctrl-C handler");
    }

    match parse(&args) {
        Some(Mode::Bare) => probe_render(false, &stop),
        Some(Mode::Wrapped) => probe_render(true, &stop),
        Some(Mode::Coexist) => probe_coexistence(&stop),
        Some(Mode::RenderChild { uid, wrapped }) => render_child(&uid, wrapped, &stop),
        None => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

const USAGE: &str = "\
hal_render_probe — the B0 hardware spike (three questions, one sitting).

USAGE:
    hal_render_probe --bare       Q1/Q2: a BARE IOProc on the physical output
    hal_render_probe --wrapped    Q1/Q2: the private zero-input-channel wrapper
    hal_render_probe --coexist    Q3:    tap aggregate + measurement aggregate

    --render-child --uid <uid> [--wrapped]
                                  internal: re-spawned by --bare/--wrapped so
                                  the renderer is a DIFFERENT pid and is
                                  therefore not excluded from the tap.

Run --bare and --wrapped with AirPods or a USB headset as the default output,
after `tccutil reset` — the microphone-prompt half of Q1 is an observation, not
a return value. --coexist needs the microphone TCC grant.
";

enum Mode {
    Bare,
    Coexist,
    RenderChild { uid: String, wrapped: bool },
    Wrapped,
}

fn parse(args: &[String]) -> Option<Mode> {
    let mut child = false;
    let mut coexist = false;
    let mut uid: Option<String> = None;
    let mut wrapped = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bare" => {}
            "--coexist" => coexist = true,
            "--render-child" => child = true,
            "--uid" => {
                i += 1;
                uid = Some(args.get(i)?.clone());
            }
            "--wrapped" => wrapped = true,
            _ => return None,
        }
        i += 1;
    }
    if child {
        return Some(Mode::RenderChild { uid: uid?, wrapped });
    }
    if coexist {
        return Some(Mode::Coexist);
    }
    if wrapped {
        return Some(Mode::Wrapped);
    }
    if args.iter().any(|a| a == "--bare") {
        return Some(Mode::Bare);
    }
    None
}

// ───────────────────────────── Q1 + Q2: the render probe ─────────────────────

/// Bring the tap up, spawn this binary again as a renderer, and report whether
/// the tap saw it — plus the two lines only the owner can settle.
fn probe_render(wrapped: bool, stop: &AtomicBool) {
    let label = if wrapped { "wrapped" } else { "bare" };
    let device = properties::default_output_device().expect("a default output device");
    let uid = properties::device_uid(device).expect("its UID");
    let rate = properties::nominal_sample_rate(device).expect("its nominal rate");
    let inputs = properties::input_stream_channel_count(device).unwrap_or(0);

    println!("--- hal_render_probe --{label} ---");
    println!("default output: uid='{uid}' rate={rate} Hz own_input_channels={inputs}");
    if inputs == 0 {
        println!(
            "NOTE: this output exposes NO input streams, so the microphone-prompt question \
             cannot be answered on it. Select AirPods or a USB headset as the default output \
             and run again — on a mic-less output both modes trivially do not prompt."
        );
    }
    println!(
        ">>> WATCH THE SCREEN for the next {RENDER_SECONDS}s. A microphone permission dialog \
         appearing is the FAIL for the mic-prompt line below."
    );

    // The tap, and an IOProc that counts what reaches it. RAII locals: any
    // panic below unwinds through `io` (stop + destroy IOProc) then `tap`
    // (destroy aggregate, destroy tap), which is the invariant teardown order.
    let mut tap = TapSystem::create().expect("TapSystem::create (needs the System Audio grant)");
    println!(
        "tap up: aggregate={} self_excluded={} (the tap excludes THIS pid, which is why the \
         renderer is a child process)",
        tap.aggregate, tap.self_excluded
    );
    let counts = TapCounts::default();
    let mut io = IoProcHandle::register(tap.aggregate, counts.callback())
        .expect("IoProcHandle::register on the tap aggregate");
    io.start().expect("IoProcHandle::start");

    // The renderer, as a foreign process.
    let mut child = spawn_render_child(&uid, wrapped);
    let ready = child.read_ready(READY_TIMEOUT);

    let before = counts.snapshot();
    wait_for(WITNESS_WINDOW, stop);
    let after = counts.snapshot();

    let exited = child.wait_for_exit(Duration::from_secs(RENDER_SECONDS + 5));
    drop(child);
    io.stop();
    let errors = tap.teardown();

    // ── the verdicts ────────────────────────────────────────────────────────
    println!(
        "tap counters over a {:.1}s window: callbacks {} -> {}, nonzero blocks {} -> {}",
        WITNESS_WINDOW.as_secs_f64(),
        before.callbacks,
        after.callbacks,
        before.nonzero,
        after.nonzero,
    );
    match &ready {
        Some(line) => println!("child said: {line}"),
        None => println!("child never said CHILD-READY within {READY_TIMEOUT:?}"),
    }
    println!("child exit status: {exited:?}");
    if !errors.is_empty() {
        println!("tap teardown errors: {errors:?}");
    }

    let nonzero_advanced = after.nonzero > before.nonzero;
    if ready.is_none() {
        println!(
            "PROBE tap-capture[{label}]: FAIL — the renderer never opened its device, so nothing \
             was rendered and Q2 is UNANSWERED. Read the child's stderr above; a device-open \
             failure is a harness problem, not the spike's answer."
        );
    } else if nonzero_advanced {
        println!(
            "PROBE tap-capture[{label}]: PASS — a global process tap DOES capture a foreign \
             raw-IOProc renderer on the default output ({} nonzero blocks in {:.1}s). This is \
             the premise of the verification feature.",
            after.nonzero - before.nonzero,
            WITNESS_WINDOW.as_secs_f64(),
        );
    } else {
        println!(
            "PROBE tap-capture[{label}]: FAIL — the tap saw NO nonzero input while a foreign \
             raw-IOProc renderer played. Fallback: an AudioQueue client (queue_render.rs) in \
             place of DeviceRenderer's IOProc. If --bare PASSes and --wrapped does not, that is \
             owner question E9 and must not be shipped past."
        );
    }

    println!(
        "PROBE mic-prompt[{label}]: OBSERVE — PASS if NO microphone permission dialog appeared \
         during this run; FAIL if one did. (Owner question E9. Meaningful only on a mic-capable \
         default output with a fresh TCC state; this output reports {inputs} input channels.)"
    );

    if wrapped {
        report_wrapper_rate(&uid, rate);
    }
}

/// Spike S2(d): does the private wrapper's nominal rate equal its sole
/// sub-device's? Nothing in the HAL documentation establishes that it must, and
/// the answer decides whether the helper's two-rate `ready` line is mandatory.
///
/// Creates and destroys its own wrapper rather than asking the child, so the
/// number is read in this process where a failure is visible.
fn report_wrapper_rate(out_uid: &str, sub_device_rate: f64) {
    let mut aggregate = match RenderAggregate::create(out_uid) {
        Ok(aggregate) => aggregate,
        Err(e) => {
            println!("PROBE wrapper-rate: FAIL — the render aggregate would not create: {e}");
            return;
        }
    };
    let wrapper_rate = properties::nominal_sample_rate(aggregate.id());
    let errors = aggregate.teardown();
    if !errors.is_empty() {
        println!("render aggregate teardown errors: {errors:?}");
    }
    match wrapper_rate {
        Ok(rate) if (rate - sub_device_rate).abs() <= 0.5 => println!(
            "PROBE wrapper-rate: PASS — wrapper {rate} Hz == sub-device {sub_device_rate} Hz; \
             the helper's two-rate `ready` line stays defensive."
        ),
        Ok(rate) => println!(
            "PROBE wrapper-rate: FAIL — wrapper {rate} Hz != sub-device {sub_device_rate} Hz. \
             The two-rate `ready` line is MANDATORY and the parent's rate fence must name which \
             rate disagreed."
        ),
        Err(e) => println!("PROBE wrapper-rate: FAIL — could not read the wrapper's rate: {e}"),
    }
}

/// The child half of `--bare` / `--wrapped`: open a device, render a tone, tear
/// down. A different pid from the tap's owner, which is the whole point.
fn render_child(uid: &str, wrapped: bool, stop: &AtomicBool) {
    // `aggregate` is declared before `io` so that `io` (stop + destroy IOProc)
    // drops FIRST and the aggregate second — the invariant teardown order.
    let mut aggregate = None;
    let device = if wrapped {
        let created = RenderAggregate::create(uid).expect("RenderAggregate::create");
        let id = created.id();
        aggregate = Some(created);
        id
    } else {
        let id = properties::translate_uid_to_device(uid).expect("translate_uid_to_device");
        assert_ne!(id, 0, "no device has UID '{uid}'");
        id
    };

    let render_rate = properties::nominal_sample_rate(device).expect("the device's nominal rate");
    let sub_rate = properties::translate_uid_to_device(uid)
        .ok()
        .filter(|id| *id != 0)
        .and_then(|id| properties::nominal_sample_rate(id).ok())
        .unwrap_or(f64::NAN);

    let mut phase = 0.0f64;
    let step = std::f64::consts::TAU * PROBE_TONE_HZ / render_rate;
    let tone: IoCallback = Box::new(move |mut block| {
        // Realtime lane: no allocation, no logging, no locks. `sin` is the one
        // computation, which is what a tone probe is.
        for (samples, channels) in block.output.buffers_mut() {
            let channels = channels.max(1);
            for frame in samples.chunks_mut(channels) {
                let v = (phase.sin() as f32) * PROBE_AMPLITUDE;
                phase += step;
                if phase > std::f64::consts::TAU {
                    phase -= std::f64::consts::TAU;
                }
                for sample in frame.iter_mut() {
                    *sample = v;
                }
            }
        }
    });

    let mut io = IoProcHandle::register(device, tone).expect("IoProcHandle::register");
    io.start().expect("IoProcHandle::start");

    // The parent waits on this line before it opens its witness window, so it
    // must be flushed before any sleeping happens.
    println!("CHILD-READY device={device} render_rate={render_rate} sub_device_rate={sub_rate}");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    wait_for(Duration::from_secs(RENDER_SECONDS), stop);

    io.stop();
    if let Some(mut aggregate) = aggregate {
        let errors = aggregate.teardown();
        if !errors.is_empty() {
            eprintln!("child: render aggregate teardown errors: {errors:?}");
        }
    }
}

// ─────────────────────────── Q3: the coexistence probe ───────────────────────

/// Bring the tap aggregate up, then the MS-22 measurement aggregate on the SAME
/// physical output, and tear each down in turn while the other is watched.
fn probe_coexistence(stop: &AtomicBool) {
    let device = properties::default_output_device().expect("a default output device");
    let uid = properties::device_uid(device).expect("its UID");
    println!("--- hal_render_probe --coexist ---");
    println!("default output: uid='{uid}'");
    println!(
        "NOTE: this needs the MICROPHONE grant as well as System Audio Recording. \
         Set PARAEQ_MIC_UID to name a specific mic; unset, the default input is used."
    );

    let mut tap = TapSystem::create().expect("TapSystem::create");
    let counts = TapCounts::default();
    let mut io = IoProcHandle::register(tap.aggregate, counts.callback())
        .expect("IoProcHandle::register on the tap aggregate");
    io.start().expect("IoProcHandle::start");
    println!("tap aggregate up: {}", tap.aggregate);

    // The tap's IOProc only cycles while the system renders audio (0 cb/s
    // idle), so a coexistence probe with nothing playing would read as a
    // failure of the tap rather than a fact about the pair.
    let playback = Playback::start();

    // ── the question itself ─────────────────────────────────────────────────
    let first = MicCapture::create(measure_config());
    let mut mic = match first {
        Ok(mic) => {
            println!(
                "PROBE coexist-create: PASS — the MS-22 measurement aggregate created while the \
                 tap aggregate is live on the same output device (mic '{}', {} Hz).",
                mic.mic_uid(),
                mic.sample_rate_hz(),
            );
            mic
        }
        Err(e) => {
            println!(
                "PROBE coexist-create: FAIL — MicCapture::create refused with the tap live: \
                 {e:?}. Rerun with no tap to confirm the aggregate itself is healthy. If it is, \
                 this is the capture-side fallback ladder's trigger: A -> B2 (mic-only aggregate \
                 + software resample + Warn(TwoClock)) -> B1 (mic inside the TAP aggregate, \
                 better physics but it rebuilds a live tap mid-session — owner question E12)."
            );
            drop(playback);
            io.stop();
            let _ = tap.teardown();
            return;
        }
    };

    // Both must actually deliver callbacks, not merely exist.
    let before = counts.snapshot();
    let mic_before = mic.counters().callbacks;
    drain(&mut mic);
    wait_for(WITNESS_WINDOW, stop);
    let after = counts.snapshot();
    let mic_after = mic.counters().callbacks;
    println!(
        "over {:.1}s: tap callbacks {} -> {} (nonzero {} -> {}), mic callbacks {} -> {}",
        WITNESS_WINDOW.as_secs_f64(),
        before.callbacks,
        after.callbacks,
        before.nonzero,
        after.nonzero,
        mic_before,
        mic_after,
    );
    if after.callbacks > before.callbacks && mic_after > mic_before {
        println!("PROBE coexist-callbacks: PASS — both IOProcs cycled with both aggregates live.");
    } else {
        println!(
            "PROBE coexist-callbacks: FAIL — one of the two stopped cycling while the other was \
             up (tap +{}, mic +{}). Same fallback ladder as coexist-create.",
            after.callbacks - before.callbacks,
            mic_after - mic_before,
        );
    }

    // ── tearing down the measurement aggregate must not break the tap ───────
    drop(mic);
    let before = counts.snapshot();
    wait_for(WITNESS_WINDOW, stop);
    let after = counts.snapshot();
    if after.callbacks > before.callbacks {
        println!(
            "PROBE coexist-teardown-measure: PASS — the tap kept cycling after the measurement \
             aggregate was destroyed (+{} callbacks).",
            after.callbacks - before.callbacks
        );
    } else {
        println!(
            "PROBE coexist-teardown-measure: FAIL — destroying the measurement aggregate stopped \
             the tap. The verification teardown ORDER becomes load-bearing; escalate with E12."
        );
    }

    // ── tearing down the tap must not break the measurement aggregate ───────
    let mut mic = match MicCapture::create(measure_config()) {
        Ok(mic) => mic,
        Err(e) => {
            println!(
                "PROBE coexist-teardown-tap: FAIL — the measurement aggregate would not come \
                 back a second time: {e:?}"
            );
            drop(playback);
            io.stop();
            let _ = tap.teardown();
            return;
        }
    };
    let mic_before = mic.counters().callbacks;
    io.stop();
    let errors = tap.teardown();
    if !errors.is_empty() {
        println!("tap teardown errors: {errors:?}");
    }
    drain(&mut mic);
    wait_for(WITNESS_WINDOW, stop);
    let mic_after = mic.counters().callbacks;
    drop(mic);
    drop(playback);

    if mic_after > mic_before {
        println!(
            "PROBE coexist-teardown-tap: PASS — the measurement aggregate kept cycling after the \
             tap aggregate was destroyed (+{} callbacks).",
            mic_after - mic_before
        );
    } else {
        println!(
            "PROBE coexist-teardown-tap: FAIL — destroying the tap aggregate stopped the \
             measurement aggregate. Escalate with E12."
        );
    }
}

fn measure_config() -> MeasureAggregateConfig {
    MeasureAggregateConfig {
        mic: match std::env::var("PARAEQ_MIC_UID") {
            Ok(uid) if !uid.is_empty() => MicSelector::Uid(uid),
            _ => MicSelector::DefaultInput,
        },
        ..MeasureAggregateConfig::default()
    }
}

/// Empty the capture ring so the counter deltas below describe THIS window.
fn drain(mic: &mut MicCapture) {
    let mut scratch = vec![0.0f64; 8192];
    while mic.capture(&mut scratch).unwrap_or(0) > 0 {}
}

// ────────────────────────────────── plumbing ─────────────────────────────────

/// What the tap's IOProc saw. Atomics only: the counting closure runs on the
/// HAL's realtime thread.
#[derive(Default)]
struct TapCounts {
    callbacks: Arc<AtomicU64>,
    nonzero: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug)]
struct TapSnapshot {
    callbacks: u64,
    nonzero: u64,
}

impl TapCounts {
    /// `nonzero` rather than `callbacks` is the witness that matters: a
    /// mic-capable default output cycles its IOProc with zeros, so a callback
    /// count proves the device is alive and nothing about whether the child's
    /// audio arrived.
    fn callback(&self) -> IoCallback {
        let callbacks = Arc::clone(&self.callbacks);
        let nonzero = Arc::clone(&self.nonzero);
        Box::new(move |block| {
            callbacks.fetch_add(1, Ordering::Relaxed);
            let any = block
                .input
                .buffers()
                .any(|(samples, _)| samples.iter().any(|s| s.abs() > NONZERO_EPSILON));
            if any {
                nonzero.fetch_add(1, Ordering::Relaxed);
            }
        })
    }

    fn snapshot(&self) -> TapSnapshot {
        TapSnapshot {
            callbacks: self.callbacks.load(Ordering::Relaxed),
            nonzero: self.nonzero.load(Ordering::Relaxed),
        }
    }
}

/// Keeps the system rendering audio for the guard's lifetime by looping
/// `afplay` on a builtin sound — the same helper shape `tests/test_hardware.rs`
/// uses, and for the same reason: the tap aggregate's IOProc delivers no
/// callbacks while the system is idle.
struct Playback {
    playing: Arc<AtomicBool>,
    player: Option<std::thread::JoinHandle<()>>,
}

impl Playback {
    fn start() -> Playback {
        let playing = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&playing);
        let player = std::thread::spawn(move || {
            while flag.load(Ordering::Relaxed) {
                let _ = Command::new("afplay")
                    .arg("/System/Library/Sounds/Submarine.aiff")
                    .status();
            }
        });
        Playback {
            playing,
            player: Some(player),
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
        if let Some(player) = self.player.take() {
            let _ = player.join();
        }
    }
}

/// Owns the spawned renderer. `Drop` kills and reaps it on EVERY exit path
/// including panic: a child left rendering would leave a private aggregate
/// wrapping the user's output device, which is the one outcome this probe must
/// never produce while asking whether that aggregate is safe.
struct ChildGuard {
    child: Option<Child>,
    stdout: Option<std::io::BufReader<std::process::ChildStdout>>,
}

impl ChildGuard {
    /// Block for the child's `CHILD-READY` line, or `None` if it never comes.
    ///
    /// Read on a helper thread because `BufRead::read_line` has no deadline and
    /// a child wedged inside a device open would otherwise hang the probe.
    fn read_ready(&mut self, timeout: Duration) -> Option<String> {
        use std::io::BufRead;
        let mut stdout = self.stdout.take()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let read = stdout.read_line(&mut line);
            let _ = tx.send(read.ok().filter(|n| *n > 0).map(|_| line));
        });
        rx.recv_timeout(timeout).ok().flatten()
    }

    /// Poll for a clean exit, then fall back to the guard's kill on `None`.
    fn wait_for_exit(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let child = self.child.as_mut()?;
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {}
                Err(_) => return None,
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_render_child(uid: &str, wrapped: bool) -> ChildGuard {
    let exe = std::env::current_exe().expect("this binary's own path");
    let mut command = Command::new(exe);
    command
        .args(["--render-child", "--uid", uid])
        .stdout(Stdio::piped());
    if wrapped {
        command.arg("--wrapped");
    }
    let mut child = command
        .spawn()
        .expect("re-spawn hal_render_probe as the renderer");
    let stdout = child.stdout.take().map(std::io::BufReader::new);
    ChildGuard {
        child: Some(child),
        stdout,
    }
}

/// Sleep in short steps so Ctrl-C still reaches the teardown below.
fn wait_for(total: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
}
