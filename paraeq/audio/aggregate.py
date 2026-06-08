"""Programmatic macOS Aggregate Device creation for ParaEQ's real-time routing.

ParaEQ routes system audio through a single Aggregate Device used in duplex:
BlackHole (the loopback) listed FIRST so it occupies channels 1..N, and the
physical headphone/speaker output listed SECOND. macOS plays system audio to
the aggregate's first output channels (BlackHole), which loop back to
BlackHole's inputs; ParaEQ reads those, applies EQ, and writes to the
physical-output channels. See ``app/main_window.py`` (``start_audio_engine``)
and ``docs/CONTEXT.md`` for the channel-map side of this.

This module builds that aggregate via the Core Audio HAL
(``AudioHardwareCreateAggregateDevice``) so the user no longer has to assemble
it by hand in Audio MIDI Setup. It is macOS-only: ``CoreAudio``/``objc`` are
imported lazily so the rest of ``paraeq`` stays importable on other platforms.
The Rust port will call the identical Core Audio API with the identical keys —
this module doubles as executable documentation of that API.

Lifecycle (see ``setup_routing`` / ``teardown_routing``):
- launch: pick the physical output (current default, or — if we crashed and the
  default is a stale ParaEQ aggregate — a persisted hint), drop any leaked
  ParaEQ aggregate, create a fresh ``[BlackHole, physical]`` aggregate, set it
  as the system default, and re-init PortAudio so sounddevice sees it.
- quit: restore the physical output as default, then destroy the aggregate,
  leaving the system exactly as it was.
"""

import logging
import re
import struct
from dataclasses import dataclass

logger = logging.getLogger(__name__)

PARAEQ_AGGREGATE_NAME = "ParaEQ"
PARAEQ_AGGREGATE_UID = "com.paraeq.aggregate"


@dataclass
class CoreAudioDevice:
    id: int
    name: str
    uid: str
    is_output: bool = True


@dataclass
class RoutingState:
    """Everything needed to undo what ``setup_routing`` did."""

    aggregate_id: int
    aggregate_uid: str
    aggregate_index: int | None  # sounddevice index of the aggregate
    restore_output_uid: str | None  # physical output to restore on teardown
    restore_output_id: int | None


class AggregateError(RuntimeError):
    """A Core Audio call returned a non-zero OSStatus."""


# ---------------------------------------------------------------------------
# Lazy Core Audio access (macOS-only, optional dependency)
# ---------------------------------------------------------------------------

def _ca():
    import CoreAudio

    return CoreAudio


def _objc_null():
    import objc

    return objc.NULL


def _addr(selector, scope=None):
    ca = _ca()
    if scope is None:
        scope = ca.kAudioObjectPropertyScopeGlobal
    return ca.AudioObjectPropertyAddress(
        selector,
        scope,
        ca.kAudioObjectPropertyElementMain,
    )


def _check(status: int, what: str) -> None:
    if status != 0:
        raise AggregateError(f"{what} failed with OSStatus {status}")


def _cfstring_from_ptr(ptr_bytes) -> str | None:
    """Dereference a CFStringRef returned as raw pointer bytes into a str.

    Core Audio's CFString-valued properties come back through PyObjC as the
    raw 8-byte CFStringRef pointer rather than a bridged string, so we wrap
    the pointer as an objc object and stringify it. The property hands back a
    +1 retained copy; we intentionally leak that single retain (a handful of
    tiny strings per app launch, reclaimed at process exit) rather than risk
    an ownership double-free.
    """
    import objc

    ptr = struct.unpack("P", bytes(ptr_bytes))[0]
    if ptr == 0:
        return None
    return str(objc.objc_object(c_void_p=ptr))


def _get_cfstring(obj_id: int, selector) -> str | None:
    ca = _ca()
    status, _, data = ca.AudioObjectGetPropertyData(
        obj_id, _addr(selector), 0, _objc_null(), 8, None
    )
    if status != 0:
        return None
    return _cfstring_from_ptr(data)


# ---------------------------------------------------------------------------
# Raw Core Audio operations (mocked in tests)
# ---------------------------------------------------------------------------

def list_devices() -> list[CoreAudioDevice]:
    ca = _ca()
    addr = _addr(ca.kAudioHardwarePropertyDevices)
    status, size = ca.AudioObjectGetPropertyDataSize(
        ca.kAudioObjectSystemObject, addr, 0, _objc_null(), None
    )
    _check(status, "list devices (size)")
    count = size // 4
    status, _, data = ca.AudioObjectGetPropertyData(
        ca.kAudioObjectSystemObject, addr, 0, _objc_null(), size, None
    )
    _check(status, "list devices")
    ids = struct.unpack(f"{count}I", bytes(data))
    devices = [
        CoreAudioDevice(
            id=did,
            name=_get_cfstring(did, ca.kAudioObjectPropertyName) or "",
            uid=_get_cfstring(did, ca.kAudioDevicePropertyDeviceUID) or "",
            is_output=device_has_output(did),
        )
        for did in ids
    ]
    logger.debug("listed CoreAudio devices", extra={"count": len(devices)})
    return devices


