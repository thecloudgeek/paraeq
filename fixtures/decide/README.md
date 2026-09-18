# `fixtures/decide/` — the decision engine's characterization bundles

Eight measurement bundles and, once the owner has blessed them, the
`DecisionSet` each one produces. They are **not** oracle fixtures. Every other
directory under `fixtures/` holds a number some independent implementation says
is correct; these hold the number *this* implementation produced, after a human
read it and agreed it was the right answer.

The spec's own sentence, which is the whole contract:

> Cases: one per `TransducerClass`; a clean room; a room with a −30 dB null at
> one of five positions; a lost-seal coupler reseat; a cal file with the
> `7005770_90deg.txt` outlier; a clipped position; a noisy room at the SNR
> boundary; an EARS HEQ cal. These are **not** oracle fixtures — they are
> characterization fixtures whose expected values are reviewed by the owner once
> and then frozen. **A diff in `expected.json` is a policy change and must be
> argued for in the PR, which is the entire point.**

---

## The carve-out from the generator rule

CLAUDE.md says, and still says:

> **Fixtures are sacred**: `fixtures/` is generated ONLY by
> `prototype/tools/generate_fixtures.py` (deterministic, seeded). Never edit
> fixtures by hand; regenerate and commit script + output together.

**`fixtures/decide/` is the one sanctioned exception, and it is an exception to
the GENERATOR, not to the rule.** The Python oracle exists to produce parity
fixtures for the ten DSP modules ported from the prototype. There is no
prototype decision engine to port, and writing one for the purpose would not be
an independent oracle — it would launder a design bug into a golden fixture.
What the fixture policy is actually about is **provenance**, and provenance is
intact here: no byte under this directory is hand-authored. Every `bundle.json`
and every `ir/*.f64` sidecar is written by a seeded Rust generator
(`crates/paraeq-decide/tests/common/generator.rs`), which is re-runnable and
byte-identical:

```
cargo test -p paraeq-decide --test test_golden_bundles -- --ignored \
    regenerating_the_inputs_rewrites_every_bundle_and_sidecar
```

`git status` must be clean afterwards. Only `notes.md` and this README are
written by hand, because they are prose the owner signs rather than data — and
the freeze manifest digests them too, so an edit to either is visible.

The collision with the Python generator was real and is closed on the other
side as well: `generate_fixtures.py` used to `shutil.rmtree(OUT)`, so the
regeneration command CLAUDE.md sanctions **deleted every owner-frozen bundle in
this directory in passing**. It now wipes `fixtures/` child by child with
`decide` in a `PRESERVED` set, and `save_case` asserts that no `gen_*()` can
write here.

---

## Layout

```
fixtures/decide/
  README.md                    this file
  manifest.json                per-file digests + the freeze record
  <case>/
    bundle.json                a MeasurementBundle; IR samples referenced, not inlined
    expected.json              the DecisionSet decide() produced — written by the BLESS
    ir/pos<NN>_ch<N>.f64       one channel per file, little-endian raw f64
    ir/verify_ch<N>.f64        the verification pass, where a case has one
    notes.md                   what this case exercises and what the owner approved
```

### Why the IR samples are in sidecars

`ImpulseResponse::samples` is `Vec<Vec<f64>>` and serializes **inline**. Against
the shipped store window (`crates/paraeq-measure/src/store.rs`: 100 ms before
the direct arrival, 1500 ms after) a nine-position room bundle is roughly
25–30 MB of f64 JSON text, and eight of them would be ~200 MB. That is not
committable, not reviewable, and it would bury the `expected.json` the owner is
supposed to read. So each channel goes to a sidecar in **the repo's own existing
convention** — `generate_fixtures.py`'s `save_case` writes little-endian raw f64
and records `{file, len, shape}` in the JSON envelope, and the reader has existed
since Stage 1 at `crates/paraeq-dsp/tests/common/mod.rs`. `bundle.json`'s
`ir.samples` is an array of those envelopes, one per channel, and
`crates/paraeq-decide/tests/common/golden.rs` hydrates them back before
deserialization. `bundle.json` is then a few hundred lines that read like a case
description, which is the point.

**The alternative, flagged rather than taken:** f32 WAVs at
`IrStore::path_for`'s exact `pos<NN>_ch<N>.wav` naming, which would make "the
fixture **is** a saved profile / the bug-report attachment" literally true. It
is the better long-term shape and it costs two things: a `hound` dev-dependency
on a crate whose manifest lists three dependencies on purpose, and f32 storage,
which silently rounds every sample on the way in — so the byte-for-byte freeze
would be a freeze over rounded inputs. **OPEN \[OWNER\]**: say the word and the
sidecars become WAVs.

### Window sizes, and why they differ by path

| path | window | why |
|---|---|---|
| room | −100 ms / +1500 ms | the shipped store window. The `fdw_post_cycles` domain ceiling is DERIVED from it (30 cycles at 20 Hz), so a shorter room fixture would make the top of that domain unreachable in the one place it is exercised. |
| coupler | −100 ms / +250 ms | a coupler has no reflection problem to gate away (`GatingMode::None`), its energy is gone inside ~30 ms, and 250 ms still resolves to ~4 Hz. The analysis bounds the decided `right_window_ms` by the data the recording holds, so a shorter recording windows what is there rather than refusing. Storing the full 1.6 s would triple these sidecars to hold silence. |

