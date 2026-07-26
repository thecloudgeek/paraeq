//! The cap is the type; the table is data.
//!
//! Enforcement is type-level rather than a runtime `if` at the playback site or
//! a UI slider limit, because both of those are bypassable by the next caller.
//! [`SweepLevel::new`] is the sole constructor, the newtype's field is private,
//! and a [`StimulusSink`](crate::seam::StimulusSink) accepts nothing else — so
//! the unguarded hot path does not typecheck.

use paraeq_dsp::targets::TransducerClass;
use std::ops::RangeInclusive;
use std::sync::LazyLock;

/// MS-17 envelope for the two coupler paths, dB SPL per dBFS RMS. Contains the
/// 104 dB/dBFS chain that just reaches the 84 dB target at the −20 dBFS cap,
/// with wide margin either side. See
/// [`TransducerCaps::sensitivity_envelope_spl_per_dbfs`] — OPEN \[NEEDS DATA\].
const COUPLER_SENSITIVITY_ENVELOPE: RangeInclusive<f64> = 85.0..=130.0;

/// MS-17 envelope for the two room paths, dB SPL per dBFS RMS. Contains the
/// 87 dB/dBFS chain that just reaches the 75 dB target at the −12 dBFS cap.
/// Wider at the bottom than the coupler's because listening distance varies by
/// far more than coupler geometry does. OPEN \[NEEDS DATA\].
const ROOM_SENSITIVITY_ENVELOPE: RangeInclusive<f64> = 65.0..=120.0;

/// Unconditional ceiling, checked before the table and independent of it.
///
/// -3.01 dBFS RMS is simultaneously the level of the unscaled `generate_sweep`
/// output and REW's absolute maximum; nothing this crate emits may approach it,
/// whatever a future edit to [`caps_for`]'s table says. The redundancy with the
/// per-class caps (all of which are far below this) is deliberate: the table is
/// data and data gets edited, while this is the floor of the design.
pub const ABSOLUTE_MAX_DBFS_RMS: f64 = -3.0;

/// A sweep level that has been checked against a class's cap. Constructible
/// only through [`SweepLevel::new`].
///
/// `Copy`, so handing one to a sink cannot move it out of the session log that
/// has to record it. Deliberately not `Default`, not `From<f64>`, and not
/// `Deserialize`: each would be a second constructor, and a second constructor
/// is a second place for the caps to not be applied.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct SweepLevel(f64);

impl SweepLevel {
    /// The sole constructor (MS-2).
    ///
    /// Check order is load-bearing. Non-finite first, because `NaN > cap` is
    /// false and a NaN would otherwise pass both ceilings and reach a sink as
    /// an unbounded level — the same NaN-blindness the spec records at
    /// `shared.rs::process_block`'s `if a > peak` scan, where a block of NaNs
    /// reads to the watchdog as silence. Then the absolute maximum, which binds
    /// regardless of the table and would be unreachable dead code if the table
    /// were consulted first. Then the class cap.
    pub fn new(dbfs_rms: f64, class: TransducerClass) -> Result<Self, LevelError> {
        if !dbfs_rms.is_finite() {
            return Err(LevelError::NotFinite { dbfs_rms });
        }
        if dbfs_rms > ABSOLUTE_MAX_DBFS_RMS {
            return Err(LevelError::OverAbsoluteMax { dbfs_rms });
        }
        let cap_dbfs_rms = caps_for(class).sweep_level_dbfs_rms;
        if dbfs_rms > cap_dbfs_rms {
            return Err(LevelError::OverClassCap {
                cap_dbfs_rms,
                class,
                dbfs_rms,
            });
        }
        Ok(Self(dbfs_rms))
    }

    pub fn dbfs_rms(self) -> f64 {
        self.0
    }
}

/// A refusal, not a warning. `Copy + PartialEq` so a session log records the
/// refusal verbatim and a test asserts on it.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum LevelError {
    #[error("level {dbfs_rms} dBFS RMS is not finite")]
    NotFinite { dbfs_rms: f64 },
    #[error(
        "level {dbfs_rms} dBFS RMS exceeds the absolute maximum {ABSOLUTE_MAX_DBFS_RMS} dBFS RMS"
    )]
    OverAbsoluteMax { dbfs_rms: f64 },
    #[error("level {dbfs_rms} dBFS RMS exceeds the {class:?} cap of {cap_dbfs_rms} dBFS RMS")]
    OverClassCap {
        cap_dbfs_rms: f64,
        class: TransducerClass,
        dbfs_rms: f64,
    },
}

