"""Gates on the fixture generator itself, not on any fixture it writes.

Three properties, all from the rescope plan's fixture convention and
docs/specs/2026-07-15-measurement-suite-design.md ("Room behaviour arrives as
new functions, never modified ones."):

1. Regenerating never removes `fixtures/decide/`. Those bundles are the one
   kind of fixture `generate_fixtures.py` neither writes nor owns -- the owner
   reviews each once and freezes it -- yet the sanctioned command in CLAUDE.md
   used to `shutil.rmtree()` the whole `fixtures/` tree.
2. Regenerating is ADDITIVE ONLY. The generator's docstring promise is
   "rerunning must be byte-identical", read in practice as "`git status` stays
   clean". Adding a gen_*() case breaks that reading on purpose: the promise
   was never "no diff", it was "no UNEXPLAINED diff". Explained means new files
   under a stage directory; unexplained means a changed byte in an existing
   fixture, a deletion, or a file appearing somewhere no gen_*() writes.
3. Regenerating twice in a row is a no-op. This is the half of the promise that
   still means byte-identical, and it is what proves the seeded RNGs and the
   absence of clocks actually hold.

Every test runs against a `tmp_path` COPY of `fixtures/` with the generator's
`OUT` redirected at it, so a buggy test can never delete a real fixture and a
developer mid-change never gets a spurious red from their own dirty tree.
"""
import importlib.util
import json
import platform
import shutil
import sys
from pathlib import Path

import numpy as np
import pytest
import scipy

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "fixtures"
GENERATOR = ROOT / "prototype" / "tools" / "generate_fixtures.py"


def _load_generator():
    """Import prototype/tools/generate_fixtures.py, which is a script, not a package."""
    spec = importlib.util.spec_from_file_location("generate_fixtures", GENERATOR)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


gen = _load_generator()

# Stage directories a pending case addition is allowed to CREATE. An item that
# introduces a brand-new stage name adds it here in the same commit -- that edit
# is the deliberate decision the gate is asking for, and it is what catches a
# mis-named stage (e.g. save_case("FR", ...) quietly minting fixtures/FR/).
#
# "resample" is gen_resample_poly()'s stage (Stage 6, B12): the one Tier-2 case
# for crates/paraeq-dsp/src/resample.rs, and the first case to write it.
NEW_STAGES: frozenset[str] = frozenset({"resample"})

# The interpreter is part of the toolchain even though PINNED_VERSIONS does not
# name it: main() stamps platform.python_version() into fixtures/manifest.json,
# so on a different patch release a regen rewrites the manifest and
# `regeneration_is_additive_only` fails for a reason that has nothing to do
# with fixtures. Read the expected version from the COMMITTED manifest rather
# than spelling it a fourth time, so the three stamps have one source.
#
# NOTE for whoever revisits this: adding "python" to PINNED_VERSIONS and having
# check_pinned_versions() enforce it would be the cleaner fix, but it would
# also refuse `python prototype/tools/generate_fixtures.py` on every developer
# machine running a different patch release. That is a policy change the owner
# has not made, so it is deliberately not made here.
_MANIFEST = json.loads((FIXTURES / "manifest.json").read_text())
_TOOLCHAIN_MATCHES = (
    np.__version__ == gen.PINNED_VERSIONS["numpy"]
    and scipy.__version__ == gen.PINNED_VERSIONS["scipy"]
    and platform.python_version() == _MANIFEST["python"]
)
_SKIP_REASON = (
    "needs the pinned fixture toolchain "
    f"(numpy=={gen.PINNED_VERSIONS['numpy']}, scipy=={gen.PINNED_VERSIONS['scipy']}, "
    f"python {_MANIFEST['python']} per fixtures/manifest.json); running "
    f"numpy=={np.__version__}, scipy=={scipy.__version__}, python "
    f"{platform.python_version()}. Install with: "
    "pip install -e './prototype[dev,fixtures]'"
)
requires_pinned_toolchain = pytest.mark.skipif(not _TOOLCHAIN_MATCHES, reason=_SKIP_REASON)