Capture width also differs by path, and honestly: a room run captures through
**one** microphone with the stimulus on both output channels (`routing: Both`,
one correction applied as one), while an EARS jig captures **both ear capsules
at once** and the installed plan carries the matching two channels.

The whole directory is ~21 MB.

---

## What these eight cases do NOT cover

`DiagnosticCode` has 27 variants. The eight cases exercise a handful of them.
The rest — `AbsurdCurve`, `CalMalformed`, `CalMissing`, `ClippingSession`,
`CoherentAveragingRejected`, `ExcessiveVariance`, `LowSnrHard`,
`MicNotConnected`, `NoSignal`, `NoiseFloorTooHigh`, `OverrideOutOfDomain`,
`PositionOutlierCouplerHf`, `PositionOutlierRoom`, `SelfExclusionUnavailable`,
`SweepRateMismatch`, `TooFewPositions`, `TwoClock` (as a hard case),
`VerificationPreampMismatch`, `VerificationResidual`,
`VerificationRoutingMismatch` and `WrongTransducer` — are covered by **ordinary
in-crate unit tests over perturbed synthetic bundles**
(`crates/paraeq-decide/tests/common/mod.rs::synthetic_bundle`, one field
changed per test), **not** by growing this directory to twenty-five
directories.

This has to be written down or the promise dies quietly: "the owner reviews them
once" is only true while there is a set small enough to review. Twenty-five
directories is not that set, and a fixture nobody reads is a fixture that freezes
whatever it happened to contain.

The same reasoning applies to the two verification refusals that have no golden
case here — a `Both` routing over divergent per-channel band sets, and an
`installed_preamp_db` its own bands do not reproduce. Both are refusals, and
giving either one a golden case would mean either a ninth directory or a second
unrelated failure stacked into a case the owner already signed for something
else. They belong in unit tests.

---

## The freeze

Four mechanisms, all in `crates/paraeq-decide/tests/test_golden_bundles.rs`:

1. **`decide_reproduces_expected_json_byte_for_byte`** — the characterization
   assertion, compared as text. Byte-exactness is available because
   `serde_json`'s `float_roundtrip` feature is on workspace-wide, enabled
   specifically so this can be an `assert_eq!` rather than an epsilon walk.
2. **`expected_json_is_canonical`** — reparse and reserialize must reproduce the
   file bytes. Catches a hand-edit, a key reorder or a float-format drift that
   (1) tolerates whenever `decide()` happens to agree semantically.
3. **`the_freeze_manifest_matches_every_file_on_disk`** — the only thing
   protecting the **inputs**. (1) does not: a silently edited `bundle.json`
   would simply bless to a different `expected.json` and look legitimate.
4. **`blessing_is_off_unless_the_env_var_is_set`** — CI sets no such variable,
   so CI can never silently re-bless.

### `manifest.json`

```json
{
  "algorithm": "fnv1a64",
  "cases": { "<case>": { "<file>": { "bytes": 1234, "fnv1a64": "0x…" } } },
  "frozen_at": null,
  "reviewed_by": null,
  "schema_version": 1
}
```

`frozen_at` and `reviewed_by` are `null` until the bless fills them; they are the
owner's record, not the generator's.

The digest is **FNV-1a 64, inline in the test helper**, because the workspace has
no hashing crate and CLAUDE.md carries a forbidden-dependency list. It is a
drift detector against accidents — a hand-edit, a truncated sidecar, a stale
regeneration — and not a security boundary. **OPEN \[OWNER\]**: `sha2` is the
standard answer if a real digest is wanted; one dev-dependency, one line.

### JSON style

`serde_json::to_string_pretty` (two-space indent, struct field order) plus a
trailing newline. This deliberately differs from the Python fixtures'
`indent=1, sort_keys=True`: that is `generate_fixtures.py`'s style and these
files are not its output. The choice is pinned by (2) above so it cannot drift
into a re-format that reads as a policy diff.

---

## The bless

```
PARAEQ_BLESS_DECIDE=1 cargo test -p paraeq-decide --test test_golden_bundles
```

`PARAEQ_BLESS=1` is accepted as an alias. It rewrites all eight `expected.json`
files **and** `manifest.json` in one pass, so the PR shows the policy change and
the re-freeze together rather than as two commits a reviewer has to correlate.
A variable that is set but empty, or set to `0`, is off.

To prove the mechanism without writing into the repo — a dry run:

```
PARAEQ_BLESS_DECIDE_OUT=/some/scratch/dir \
    cargo test -p paraeq-decide --test test_golden_bundles
```

That writes `<dir>/<case>/expected.json` and touches nothing here.

**The bless is owner-gated and runs LAST.** `expected.json` is a photograph of
every `decide()` default, and each unresolved default re-blesses all eight
files. A freeze taken over values the owner has not ruled on is the exact
failure the mechanism exists to prevent, and a freeze that re-blesses twice in
its first week teaches everyone to rubber-stamp the diff — which is precisely
what the spec is trying to stop. The ordering that falls out: **author the
inputs and the tests first, let them fail, implement the rules, and bless
once.** Inputs do not move when policy moves; `expected.json` does.

Until the bless has run, the two assertions that read `expected.json` report
that they skipped and pass, and the workspace stays green.

---

## Reviewing a case

Read its `notes.md` first. It says what the case exercises, what verdict it
should reach and which diagnostics it should raise, and it was written **before**
the bless — so the review is "does `expected.json` agree with this paragraph?"
rather than "do these nine hundred floats look right?". That is the whole
difference between a freeze that means something and a signature over a blob.