def device_has_output(device_id: int) -> bool:
    """True if the device exposes output streams (filters out input-only mics)."""
    ca = _ca()
    addr = _addr(ca.kAudioDevicePropertyStreams, ca.kAudioObjectPropertyScopeOutput)
    try:
        status, size = ca.AudioObjectGetPropertyDataSize(device_id, addr, 0, _objc_null(), None)
        return status == 0 and size > 0
    except Exception:
        return False


def default_output() -> CoreAudioDevice | None:
    ca = _ca()
    status, _, data = ca.AudioObjectGetPropertyData(
        ca.kAudioObjectSystemObject,
        _addr(ca.kAudioHardwarePropertyDefaultOutputDevice),
        0,
        _objc_null(),
        4,
        None,
    )
    _check(status, "get default output")
    dev_id = struct.unpack("I", bytes(data))[0]
    return find_by_id(list_devices(), dev_id)


def set_default_output(device_id: int) -> None:
    ca = _ca()
    data = struct.pack("I", device_id)
    status = ca.AudioObjectSetPropertyData(
        ca.kAudioObjectSystemObject,
        _addr(ca.kAudioHardwarePropertyDefaultOutputDevice),
        0,
        _objc_null(),
        len(data),
        data,
    )
    _check(status, "set default output")
    logger.info("set default output device", extra={"device_id": device_id})


def create_aggregate(
    sub_uids: list[str],
    main_uid: str,
    name: str = PARAEQ_AGGREGATE_NAME,
    uid: str = PARAEQ_AGGREGATE_UID,
) -> int:
    """Create a (non-stacked) aggregate device. Sub-device order = channel order."""
    ca = _ca()

    def k(const):
        # The kAudio*Key constants arrive as bytes (b"name"); CFDictionary keys
        # must bridge from str, so decode them.
        return const.decode() if isinstance(const, bytes) else const

    description = {
        k(ca.kAudioAggregateDeviceNameKey): name,
        k(ca.kAudioAggregateDeviceUIDKey): uid,
        k(ca.kAudioAggregateDeviceSubDeviceListKey): [
            {k(ca.kAudioSubDeviceUIDKey): sub_uid} for sub_uid in sub_uids
        ],
        k(ca.kAudioAggregateDeviceMainSubDeviceKey): main_uid,
        k(ca.kAudioAggregateDeviceIsStackedKey): 0,  # true aggregate, not multi-output
    }
    status, device_id = ca.AudioHardwareCreateAggregateDevice(description, None)
    _check(status, "create aggregate")
    logger.info(
        "created aggregate device",
        extra={"device_id": device_id, "sub_uids": list(sub_uids), "main_uid": main_uid},
    )
    return device_id


def destroy_aggregate(device_id: int) -> None:
    ca = _ca()
    status = ca.AudioHardwareDestroyAggregateDevice(device_id)
    _check(status, "destroy aggregate")
    logger.info("destroyed aggregate device", extra={"device_id": device_id})


def reinit_portaudio() -> None:
    """Re-enumerate devices so a just-created/destroyed aggregate is visible.

    PortAudio caches the device list at initialisation, so a fresh aggregate is
    invisible until it is re-initialised — this is the "had to restart the app"
    gotcha, handled in-process.
    """
    import sounddevice as sd

    try:
        sd._terminate()
        sd._initialize()
    except Exception as exc:  # pragma: no cover - depends on live PortAudio
        logger.warning("PortAudio re-init failed: %s", exc)


def sd_index_for_name(name: str) -> int | None:
    import sounddevice as sd

    for idx, dev in enumerate(sd.query_devices()):
        if dev["name"] == name and dev["max_output_channels"] > 0:
            return idx
    return None


# ---------------------------------------------------------------------------
# Pure helpers
# ---------------------------------------------------------------------------

# The bare BlackHole loopback (e.g. "BlackHole 16ch"), NOT an aggregate that
# merely contains "blackhole" in its name (e.g. "blackholeAgg"). Mirrors the
# filter in app/setup/setup_wizard.py.
_BLACKHOLE_LOOPBACK_RE = re.compile(r"^BlackHole\s*\d+ch$", re.IGNORECASE)


def find_blackhole(devices: list[CoreAudioDevice]) -> CoreAudioDevice | None:
    for dev in devices:
        if _BLACKHOLE_LOOPBACK_RE.match(dev.name.strip()):
            return dev
    return None


def _is_usable_physical(dev: CoreAudioDevice | None, blackhole: CoreAudioDevice) -> bool:
    """A device is a usable physical output if it is real (not BlackHole, not
    an aggregate) — using BlackHole here would build a silent aggregate."""
    if dev is None:
        return False
    if dev.id == blackhole.id or dev.uid == PARAEQ_AGGREGATE_UID:
        return False
    if not dev.is_output:
        return False
    return "blackhole" not in dev.name.lower()


