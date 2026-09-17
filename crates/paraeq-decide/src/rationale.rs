//! The rationale copy: rendered HERE, in Rust, from a typed
//! [`RationaleKey`](crate::decision::RationaleKey).
//!
//! "Rendered here, in Rust. The UI is a text field, not an author"
//! (`Rationale::text`). Rationale assembled in the renderer is one of the seven
//! review-blocking violations the seam names: copy is reviewable in the decision
//! table precisely because Rust renders it, and string-templating it in
//! TypeScript means the two modes' explanations drift with no test to notice.
//!
//! **B7a ships the module and one function; B7b fills it.** Every string in the
//! decision table interpolates a number the RULES compute ("We averaged {n}
//! {noun}s", "Below {f_t:.0} Hz your room's problems are the same everywhere you
//! sit"), so the copy cannot land before the rules that supply the
//! interpolations. Until then a rationale carries its key and an empty text: the
//! key is the machine-readable half and is already correct, and an empty string
//! is visibly unwritten rather than plausibly wrong. B17 adds the first real
//! string here — the one that labels `transition_hz`'s 200 Hz fallback AS a
//! fallback.

use crate::decision::{Rationale, RationaleKey};

/// The placeholder rationale for `key`.
///
/// The key is the contract — it is what the tests and any future localization
/// key off, and it is what tells the drawer WHICH copy applies (`target` carries
/// three keys because its copy differs materially by which selection rule
/// fired). The text is B7b's.
pub(crate) fn placeholder(key: RationaleKey) -> Rationale {
    Rationale {
        key,
        text: String::new(),
    }
}
