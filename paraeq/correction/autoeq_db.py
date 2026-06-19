"""AutoEq database access: index/preset parsing and a caching HTTP client.

GUI-free. Networking goes through the module-level ``_http_get`` so tests can
patch it. Data source: github.com/jaakkopasanen/AutoEq (raw.githubusercontent.com).
"""

import logging
import os
import re
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path

from platformdirs import user_data_dir

logger = logging.getLogger(__name__)

_INDEX_LINE = re.compile(
    r"^\s*-\s*\[(?P<name>.+?)\]\((?P<path>.+?)\)\s+by\s+(?P<source>.+?)\s+on\s+(?P<rig>.+?)\s*$"
)

_PREAMP_RE = re.compile(r"Preamp\s*:\s*([-\d.]+)\s*dB", re.IGNORECASE)
_FILTER_RE = re.compile(
    r"Filter\s+\d+\s*:\s*ON\s+(\w+)\s+Fc\s+([\d.]+)\s+Hz\s+Gain\s+([-\d.]+)\s+dB\s+Q\s+([\d.]+)",
    re.IGNORECASE,
)
# AutoEq short codes → internal filter types. Accepts both EqualizerAPO shelf
# spellings (LSC/HSC) and the legacy LS/HS aliases.
_AUTOEQ_TO_INTERNAL = {
    "HS": "high_shelf",
    "HSC": "high_shelf",
    "LS": "low_shelf",
    "LSC": "low_shelf",
    "NO": "notch",
    "PK": "peaking",
}


@dataclass
class AutoEQEntry:
    name: str
    source: str
    rig: str
    rel_path: str


@dataclass
class ParsedPreset:
    preamp_db: float
    bands: list  # list[EQBand]


def parse_index(markdown: str) -> list[AutoEQEntry]:
    """Parse an AutoEq ``results/INDEX.md`` into a list of entries.

    Each recognised line looks like
    ``- [Model](./source/rig/Model) by source on rig``. Lines that do not
    match (headings, prose, blanks) are skipped. The leading ``./`` is
    stripped from the relative path.
    """
    entries: list[AutoEQEntry] = []
    for line in markdown.splitlines():
        m = _INDEX_LINE.match(line)
        if not m:
            continue
        path = m.group("path").strip()
        if path.startswith("./"):
            path = path[2:]
        entries.append(
            AutoEQEntry(
                name=m.group("name").strip(),
                source=m.group("source").strip(),
                rig=m.group("rig").strip(),
                rel_path=path,
            )
        )
    logger.info("Parsed %d AutoEq index entries", len(entries))
    return entries


def parse_parametric_eq(text: str) -> ParsedPreset:
    """Parse AutoEq ParametricEq.txt text into a :class:`ParsedPreset`.

    Captures the ``Preamp`` line (defaulting to 0.0 dB if absent) and every
    ``Filter N: ON <TYPE> Fc <hz> Hz Gain <db> dB Q <q>`` line. Unknown filter
    codes fall back to peaking.
    """
    from paraeq.correction.parametric_eq import EQBand

    preamp_db = 0.0
    bands: list[EQBand] = []
    for line in text.splitlines():
        pm = _PREAMP_RE.search(line)
        if pm:
            preamp_db = float(pm.group(1))
            continue
        fm = _FILTER_RE.search(line)
        if fm:
            short, fc, gain, q = fm.group(1), fm.group(2), fm.group(3), fm.group(4)
            bands.append(
                EQBand(
                    filter_type=_AUTOEQ_TO_INTERNAL.get(short.upper(), "peaking"),
                    fc=float(fc),
                    gain_db=float(gain),
                    q=float(q),
                )
            )
    logger.info("Parsed AutoEq preset: preamp=%.1f dB, %d bands", preamp_db, len(bands))
    return ParsedPreset(preamp_db=preamp_db, bands=bands)


_RAW_BASE = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master"
_INDEX_REL = "results/INDEX.md"
_FILENAME_CASINGS = ("ParametricEq.txt", "ParametricEQ.txt")


def _http_get(url: str) -> str:
    """GET a UTF-8 text resource. Patched in tests."""
    with urllib.request.urlopen(url, timeout=30) as resp:
        return resp.read().decode("utf-8")


def _default_cache_dir() -> Path:
    return Path(user_data_dir("ParaEQ")) / "autoeq_cache"


def _atomic_write_text(path: Path, text: str) -> None:
    """Write text via a temp file + os.replace so an interrupted write never
    leaves a truncated file that a later run would serve as a valid cache hit."""
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(text, encoding="utf-8")
    os.replace(tmp, path)


class AutoEQClient:
    """Cache-first access to AutoEq's index and per-model presets.

    The index (``results/INDEX.md``) is fetched once and cached; individual
    presets are fetched lazily on first access and cached thereafter. All
    network I/O is synchronous — callers in the GUI run it on a worker thread.
    """

    def __init__(self, cache_dir=None):
        self.cache_dir = Path(cache_dir) if cache_dir is not None else _default_cache_dir()

    def fetch_index(self, *, force: bool = False) -> list[AutoEQEntry]:
        cache_file = self.cache_dir / "INDEX.md"
        if cache_file.exists() and not force:
            text = cache_file.read_text(encoding="utf-8")
            logger.debug("AutoEq index served from cache: %s", cache_file)
        else:
            text = _http_get(f"{_RAW_BASE}/{urllib.parse.quote(_INDEX_REL)}")
            self.cache_dir.mkdir(parents=True, exist_ok=True)
            _atomic_write_text(cache_file, text)
            logger.info("AutoEq index downloaded and cached: %s", cache_file)
        return parse_index(text)

    def fetch_preset(self, entry: AutoEQEntry, *, force: bool = False) -> ParsedPreset:
        cache_file = self.cache_dir / (entry.rel_path.replace("/", "__") + ".txt")
        if cache_file.exists() and not force:
            logger.debug("AutoEq preset served from cache: %s", cache_file)
            return parse_parametric_eq(cache_file.read_text(encoding="utf-8"))
        text = self._download_preset(entry)
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        _atomic_write_text(cache_file, text)
        logger.info("AutoEq preset downloaded and cached: %s", entry.name)
        return parse_parametric_eq(text)

    def _download_preset(self, entry: AutoEQEntry) -> str:
        model = entry.rel_path.rstrip("/").split("/")[-1]
        last_err: Exception | None = None
        for casing in _FILENAME_CASINGS:
            rel = f"results/{entry.rel_path}/{model} {casing}"
            url = f"{_RAW_BASE}/{urllib.parse.quote(rel)}"
            try:
                return _http_get(url)
            except Exception as exc:  # try the other casing before giving up
                last_err = exc
                logger.debug("AutoEq preset fetch failed for %s: %s", url, exc)
        raise last_err