def find_by_uid(devices: list[CoreAudioDevice], uid: str | None) -> CoreAudioDevice | None:
    if not uid:
        return None
    for dev in devices:
        if dev.uid == uid:
            return dev
    return None


def find_by_id(devices: list[CoreAudioDevice], device_id: int) -> CoreAudioDevice | None:
    for dev in devices:
        if dev.id == device_id:
            return dev
    return None


def _pick_fallback_output(devices: list[CoreAudioDevice]) -> CoreAudioDevice | None:
    """A real physical output to fall back on (skip inputs, BlackHole, aggregates)."""
    for dev in devices:
        if not dev.is_output:
            continue
        low = dev.name.lower()
        if dev.uid == PARAEQ_AGGREGATE_UID:
            continue
        if "blackhole" in low or "aggregate" in low or low.endswith("agg"):
            continue
        return dev
    return None


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------

def _rollback(aggregate_id: int, physical: CoreAudioDevice | None) -> None:
    """Undo a partially-applied setup so the Mac is never left silent.

    Restore the physical device as the system default, then destroy the
    half-created aggregate. The two steps are independent so one failing does
    not skip the other.
    """
    if physical is not None:
        try:
            set_default_output(physical.id)
        except Exception as exc:
            logger.error("rollback: restore default output failed: %s", exc)
    try:
        destroy_aggregate(aggregate_id)
        reinit_portaudio()
    except Exception as exc:
        logger.error("rollback: destroy aggregate failed: %s", exc)


def setup_routing(previous_physical_uid: str | None = None) -> RoutingState | None:
    """Create the ParaEQ aggregate and make it the system default output.

    Returns the :class:`RoutingState` needed to undo this on quit, or ``None``
    if routing could not be established (no BlackHole, no usable physical
    output, the aggregate not visible to sounddevice, or any Core Audio
    failure). On ``None`` the caller falls back to manual device choice, and
    the system default is guaranteed to be left on a real output (never an
    orphaned/silent aggregate).
    """
    try:
        devices = list_devices()
        blackhole = find_blackhole(devices)
        if blackhole is None:
            logger.warning("aggregate setup skipped: BlackHole not found")
            return None

        current = default_output()
        if current is not None and current.uid == PARAEQ_AGGREGATE_UID:
            # We crashed last run; the default is a stale ParaEQ aggregate.
            physical = find_by_uid(devices, previous_physical_uid)
        else:
            physical = current
        # Never aggregate BlackHole with itself — fall back to a real output.
        if not _is_usable_physical(physical, blackhole):
            physical = _pick_fallback_output(devices)
        if not _is_usable_physical(physical, blackhole):
            logger.warning("aggregate setup skipped: no usable physical output found")
            return None

        # Idempotency / crash cleanup: drop any pre-existing ParaEQ aggregate.
        for dev in devices:
            if dev.uid == PARAEQ_AGGREGATE_UID:
                destroy_aggregate(dev.id)

        new_id = create_aggregate([blackhole.uid, physical.uid], blackhole.uid)
        # Past this point the aggregate exists and becomes the system default;
        # any failure must roll back or we leave the Mac silent.
        try:
            set_default_output(new_id)
            reinit_portaudio()
            index = sd_index_for_name(PARAEQ_AGGREGATE_NAME)
            if index is None:
                raise AggregateError(
                    "aggregate not visible to sounddevice after PortAudio re-init"
                )
        except Exception:
            _rollback(new_id, physical)
            raise

        logger.info(
            "aggregate routing ready",
            extra={
                "aggregate_id": new_id,
                "aggregate_index": index,
                "blackhole_uid": blackhole.uid,
                "physical_uid": physical.uid,
            },
        )
        return RoutingState(
            aggregate_id=new_id,
            aggregate_uid=PARAEQ_AGGREGATE_UID,
            aggregate_index=index,
            restore_output_uid=physical.uid,
            restore_output_id=physical.id,
        )
    except Exception as exc:
        logger.error("aggregate setup failed: %s", exc)
        return None


def teardown_routing(state: RoutingState | None) -> None:
    """Restore the physical output as default, then destroy the aggregate.

    The restore and destroy are independent: a failure to restore the output
    must not prevent the aggregate from being removed (and vice versa).
    """
    if state is None:
        return
    if state.restore_output_uid:
        try:
            # Re-resolve by UID — device ids can shift after create/destroy.
            dev = find_by_uid(list_devices(), state.restore_output_uid)
            if dev is not None:
                set_default_output(dev.id)
        except Exception as exc:
            logger.error("teardown: restore default output failed: %s", exc)
    try:
        destroy_aggregate(state.aggregate_id)
        reinit_portaudio()
    except Exception as exc:
        logger.error("teardown: destroy aggregate failed: %s", exc)
