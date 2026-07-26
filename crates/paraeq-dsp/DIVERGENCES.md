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

(add entries here as they are discovered during implementation)