/// The per-class safety table (measurement-safety-design.md § "Targets and
/// caps, per transducer class"), keyed by the class the user selected.
///
/// Not a wire type: hard caps are non-overridable, including from the Advanced
/// drawer, so nothing here is ever deserialized from a settings file. Targets
/// and warn thresholds are overridable *within* caps — those overrides live in
/// `paraeq-decide`'s `Overrides` and are bounded by these numbers, never
/// replacements for them.
///
/// No longer `Copy`: `sensitivity_envelope_spl_per_dbfs` is a
/// `RangeInclusive`, which is not `Copy` (it is an iterator). The rows are
/// `LazyLock` statics and [`caps_for`] still hands out `&'static`, so nothing
/// clones a table row on any path.
#[derive(Clone, Debug, PartialEq)]
pub struct TransducerCaps {
    /// `None` on the coupler paths: per-DUT, because a headphone's usable low
    /// corner is a property of the headphone. `Some` on the room paths, where
    /// the number protects the box. Never extended downward, and REW's
    /// half-start convention (sweep from `f_start/2`) is rejected outright:
    /// excursion rises as `1/f²` below box tuning and a ported enclosure
    /// unloads entirely, so a 20 Hz request driven at REW's 10 Hz is 25×
    /// at-tuning excursion with no air spring left.
    ///
    /// The room values are also `paraeq_decide::PathProfile::sweep_f_start_hz`.
    /// Two tables, one number, by crate-DAG necessity — this table owns it (it
    /// is a driver-excursion safety limit), and
    /// `test_level.rs::both_crates_tabulate_the_same_room_sweep_start` compares
    /// the two through a dev-dependency, so they cannot drift apart silently.
    pub f_start_hz: Option<f64>,
    /// § The Two-Clock Complication. Tabulated rather than derived, and the
    /// same for every class, because the reason is not per-class: the "trade
    /// level for length" move (16× duration buys back 12 dB) is unavailable —
    /// the skew evidence stops at 5 s and 16× length is exactly the
    /// tweeter-overheating regime REW warns about. The ladder must not
    /// auto-extend past this.
    pub max_sweep_len_s: f64,
    /// Hard refuse. Non-overridable. Room paths refuse 10 dB below coupler
    /// paths because a room measurement necessarily happens in a space a person
    /// may be standing in, whereas a coupler measurement is nominally on a jig.
    ///
    /// OPEN (spec): the 85/100 dB thresholds are attributed to EN 50332 and
    /// IEC 62368-1 cl. 10.6, but the clause numbers and their applicability to
    /// a software product are unverified. Defensible as engineering thresholds
    /// regardless; confirm before any published compliance claim.
    pub spl_refuse_db: f64,
    /// What the solve aims for. 84 dB for both coupler paths, not the corpus's
    /// 94 dB (1 Pa) IEM reference: ParaEQ needs no absolute reference, head
    /// detection is unsolved, and running 10 dB hot for no SNR benefit is
    /// unjustified. The hearing-safety and tweeter-safety optima both point
    /// quiet.
    pub spl_target_db: f64,
    /// Deliberately just above the coupler target: at target the warning never
    /// fires, so any solve that lands hot is immediately visible.
    pub spl_warn_db: f64,
    /// MS-17's chain-sensitivity envelope, in dB SPL per dBFS RMS — i.e. the
    /// SPL the whole chain would produce at 0 dBFS RMS. Solved sensitivity
    /// outside this **refuses** (`SensitivityOutOfEnvelope`) rather than
    /// escalating, because escalating into an empty jig is precisely how the
    /// 122 dB scenario happens: the user then puts the IEM in.
    ///
    /// This detects the **chain**, not the head. It is a gross-error
    /// discriminator — empty jig, headphone off the coupler, sweep misrouted
    /// to the laptop speakers, wrong class selected — and nothing finer. A
    /// headphone on a head with in-envelope sensitivity is indistinguishable
    /// from one on a jig; the acknowledgement gate and the quiet targets are
    /// the mitigation, and there is no detector.
    ///
    /// **OPEN \[NEEDS DATA\]:** the spec requires the envelope (MS-17) but
    /// states no numbers, so these are engineering estimates, deliberately
    /// wide. One end is not free, though, and
    /// `test_ladder.rs::the_envelope_admits_every_chain_that_can_reach_target`
    /// pins it: the envelope must contain
    /// `spl_target_db − sweep_level_dbfs_rms` (104 dB SPL/dBFS coupler, 87
    /// room), the least sensitive chain that can still hit target at the class
    /// cap — otherwise this table would refuse chains the caps table calls
    /// legal. The upper bounds say "no real transducer is this loud at full
    /// scale; suspect the cal or the input gain". Settle them by measuring S
    /// on the EARS rig and on a real room system, which Stage 5's headless
    /// end-to-end run makes possible for the first time.
    pub sensitivity_envelope_spl_per_dbfs: std::ops::RangeInclusive<f64>,
    /// The ladder's first rung *and* the cap [`SweepLevel::new`] enforces.
    ///
    /// The spec calls this "a starting point for the solve, not the emitted
    /// level — the solve overrides it". The reading implemented here is that
    /// the solve overrides it *downward only*: it is the one per-class dBFS
    /// number the table carries, MS-2 requires a per-class cap in these units,
    /// and quiet is the safe direction on every axis this spec argues. A chain
    /// so insensitive it needs more than this to reach target is a chain the
    /// envelope check (MS-17) refuses rather than escalates into.
    pub sweep_level_dbfs_rms: f64,
}

