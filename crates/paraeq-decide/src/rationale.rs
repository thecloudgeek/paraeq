//! The rationale copy: rendered HERE, in Rust, from a typed
//! [`RationaleKey`](crate::decision::RationaleKey).
//!
//! "Rendered here, in Rust. The UI is a text field, not an author"
//! (`Rationale::text`). Rationale assembled in the renderer is one of the seven
//! review-blocking violations the seam names: copy is reviewable in the decision
//! table precisely because Rust renders it, and string-templating it in
//! TypeScript means the two modes' explanations drift with no test to notice.
//!
//! **This file is what a copy review reads.** Every string below is the
//! decision table's own `Rationale (rendered by Rust)` column, character for
//! character, with the table's `{placeholders}` replaced by the values the
//! rules computed. One function per [`RationaleKey`], so a reviewer can diff
//! this file against the table row by row and a missing row does not compile.
//!
//! **No string templating may exist anywhere else in the workspace.** If a
//! second renderer appears, the two modes' explanations can drift with no test
//! to notice, which is exactly the review-blocking violation the seam names.
//!
//! **Two strings are NOT quotations, and both are flagged where they are
//! written:** [`clock_adjust`] has no row in the decision table at all (the
//! decision comes from the 2026-07-21 record's §Q6 drawer toggle, which
//! specifies the control and not the copy), and
//! [`transition_hz_fallback`] is B17's — the decision table writes one
//! sentence for `transition_hz` and it is written for a MEASUREMENT, so the
//! no-crossing case needed a second string that claims nothing
//! (`docs/specs/2026-07-15-wizard-design.md:566`). [`transition_hz`] itself is
//! still the table's, character for character.

use crate::decision::{Rationale, RationaleKey};
use paraeq_dsp::targets::TransducerClass;

/// One rationale. Private so that every string in this module arrives through
/// a named, reviewable function rather than as an ad-hoc pair.
fn rationale(key: RationaleKey, text: String) -> Rationale {
    Rationale { key, text }
}

/// `align_spl_band`.
pub(crate) fn align_spl_band() -> Rationale {
    rationale(
        RationaleKey::AlignSplBand,
        "We level-matched the positions first — otherwise the mic positions \
         nearest the speaker would dominate the average."
            .to_string(),
    )
}

/// `authority`. `f_t` is the decided `transition_hz`.
///
/// The copy quotes `transition_hz`, which is the ONE place the two rows touch:
/// the authority *curve* is composed from σ(f) and the excursion envelope and
/// never from `f_t` ("`transition_hz` … is never an input to the authority
/// weight"). The prose names the frequency because that is what the sentence is
/// about; the numbers do not move with it.
pub(crate) fn authority(transition_hz: f64) -> Rationale {
    rationale(
        RationaleKey::Authority,
        format!(
            "Above {transition_hz:.0} Hz the dips are sound cancelling itself \
             out. No EQ can fix that — it just makes it louder and still \
             cancelled."
        ),
    )
}

/// `averaging`.
pub(crate) fn averaging() -> Rationale {
    rationale(
        RationaleKey::Averaging,
        "One mic position sitting in a cancellation would otherwise drag the \
         average down and make us boost a hole that isn't really there."
            .to_string(),
    )
}

/// `class`. `{class}` renders through `TransducerClass::display_name`, which is
/// the owner-decided UI string for the enum — never the `Debug` spelling.
pub(crate) fn class(class: TransducerClass) -> Rationale {
    rationale(
        RationaleKey::Class,
        format!(
            "You told us these are {}. The levels we measured are consistent \
             with that.",
            class.display_name()
        ),
    )
}

/// `clock_adjust`.
///
/// **NOT a quotation — there is no decision-table row for this decision.** The
/// decision itself comes from the 2026-07-21 record's §Q6 ("Drawer override:
/// clock-adjust toggle with the estimated ppm shown, **defaulted on**"), which
/// specifies the control and the default and says nothing about the copy. This
/// string is written in the table's voice so the drawer is not blank, and it is
/// flagged here rather than in a commit message so that a copy review sees the
/// one line it has to rule on. The estimated ppm is deliberately NOT
/// interpolated: it is carried as `Diagnostic::value` and as
/// `EvidenceLabel::TwoClockSkewPpm`, and a number in two places is a number
/// that can disagree with itself.
pub(crate) fn clock_adjust() -> Rationale {
    rationale(
        RationaleKey::ClockAdjust,
        "Your mic and speakers run on different clocks. We measured the drift \
         and took it out before analysing anything."
            .to_string(),
    )
}

