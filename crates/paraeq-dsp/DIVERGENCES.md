# Deliberate divergences from the Python oracle

Spec requirement (2026-07-02 design): every place the Rust intentionally
differs from `prototype/paraeq/`, so a red parity test is always actionable.

1. **`design_fir_correction` drops the `freqs` parameter.** The Python accepts
   `freqs` but never uses it (resampling is positional). Rust omits it.
2. **Inverse-FFT normalization is explicit.** numpy divides by n inside
   `ifft/irfft`; rustfft/realfft do not — every inverse in Rust carries `1/n`.
   Numerically identical; structurally different.
3. **`compute_frequency_response` / `deconvolve` are mono.** The Python accepts
   `(samples, channels)` 2-D arrays; Rust callers loop channels explicitly.
4. **`fractional_octave_smooth` window scan is O(n²) by design** to mirror the
   oracle sample-for-sample. Any future O(n) optimization must keep the fixture
   test byte-identical.
5. **Errors are `Result<_, DspError>`** where the Python raises `ValueError`/
   `FileNotFoundError`. Same conditions, different mechanism.
6. **`compensation.rs` ParaEQ-CSV comment/blank handling is more lenient than
   the oracle.** Python's CSV path (`csv.reader` + `row[0].startswith("#")`)
   checks the raw, untrimmed first field, so an indented `"  # comment"` line
   or a whitespace-only line raises `ValueError` there. The Rust parser
   (`parse_paraeq_csv`) trims each line before checking for `#`/emptiness, so
   both cases are silently skipped instead of erroring. Reachable only on
   hand-edited compensation files; no current fixture exercises it.
7. **`targets.rs` CSV metadata/data-row parsing is stricter in two ways than
   the oracle, in opposite directions.**
   - *Metadata keys*: Python recognizes `# key: value` headers via
     `content.lstrip("#")`, which strips *any number* of leading `#`
     characters — so `## name: Foo` still sets `name`. Rust's
     `strip_prefix('#')` strips exactly one `#`, so a double-`##` header is
     left with a leading `#` in the key and silently fails the
     `_METADATA_KEYS` (Rust: `match key.trim().to_lowercase()`) lookup,
     dropping the metadata instead of applying it.
   - *Data rows*: Python's `stripped.split(",")` only requires `len(parts) >=
     2` (extra trailing columns beyond frequency/gain are silently ignored).
     Rust's `t.split_once(',')` puts everything after the first comma into the
     gain field, so a `>2`-column row (e.g. `"20,1.0,extra"`) fails to parse
     as `f64` and returns `DspError::Parse` instead of ignoring the extra
     column. Neither direction is exercised by the current target-curve
     corpus (no double-`#` headers, no `>2`-column rows).
8. **`autofit.rs` degenerate all-out-of-band / all-zero-audible-residual
   argmax case is stricter (safer) than the oracle.** Python's
   `masked_residual = np.where(audible, np.abs(residual), 0.0)` followed by
   `np.argmax(masked_residual)` returns the *first* index of the maximum —
   which, when every audible-band residual is exactly zero (or there are no
   audible-band bins at all), is index 0 of the *unmasked* array. Since
   `peak_gain = residual[peak_idx]` then reads the raw (not masked) residual,
   Python can place a peaking band centered on an out-of-band bin whenever
   that bin happens to carry a non-zero raw residual. Rust's `auto_fit_parametric_eq`
   only ever updates `peak_idx`/`peak_abs` inside the `mask[i]` branch, so it
   either selects an in-band bin or (if no bin is in-band) leaves `peak_idx ==
   None` and breaks cleanly — it can never select an out-of-band bin.
   Unreachable on realistic measured curves (which always have non-zero
   audible-band residual until convergence); no fixture exercises it.
9. **FIR minimum-phase Nyquist-bin window weight follows scipy 1.18.0
   semantics.** `fixtures/` was generated with scipy 1.18.0 (see
   `fixtures/manifest.json`); `minimum_phase_homomorphic`'s Nyquist-bin
   cepstrum window weight (`1 + n_fft % 2`) is transcribed from that version's
   `scipy.signal._fir_filter_design.minimum_phase`. Not a behavioral
   divergence today (n_fft is always even here, so the weight is always 1),
   but noted because this is the one spot in `fir.rs` pinned to a specific
   scipy release rather than to stable, version-independent numpy semantics —
   a future scipy upgrade that changes this window would need re-verification
   against regenerated fixtures.