static BOOKSHELF: LazyLock<TransducerCaps> = LazyLock::new(|| TransducerCaps {
    f_start_hz: Some(30.0),
    max_sweep_len_s: 5.5,
    sensitivity_envelope_spl_per_dbfs: ROOM_SENSITIVITY_ENVELOPE,
    spl_refuse_db: 90.0,
    spl_target_db: 75.0,
    spl_warn_db: 85.0,
    sweep_level_dbfs_rms: -12.0,
});

static FLOORSTANDER: LazyLock<TransducerCaps> = LazyLock::new(|| TransducerCaps {
    f_start_hz: Some(20.0),
    max_sweep_len_s: 5.5,
    sensitivity_envelope_spl_per_dbfs: ROOM_SENSITIVITY_ENVELOPE,
    spl_refuse_db: 90.0,
    spl_target_db: 75.0,
    spl_warn_db: 85.0,
    sweep_level_dbfs_rms: -12.0,
});

static IN_EAR: LazyLock<TransducerCaps> = LazyLock::new(|| TransducerCaps {
    f_start_hz: None,
    max_sweep_len_s: 5.5,
    sensitivity_envelope_spl_per_dbfs: COUPLER_SENSITIVITY_ENVELOPE,
    spl_refuse_db: 100.0,
    spl_target_db: 84.0,
    spl_warn_db: 85.0,
    // 8 dB below REW's -12 dBFS default.
    sweep_level_dbfs_rms: -20.0,
});

static OVER_EAR: LazyLock<TransducerCaps> = LazyLock::new(|| TransducerCaps {
    f_start_hz: None,
    max_sweep_len_s: 5.5,
    sensitivity_envelope_spl_per_dbfs: COUPLER_SENSITIVITY_ENVELOPE,
    spl_refuse_db: 100.0,
    spl_target_db: 84.0,
    spl_warn_db: 85.0,
    sweep_level_dbfs_rms: -20.0,
});

/// The caps are derivable from the class alone, which is why
/// [`SweepLevel::new`] takes one: a second argument would be a second place to
/// disagree. Mirrors `paraeq_decide::profile_for`.
pub fn caps_for(class: TransducerClass) -> &'static TransducerCaps {
    match class {
        TransducerClass::Bookshelf => &BOOKSHELF,
        TransducerClass::Floorstander => &FLOORSTANDER,
        TransducerClass::InEar => &IN_EAR,
        TransducerClass::OverEar => &OVER_EAR,
    }
}