/// `correction_kind`. `{n}` is the number of bands the fit EMITTED, so the copy
/// counts the tone controls the user actually got.
pub(crate) fn correction_kind(bands: usize) -> Rationale {
    rationale(
        RationaleKey::CorrectionKind,
        format!(
            "We used {bands} tone controls rather than a convolution filter — \
             same result, no extra delay."
        ),
    )
}

/// `correction_range`.
pub(crate) fn correction_range() -> Rationale {
    rationale(
        RationaleKey::CorrectionRange,
        "We don't try to flatten your speaker's natural roll-off. That only \
         burns headroom and drives the woofer past where it can go."
            .to_string(),
    )
}

/// `fdw_post_cycles`.
pub(crate) fn fdw_post_cycles() -> Rationale {
    rationale(
        RationaleKey::FdwPostCycles,
        "At 30 Hz the window is half a second, so we keep your room's bass. At \
         10 kHz it's 1.5 ms, so we throw the reflections away. That's what \
         your ear does."
            .to_string(),
    )
}

/// `fdw_pre_cycles`.
pub(crate) fn fdw_pre_cycles() -> Rationale {
    rationale(
        RationaleKey::FdwPreCycles,
        "We look further after the impulse than before it — there's nothing \
         before it but noise."
            .to_string(),
    )
}

/// `flatness_target_db`. `{flat}` is the decided target, rendered plainly —
/// `3.0_f64` displays as "3", which is how the table's own sentence reads.
pub(crate) fn flatness_target_db(flatness_db: f64) -> Rationale {
    rationale(
        RationaleKey::FlatnessTargetDb,
        format!(
            "Chasing flatter than {flatness_db} dB in a room means fighting \
             your chair, not your speakers."
        ),
    )
}

/// `left_window_ms`. `{t_peak}` is the measured direct-arrival time, not the
/// window: the sentence is about how much room the capture left us.
pub(crate) fn left_window_ms(peak_time_ms: f64) -> Rationale {
    rationale(
        RationaleKey::LeftWindowMs,
        format!(
            "Your Mac's audio path means there are only {peak_time_ms:.0} ms \
             before the impulse arrives. We use all of them."
        ),
    )
}

/// `low_corner_hz`.
pub(crate) fn low_corner_hz(low_corner_hz: f64) -> Rationale {
    rationale(
        RationaleKey::LowCornerHz,
        format!(
            "Your speakers reach down to about {low_corner_hz:.0} Hz. We \
             measured that; we didn't assume it."
        ),
    )
}

/// `max_filters`. `{n}` is the number of bands the fit EMITTED, not the cap:
/// the sentence's whole point is "not because {n} is a nice number", which is
/// false of a cap. `{flat}` is the decided `flatness_target_db`.
pub(crate) fn max_filters(bands: usize, flatness_db: f64) -> Rationale {
    rationale(
        RationaleKey::MaxFilters,
        format!(
            "We used {bands} filters because that's what it took to get within \
             {flatness_db} dB — not because {bands} is a nice number."
        ),
    )
}

/// `positions_n`. `{noun}` is the path's own word for one capture; see
/// [`crate::rules::position_noun`] for why it is not `reposition_noun`.
pub(crate) fn positions_n(positions: usize, noun: &str) -> Rationale {
    rationale(
        RationaleKey::PositionsN,
        format!(
            "We averaged {positions} {noun}s. More positions mean we can tell \
             your room's problems apart from your chair's."
        ),
    )
}

/// `preamp_db`. `{p}` is how far everything was turned DOWN, so it renders the
/// magnitude: `preamp_db` is `≤ 0` by construction and "turned everything down
/// −3.2 dB" says the opposite of what happened.
pub(crate) fn preamp_db(preamp_db: f64) -> Rationale {
    rationale(
        RationaleKey::PreampDb,
        format!(
            "We turned everything down {:.1} dB to make room for the boosts. \
             That's normal, and it's why it may sound quieter at first.",
            -preamp_db
        ),
    )
}

/// `q_cap`.
pub(crate) fn q_cap() -> Rationale {
    rationale(
        RationaleKey::QCap,
        "A boost filter is a resonance. We won't build one that rings longer \
         than the room problem it's fixing."
            .to_string(),
    )
}

/// `right_window_ms`. `{t}` is the decided window and `{f_min}` the STRICTER
/// 1/6-octave resolution limit it implies — `gating::resolution_limit_hz`, not
/// `1/T`.
pub(crate) fn right_window_ms(right_window_ms: f64, resolution_limit_hz: f64) -> Rationale {
    rationale(
        RationaleKey::RightWindowMs,
        format!(
            "We keep {right_window_ms} ms of your room's decay. Below \
             {resolution_limit_hz:.0} Hz this measurement can't resolve \
             1/6-octave detail, so we grey it out."
        ),
    )
}