10. **`autofit` NaN sanitation is NaN-only.** Python's `np.nan_to_num(...,
    nan=0.0)` also maps ±inf to ±MAX_FLOAT; Rust zeroes NaN and passes ±inf
    through. Unreachable for designed peaking filters (the response floor
    prevents −inf).
11. **`fir` linear-phase peak-normalize is guarded.** Python divides by
    `max(|ir|)` unguarded (all-zero prototype → NaN array); Rust skips the
    division when the peak is 0, returning zeros.
12. **`parse_target_csv` validates at parse time.** The Python loader accepts
    single-row / non-monotonic CSVs and only fails later inside
    scipy CubicSpline; Rust rejects them at parse with `DspError::Parse`.
    Same inputs fail in both worlds — different layer and mechanism.
13. **`fr::compute_frequency_response` and `fr::average_measurements` return
    `Result`.** For `compute_frequency_response` the Python equivalent
    raises (ValueError / IndexError) on n_fft=0 or empty input — same
    inputs fail in both worlds, different mechanism. For
    `average_measurements([])` this is a **real behavioral divergence**:
    the oracle does NOT raise — `np.mean([], axis=0)` returns NaN (with a
    RuntimeWarning) — whereas Rust returns `DspError::InvalidInput` instead
    of yielding NaN. Ragged measurement lists raise in numpy (shape error)
    and error in Rust.
14. **`ParametricEQ::preamp_db` is clamped at ≤ 0; AutoEQ's is signed.**
    New in Rust (2026-07-15 engine-hardening spec, R1-1) — the prototype
    computes no preamp at all (`export_autoeq_format` hardcodes
    `"Preamp: 0.0 dB"`). The convention it implements is AutoEQ's
    `ParametricEQ.txt` preamp, exactly `−compound.max_gain`
    (frequency_response.py:211, no headroom constant — `PREAMP_HEADROOM`
    applies only to the GraphicEQ string and the FIR impulse responses),
    with one deliberate divergence: AutoEQ's `−max_gain` is signed, so a
    pure-cut EQ gets a *positive* preamp boosting the signal back to
    unity; ParaEQ returns `−max(0, peak)`, so a pure-cut EQ gets exactly
    `0.0`. Auto-boosting a source that may already sit at 0 dBFS to
    recover headroom we did not spend is a clipping risk taken for
    nothing. Pinned by `test_peq.rs::preamp_pure_cut_is_exactly_zero`.
15. **`ParametricEQ::export_autoeq_format_with_preamp` has no oracle
    counterpart.** New in Rust (Stage 5; 2026-07-15 decision-engine spec
    § Preamp requires "(a) `export_autoeq_format` emits the computed
    preamp"). Implemented *additively* rather than by changing
    `export_autoeq_format`, because that function is pinned byte-for-byte
    against `fixtures/peq/two_band`'s `autoeq_export` scalar — a Tier-1
    frozen fixture the rescope plan forbids regenerating. So
    `export_autoeq_format` keeps the oracle's `"Preamp: 0.0 dB"` literal
    (parity, frozen) and the new function carries `preamp_db()`. One
    display divergence from a bare `{:.1}` format: a preamp in
    `(-0.05, 0]` prints as `0.0`, not `-0.0`. Pinned by
    `test_peq.rs::the_two_exports_differ_only_in_the_preamp_line` and
    `::a_pure_cut_export_never_prints_negative_zero`.

16. **`ParametricEQ::export_autoeq_format_with_preamp_db` has no oracle
    counterpart either.** The prototype exporter hardcodes `Preamp: 0.0 dB`
    (parametric_eq.py:92-106) and never round-trips a preamp; the desktop app
    must export a real preamp rather than a literal zero. (When this entry was
    written that number was the preamp the user set and the engine was running
    at, via `SetGainDb`. R1-1 changed WHICH number: `eq_export_autoeq` now calls
    the sibling `export_autoeq_format_with_preamp`, the cascade-derived value,
    per engine-hardening `:117`. The divergence from the oracle is unchanged;
    only the source of the number moved.) Deliberate UX fix, owner-approved
    2026-07-13 — originally
    landed on `feature/rust-port-tauri-shell` by giving `export_autoeq_format`
    itself a `preamp_db: f64` parameter, which the 2026-09-16 integration merge
    reworked into this third, additive method so the oracle-parity export stays
    argument-free and fixture-pinned (see #15). All three share the `-0.0`
    guard, and that guard is bounded at both ends (`(-0.05, 0]`): the *only*
    divergence from the shell branch is that a preamp in that window writes
    `Preamp: 0.0 dB` where the shell branch wrote `Preamp: -0.0 dB` — the same
    number, spelled without the negative zero, and the parser accepts either
    spelling. Every other value is written verbatim, including the whole
    positive half of the desktop's accepted range (`eq.rs`:
    `PREAMP_MIN_DB..=PREAMP_MAX_DB`, -30..=+10), so a FORMAT-level export/parse
    round trip is lossless in both signs. (Format-level: the text a caller hands
    this function parses back to the same number. An APP-level export/import
    round trip is not level-neutral — see #19.) No production call site since
    R1-1 repointed `eq_export_autoeq`; pinned by
    `test_peq.rs::export_with_preamp_db_writes_the_caller_s_preamp` (which
    covers -30, -6.5, -0.04, +0.1, +3.5, +6 and the +10 endpoint) and
    `test_autoeq_parse.rs::export_then_parse_roundtrip`.

17. **AutoEq INDEX.md hrefs are percent-DECODED on parse** (`desktop/src-tauri/src/autoeq.rs::parse_index`).
    The oracle (`autoeq_db.py`) stores the raw href verbatim and then
    `urllib.parse.quote`s it again at fetch time — so a real href like
    `Sennheiser%20HD%20650` becomes `Sennheiser%2520HD%2520650` and 404s every
    preset. The oracle's tests never caught it (network is mocked). The Rust
    port decodes the href on parse so `IndexEntry::path` is the literal path and
    the fetch layer (`encode_path`) encodes exactly once. Deliberate bug fix;
    the surfaced fallback URL (`raw.githubusercontent.com/...`) is what read as a
    "git error" to the owner. Regression test:
    `preset_url_from_encoded_index_href_is_single_encoded`.

18. **A band that becomes illegal at a new sample rate is dropped, not fatal
    to the whole correction.** Not an oracle divergence — the prototype has no
    rate-change path at all — but a deliberate, user-visible divergence from
    the behaviour the `feature/rust-port-tauri-shell` branch shipped, recorded
    here for the same reason as #16 and #17. On that branch the desktop
    forwarder (`eq::resend_decision`, called from `engine_bridge.rs`)
    re-validates the **whole** band set at the new rate and issues
    `ClearCorrection` if any single band fails, so an AirPods 48 → 44.1 kHz
    handoff carrying one band above the new 22.05 kHz Nyquist removes the
    user's **entire** EQ. Engine-hardening R1-6 moves that check into
    `paraeq-engine`'s `build_correction`, which instead **drops only the
    offending bands and counts them** in `BuildReport.bands_dropped`, refusing
    the whole configuration only when nothing survives. The count is logged at
    `warn`; it is **not** on the wire — `EngineState` has no drop or
    substitution count, so the Advanced drawer's surfacing of it is not built
    (a follow-up alongside the drawer itself, Stage 7). The
    rationale is R1-3's own DECIDED text for the identical question one layer
    up: *"the auto front-end must never be bricked by one bad band… An error
    would mean no correction at all from one bad row."* Flagged `OPEN [OWNER]`
    — a reversal is one branch in one function. See
    `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` §D-10.

19. **An imported `Preamp:` line now composes with the engine's own
    auto-preamp.** Not an oracle divergence — recorded here for the same
    reason as #16 and #18, because it is user-visible and it changed in Phase
    A. `eq_import_autoeq` (and the in-app AutoEq browser,
    `AutoEqBrowser.tsx`) routes a parsed `Preamp:` line to the user's trim:
    `EngineCommand::SetGainDb`, which rides `gain_bits` and applies on BOTH
    chain paths. That wiring is owner-approved (2026-07-13; the prototype
    silently dropped it) and unchanged. What changed is the other side: R1-1
    gave the engine its own `Correction.preamp_lin`, derived from the very
    bands the import just applied, on the corrected path. AutoEq's
    `ParametricEq.txt` convention makes the file's preamp exactly
    `−max_gain` of its own bands (engine-hardening `:81`; `peq.rs`'s
    `preamp_db` doc says the same), i.e. numerically the quantity ParaEQ now
    derives — so importing a boosting preset attenuates roughly twice.
    Measured on a real-shaped preset: file `−6.8 dB` + engine `−6.78 dB`
    = `−13.58 dB` on the corrected path, and `−6.8 dB` on the bypassed one
    with no boost to compensate. ParaEQ's own export re-imported doubles by
    construction, since R1-1 made the export write the cascade-derived
    number. It fails QUIET, never loud, and the trim is visible and editable
    in one field. Flagged **`OPEN [OWNER]`**: whether a file's `Preamp:` line
    is a user trim or a headroom number the engine should re-derive is a
    product call, and changing it means changing `EqState.preamp_db`
    semantics and its persistence — both explicitly frozen for Phase A. See
    `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` §D-24.

20. **A coefficient swap that WEAKENS the auto-preamp restarts the filter
    state instead of transplanting it.** Not an oracle divergence -- the
    prototype has no realtime swap path at all -- but recorded here for the
    same reason as #16, #18 and #19: it is user-visible and it changed in
    Phase A. R1-7a's `Correction::adopt_state_from` preserves DF2T delay
    state across a coefficient swap so a band edit does not click. R1-1 then
    put `preamp_lin` inside `Correction`, and the delay lines hold the
    OUTGOING cascade's un-preamped energy while the INCOMING `preamp_lin`
    multiplies that tail on the first block -- so a swap into LESS headroom
    has no room for it and the last-resort +-1.0 clamp engages hard.
    Measured: a +12 dB band dragged flat under a full-scale 1 kHz sine, 68
    clipped samples at a pre-clamp peak of 2.64 on the block after the swap,
    which is precisely what engine-hardening's R1-8 *UI contract* paragraph
    calls R1-1's falsifier -- and the EQ tab reaches it ~10 times a second
    while a handle is being dragged. The transplant is therefore SKIPPED in
    that direction (`crates/paraeq-engine/src/chain.rs`,
    `if self.preamp_lin > old.preamp_lin { return; }`) and the incoming
    correction starts from clean state, bounded by its own preamp.

    **The cost:** R1-7a's "no step" bound no longer holds across a
    preamp-weakening swap, so dragging a boost DOWN restarts that band's
    ring-up -- the click R1-7a is release-blocking for, in that one
    direction. The other direction is untouched, and a cut-only set never
    reaches the condition (`preamp_lin == 1.0` on both sides). Scaling the
    adopted state by `old.preamp_lin / self.preamp_lin` was measured as the
    alternative and only halves the overshoot (2.64 -> 1.73): R1-1 leaves
    ZERO design margin by construction, so the corrected path sits at
    exactly 1.0 at the peak and any transient clips. Pinned in both
    directions by `crates/paraeq-engine/tests/test_chain.rs`
    (`a_swap_to_a_weaker_preamp_starts_from_clean_state`) and
    `tests/test_meters.rs` (`a_swap_that_weakens_the_preamp_does_not_clip`).
    Flagged **`OPEN [OWNER]`**: the fix that would let BOTH properties hold
    is a headroom constant, and wizard-design's Open Question 5 records "no
    headroom constant" as DECIDED (2026-07-21) on the EXPORT convention, not
    on realtime swap margin -- so re-opening it is a product call, not
    something to introduce quietly. See
    `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` D-25.

(add entries here as they are discovered during implementation)