def _redirect_out(monkeypatch, tmp_path):
    """Point the generator at a throwaway copy of the committed fixtures tree."""
    out = tmp_path / "fixtures"
    shutil.copytree(FIXTURES, out)
    monkeypatch.setattr(gen, "OUT", out)
    # main() calls parse_args(), which would otherwise read pytest's own argv.
    monkeypatch.setattr(sys, "argv", ["generate_fixtures.py"])
    return out


def _plant_decide_bundle(out):
    """Stand in for an owner-frozen decision bundle.

    The committed tree has no fixtures/decide/ yet -- the bundles arrive with
    the decision engine -- so the protection is tested against a planted one.
    Same shape as the real thing: <case>/{bundle,expected}.json.
    """
    bundle = out / "decide" / "flat_headphone"
    bundle.mkdir(parents=True)
    (bundle / "bundle.json").write_text('{"owner": "frozen"}\n')
    (bundle / "expected.json").write_text('{"verdict": "frozen"}\n')
    return bundle


def _snapshot(out):
    """Every file under `out` as {relative path: bytes} -- the tmp-copy stand-in
    for `git status --porcelain fixtures/`."""
    return {
        str(path.relative_to(out)): path.read_bytes()
        for path in sorted(out.rglob("*"))
        if path.is_file()
    }


def test_regeneration_does_not_delete_the_decide_bundles(monkeypatch, tmp_path):
    """The wipe step clears what the generator owns and nothing else."""
    out = _redirect_out(monkeypatch, tmp_path)
    bundle = _plant_decide_bundle(out)

    gen.wipe_generated_fixtures()

    assert (bundle / "bundle.json").read_text() == '{"owner": "frozen"}\n'
    assert (bundle / "expected.json").read_text() == '{"verdict": "frozen"}\n'
    # ...and the wipe is still a wipe, or this test would pass on a no-op.
    assert not (out / "fr").exists()
    assert not (out / "manifest.json").exists()
    assert sorted(child.name for child in out.iterdir()) == ["decide"]


def test_save_case_refuses_to_write_into_decide(monkeypatch, tmp_path):
    """No gen_*() can reach the decide bundles even by naming the stage."""
    out = _redirect_out(monkeypatch, tmp_path)

    with pytest.raises(AssertionError):
        gen.save_case("decide", "smuggled", {}, {"values": np.zeros(4)})

    assert not (out / "decide").exists()


@requires_pinned_toolchain
def test_regeneration_is_additive_only(monkeypatch, tmp_path):
    """No existing fixture byte changes, nothing is deleted, and anything new
    lands under a stage directory a gen_*() actually writes."""
    out = _redirect_out(monkeypatch, tmp_path)
    _plant_decide_bundle(out)
    before = _snapshot(out)

    gen.main()

    after = _snapshot(out)
    deleted = sorted(set(before) - set(after))
    assert deleted == [], f"regeneration deleted fixtures: {deleted}"
    modified = sorted(rel for rel in set(before) & set(after) if before[rel] != after[rel])
    assert modified == [], f"regeneration rewrote existing fixtures: {modified}"
    assert after["manifest.json"] == before["manifest.json"]

    for rel in sorted(set(after) - set(before)):
        parts = Path(rel).parts
        assert len(parts) > 1, f"new fixture at the tree root, not under a stage: {rel}"
        stage = parts[0]
        assert stage not in gen.PRESERVED, f"a gen_*() wrote into {stage}/: {rel}"
        assert (FIXTURES / stage).is_dir() or stage in NEW_STAGES, (
            f"new fixture under an unannounced stage {stage}/: {rel} -- add the "
            "stage to NEW_STAGES in the same commit as the case, or fix its name"
        )


@requires_pinned_toolchain
def test_regeneration_is_byte_identical_on_a_second_run(monkeypatch, tmp_path):
    """Seeded RNGs, no clocks: the second run leaves the tree untouched."""
    out = _redirect_out(monkeypatch, tmp_path)
    _plant_decide_bundle(out)
    gen.main()
    first = _snapshot(out)

    gen.main()

    second = _snapshot(out)
    assert sorted(second) == sorted(first)
    differing = sorted(rel for rel in first if first[rel] != second[rel])
    assert differing == [], f"regeneration is not deterministic: {differing}"