/// `shelves`.
pub(crate) fn shelves() -> Rationale {
    rationale(
        RationaleKey::Shelves,
        "This is a broad tilt, not a bump — a shelf fixes it with one filter \
         instead of six."
            .to_string(),
    )
}

/// `smoothing`.
pub(crate) fn smoothing() -> Rationale {
    rationale(
        RationaleKey::Smoothing,
        "Fine detail in the bass where we correct hard; coarse up top where we \
         only shape tone."
            .to_string(),
    )
}

/// `target`, coupler path, EARS HEQ/HPN/IDF cal: forced to `flat`.
pub(crate) fn target_cal_baked_in() -> Rationale {
    rationale(
        RationaleKey::TargetCalBakedIn,
        "Your EARS calibration already has a target baked into it. Applying \
         another would apply it twice."
            .to_string(),
    )
}

/// `target`, coupler path, normal cal. `{name}` is the matched curve and `{k}`
/// the size of the CLASS-FILTERED candidate set — the number we actually tried,
/// not the number that was loaded.
pub(crate) fn target_matched(name: &str, candidates: usize) -> Rationale {
    rationale(
        RationaleKey::TargetMatched,
        format!(
            "Your headphones are closest to {name} — that's the curve we \
             matched, out of {candidates} we tried."
        ),
    )
}

/// `target`, room path. `{tilt}` is the house curve's slope, rendered plainly.
pub(crate) fn target_room_parametric(tilt_db_per_oct: f64) -> Rationale {
    rationale(
        RationaleKey::TargetRoomParametric,
        format!(
            "Rooms don't get matched to a curve — the room's own response is \
             the thing we're fixing. We used a {tilt_db_per_oct} dB/octave \
             house curve with a gentle bass lift."
        ),
    )
}

/// `transition_hz`, the DERIVED case: the σ-crossing scan found a crossing.
///
/// The table's sentence is written for a measurement, which is why it has a
/// sibling: read against the 200 Hz fallback, "Below {f_t:.0} Hz your room's
/// problems are the same everywhere you sit" is a claim about a measurement
/// that was never made. [`crate::rules`] renders this one only when
/// [`crate::rules`]'s σ-crossing scan returned a crossing, and
/// [`transition_hz_fallback`] otherwise.
pub(crate) fn transition_hz(transition_hz: f64) -> Rationale {
    rationale(
        RationaleKey::TransitionHz,
        format!(
            "Below {transition_hz:.0} Hz your room's problems are the same \
             everywhere you sit, so we fix them. Above that they change with \
             every head movement."
        ),
    )
}

/// `transition_hz`, the FALLBACK case: the σ-crossing scan found nothing.
///
/// **NOT a quotation — the decision table has one sentence for this row and it
/// is the derived one.** `docs/specs/2026-07-15-wizard-design.md:566` is why a
/// second string exists: if excess-group-delay masking slips to v1.1, "the
/// 200 Hz *fallback constant* is what ships — and it must be labelled a
/// fallback in the code, the UI, and the log, never a rule". The rationale IS
/// the UI ("Rendered here, in Rust. The UI is a text field, not an author"), so
/// the UI leg of that MUST is this function. The copy therefore makes NO
/// measurement claim: it says the measurement could not be made, names the
/// number it assumed instead, and calls it a fallback.
///
/// **Selected on the ANALYSIS FACT, never on [`crate::decision::Source`].**
/// Source would be the obvious selector and it would fork the crate's
/// idempotence invariant — overriding a decision to the value auto already
/// chose must move `source` and nothing else, so copy that reads `source`
/// changes under an override that changed nothing. The scan's own `None` is an
/// input, not an output, and no override can reach it; a user who pins this row
/// to 200.0 on an unmeasurable room still gets this sentence, because it is
/// still true.
pub(crate) fn transition_hz_fallback(transition_hz: f64) -> Rationale {
    rationale(
        RationaleKey::TransitionHz,
        format!(
            "We couldn't measure where your room stops being the same \
             everywhere you sit, so we used the usual {transition_hz:.0} Hz as \
             a fallback. That's an assumption, not something we found in your \
             room."
        ),
    )
}

/// `window_type`.
pub(crate) fn window_type() -> Rationale {
    rationale(
        RationaleKey::WindowType,
        "A soft-edged window. A hard cut smears the measurement across \
         frequency."
            .to_string(),
    )
}
