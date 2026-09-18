//! The measurement <-> ENGINE seam: every `paraeq-engine` fact the
//! verification pass reads or writes, declared as a trait here and implemented
//! exactly once, in the shell.
//!
//! Why a seam and not a dependency. MS-1 is verbatim: "`crates/paraeq-measure`
//! exists: depends on `paraeq-dsp`, no Tauri dep, no CoreAudio dep, no
//! `unsafe`. Sinks/sources are traits". The crate's own manifest comment says
//! the same in the shorter form that is easy to misread — "paraeq-dsp ONLY
//! among the workspace crates" — and `paraeq-engine` is one of the crates that
//! rule excludes. So none of `paraeq-engine`'s TYPES crosses this boundary:
//! every method below returns plain data or this module's own mirror enum.
//!
//! The shape mirrors [`seam`](crate::seam) and the engine's own `AudioBackend`:
//! this crate declares, the platform layer implements, and the whole
//! verification gate sequence stays drivable by a mock with no engine, no
//! device and no process.
//!
//! **`self_excluded` is deliberately absent from [`EngineFacts`]** — see the
//! note under the trait.

use crate::MeasureError;

/// Read-only engine facts the verification gates need.
///
/// Every value comes from ONE `EngineState` snapshot or from the controller's
/// own realtime shared block. Methods are alphabetical; the comment on each
/// says which gate consumes it, because a fact with no consumer is a fact that
/// will be read by a future gate that nobody reviewed.
pub trait EngineFacts: Send {
    /// Bands the engine could NOT design at the live rate and dropped.
    ///
    /// Gate 2 refuses on `> 0`: a partially-installed cascade is not the
    /// plan's cascade, and predicting the plan's response while the chain runs
    /// a subset would blame the chain for our own prediction fault.
    fn bands_dropped(&self) -> usize;

    /// User bypass. Gate 2 refuses when true — a bypassed chain measures the
    /// uncorrected path a second time.
    fn bypass(&self) -> bool;

    /// The engine's +-1.0 output-clamp counter, read as a DELTA across the
    /// verification window. This is the NEAR side of the transducer; the
    /// capture's own clip metering is the far side, and neither implies the
    /// other.
    fn clipped_samples(&self) -> u64;

    /// Is a correction installed at all? Gate 2 refuses when false: there is
    /// nothing to verify.
    fn correction_installed(&self) -> bool;

    /// `Some(stream_rate_hz)` means the chain is running coefficients designed
    /// for a different rate. Gate 2 refuses on `Some`.
    fn correction_rate_mismatch_hz(&self) -> Option<f64>;

    /// Is the engine ENGAGED?
    ///
    /// NOT `status == Running`. `Running` is one of eight lifecycle states and
    /// several of the others — `Idle`, `InputSilent` — describe an engine that
    /// is fully engaged and merely has nothing to do, which is exactly the
    /// state a verification pre-roll produces. Definition:
    ///
    /// ```text
    /// enabled && stream.is_some()
    ///   && !matches!(status, Stopped | Failed | AutoDisabledNoInput)
    /// ```
    fn engine_engaged(&self) -> bool;

    /// The user's trim, read back after the pass pins it to 0.0 (gate 4).
    fn gain_db(&self) -> f32;

    /// The auto preamp AS ARMED, linear — the number the corrected path is
    /// actually multiplying by, not the number the export text carries.
    ///
    /// `None` means NO CORRECTION IS INSTALLED. Never default it to 1.0: a
    /// missing preamp and a unity preamp predict different responses, and
    /// collapsing them makes the residual blame the chain for our own default.
    fn installed_preamp_lin(&self) -> Option<f32>;

    /// The chain's reported latency budget, if the stream is up.
    fn latency_ms(&self) -> Option<f64>;

    /// SOS rows the stability funnel replaced with the identity section.
    ///
    /// Does NOT gate — the realized-cascade response substitutes identically,
    /// so the prediction already accounts for it — and is attached as evidence
    /// so a surprising residual has a visible first thing to look at.
    fn sections_substituted(&self) -> usize;

    /// The lifecycle status as a plain mirror, so a refusal can NAME what it
    /// saw rather than saying "not engaged".
    fn status_kind(&self) -> EngineStatusKind;

    /// The LIVE stream rate. The realized cascade response and the gate-2
    /// preamp recomputation are both evaluated here, never at the design rate.
    fn stream_rate_hz(&self) -> Option<f64>;

    /// Non-latching realtime activity, for the silent-failure witness.
    ///
    /// DECLARED HERE, NOT ON [`TapStatus`](crate::seam::TapStatus). The shipped
    /// `TapStatus` implementor is a single shared bool cell with no path to the
    /// controller's per-block counters, so putting activity on that trait would
    /// split one fact across two seams that can disagree. This is a CONTROLLER
    /// fact like every other method on this trait.
    ///
    /// `None` before the first start and after a teardown: there is no
    /// realtime block to read, and a witness window treats that as "cannot
    /// witness" and refuses rather than assuming.
    fn tap_activity(&self) -> Option<TapActivity>;
}

// `self_excluded` is NOT on `EngineFacts`, and this is the reason, quoted from
// the shipped `EngineState.self_excluded` doc so it is not re-litigated:
//
//   "Telemetry, not the gate. This is published at most once per tick and is
//    what the UI renders; `paraeq-measure` polls the backend's own live witness
//    (`paraeq_coreaudio::backend::ExclusionWitness`) at every gate that
//    precedes emission, so a safety check is never up to a tick stale. Both
//    come from the same cell, so they cannot disagree."
//
// So the MS-6 gate reads `TapStatus::self_excluded()` — the live witness,
// already declared in `seam.rs` and already implemented once — and this trait
// does not carry a second copy of the fact.

/// Non-latching realtime counters.
///
/// NOT on `EngineState` by design: a monotonic per-block counter inside the
/// controller's publish-if-changed comparison would emit a snapshot plus a UI
/// event every tick. Contrast [`EngineFacts::bands_dropped`] and
/// [`EngineFacts::sections_substituted`], which change only at install and are
/// therefore safe to publish and compare exactly.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TapActivity {
    /// Total realtime callbacks observed since the session started.
    pub callbacks: u64,
    /// Blocks carrying at least one nonzero input sample — the raw "audio is
    /// flowing through the tap" signal.
    pub nonzero_blocks: u64,
}

/// This crate's own mirror of the engine's lifecycle status.
///
/// A mirror, not an import: MS-1 forbids naming `paraeq_engine::EngineStatus`
/// here. EIGHT variants, matching the shipped enum one for one. Payloads
/// (`since_ms`, `after_ms`, `reason`) are deliberately dropped — no gate reads
/// them, and carrying them would invite one to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineStatusKind {
    AutoDisabledNoInput,
    Failed,
    Idle,
    InputSilent,
    NoInputDetected,
    Running,
    Starting,
    Stopped,
}

/// The two things the verification pass WRITES.
///
/// Separated from [`EngineFacts`] so a read-only consumer cannot be handed a
/// mutator by accident — the same split the engine already keeps between its
/// state snapshot and its command channel.
pub trait EngineControl: Send {
    /// Suspend the fail-open watchdog for the duration of the session, and
    /// return a token released on every exit path including panic.
    ///
    /// Called FIRST, before any [`EngineFacts`] read: the auto-disable state is
    /// sticky, and the verification pre-roll is exactly the silence that arms
    /// it. A lease acquired after the first gate is a lease acquired after the
    /// window it exists to cover.
    fn acquire_measurement_lease(&mut self)
        -> Result<Box<dyn MeasurementLeaseToken>, MeasureError>;

    /// Pin the user's trim to `db` and return an RAII token that restores the
    /// previous value on `Drop`, including during unwinding — the same
    /// contract [`VolumeControl`](crate::seam::VolumeControl) carries, for the
    /// same reason: the system must never be left in a measurement posture.
    ///
    /// This is the USER's trim, not the correction's preamp. Zeroing it is
    /// safe precisely because the computed preamp lives on the installed
    /// correction and is applied on the corrected path only; pinning it here
    /// would otherwise erase the very thing under test.
    fn pin_gain_db(&mut self, db: f32) -> Result<GainPin, MeasureError>;
}

/// Opaque lease token. This crate never names the engine's lease type; all it
/// needs is the ability to ask whether the lease is still held and to drop it.
pub trait MeasurementLeaseToken: Send {
    fn is_held(&self) -> bool;
}

/// The mechanism half of a [`GainPin`]: set the trim back to this value.
///
/// `FnOnce` because it runs exactly once, and boxed because the implementor is
/// on the other side of the seam — this crate supplies the policy and never
/// names the engine handle the closure captures.
pub type GainRestore = Box<dyn FnOnce(f32) -> Result<(), MeasureError> + Send>;

/// RAII restore of the user's trim (see [`EngineControl::pin_gain_db`]).
///
/// Concrete rather than a trait because the POLICY — restore exactly once, on
/// every exit path, and surface the failure rather than masking it — belongs
/// to this crate; the implementor supplies only the mechanism, as one closure.
///
/// `Drop` is the backstop, not the happy path: a caller that wants to SEE a
/// failed restore calls [`GainPin::restore`] and reads the `Err`. A `Drop` that
/// panicked on a failed restore would abort the teardown ladder behind it,
/// which is the one thing a teardown ladder must never do.
pub struct GainPin {
    pinned_db: f32,
    previous_db: f32,
    restore: Option<GainRestore>,
}

impl GainPin {
    /// Build a pin. `restore` is called with `previous_db` exactly once, by
    /// [`GainPin::restore`] or by `Drop`, whichever runs first.
    pub fn new(previous_db: f32, pinned_db: f32, restore: GainRestore) -> GainPin {
        GainPin {
            pinned_db,
            previous_db,
            restore: Some(restore),
        }
    }

    /// The value the trim was pinned to (`0.0` for the verification pass).
    pub fn pinned_db(&self) -> f32 {
        self.pinned_db
    }

    /// The value the trim held before the pin, and the value `Drop` restores.
    pub fn previous_db(&self) -> f32 {
        self.previous_db
    }

    /// Restore now and report the result. Idempotent: a second call is `Ok(())`
    /// and does not call the engine again.
    pub fn restore(&mut self) -> Result<(), MeasureError> {
        match self.restore.take() {
            Some(restore) => restore(self.previous_db),
            None => Ok(()),
        }
    }
}

impl Drop for GainPin {
    fn drop(&mut self) {
        // The restore's own failure has nowhere to go here and must not
        // panic — `Drop` runs during unwinding. A caller that needs to see it
        // calls `restore()` explicitly first, which makes this a no-op.
        let _ = self.restore();
    }
}

impl std::fmt::Debug for GainPin {
    /// Manual: the restore closure is not `Debug`. Shows the two numbers,
    /// which is what an assertion in a test wants to print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GainPin")
            .field("pinned_db", &self.pinned_db)
            .field("previous_db", &self.previous_db)
            .field("restored", &self.restore.is_none())
            .finish()
    }
}
