"""Tests for the macOS aggregate-device orchestration.

The raw Core Audio wrappers are mocked (CoreAudio is macOS-only and would
touch real system audio); these tests pin the orchestration logic —
sub-device ordering, crash recovery, idempotency, and restore-on-quit.
"""

from unittest.mock import MagicMock, patch

from paraeq.audio import aggregate
from paraeq.audio.aggregate import (
    PARAEQ_AGGREGATE_UID,
    CoreAudioDevice,
    RoutingState,
)

BLACKHOLE = CoreAudioDevice(id=59, name="BlackHole 16ch", uid="BlackHole16ch_UID")
SPEAKERS = CoreAudioDevice(id=90, name="MacBook Pro Speakers", uid="BuiltInSpeakerDevice")
HEADPHONES = CoreAudioDevice(id=91, name="External Headphones", uid="ExtHeadphones_UID")
STALE_AGG = CoreAudioDevice(id=103, name="ParaEQ", uid=PARAEQ_AGGREGATE_UID)
BLACKHOLE_AGG = CoreAudioDevice(id=104, name="blackholeAgg", uid="~:AMS2_Aggregate:1")
MIC = CoreAudioDevice(id=97, name="MacBook Pro Microphone", uid="BuiltInMicrophoneDevice", is_output=False)


# --- pure helpers ---------------------------------------------------------

def test_find_blackhole():
    assert aggregate.find_blackhole([SPEAKERS, BLACKHOLE]) is BLACKHOLE


def test_find_blackhole_none():
    assert aggregate.find_blackhole([SPEAKERS]) is None


def test_find_by_uid():
    assert aggregate.find_by_uid([SPEAKERS, BLACKHOLE], "BlackHole16ch_UID") is BLACKHOLE
    assert aggregate.find_by_uid([SPEAKERS], "missing") is None
    assert aggregate.find_by_uid([SPEAKERS], None) is None


# --- setup_routing --------------------------------------------------------

def _patch_setup(devices, current_default, create_id=200):
    """Patch every raw CoreAudio op; return the patch context + the mocks."""
    mocks = {
        "list_devices": MagicMock(return_value=devices),
        "default_output": MagicMock(return_value=current_default),
        "create_aggregate": MagicMock(return_value=create_id),
        "destroy_aggregate": MagicMock(),
        "set_default_output": MagicMock(),
        "set_device_volume": MagicMock(),
        "reinit_portaudio": MagicMock(),
        "sd_index_for_name": MagicMock(return_value=8),
    }
    return patch.multiple("paraeq.audio.aggregate", **mocks), mocks


def test_setup_routing_orders_blackhole_first():
    ctx, m = _patch_setup([SPEAKERS, BLACKHOLE], SPEAKERS)
    with ctx:
        state = aggregate.setup_routing()

    assert state is not None
    # BlackHole must be the FIRST sub-device (→ channels 1..16) and the main.
    sub_uids = m["create_aggregate"].call_args.args[0]
    main_uid = m["create_aggregate"].call_args.args[1]
    assert sub_uids == ["BlackHole16ch_UID", "BuiltInSpeakerDevice"]
    assert main_uid == "BlackHole16ch_UID"
    # New aggregate becomes the system default; sounddevice index resolved.
    m["set_default_output"].assert_called_once_with(200)
    assert state.aggregate_index == 8
    assert state.restore_output_uid == "BuiltInSpeakerDevice"
    assert state.restore_output_id == 90


def test_setup_routing_returns_none_without_blackhole():
    ctx, m = _patch_setup([SPEAKERS], SPEAKERS)
    with ctx:
        assert aggregate.setup_routing() is None
    m["create_aggregate"].assert_not_called()


def test_setup_routing_destroys_pre_existing_aggregate():
    ctx, m = _patch_setup([SPEAKERS, BLACKHOLE, STALE_AGG], SPEAKERS)
    with ctx:
        aggregate.setup_routing()
    # A leaked ParaEQ aggregate is destroyed before a fresh one is created.
    m["destroy_aggregate"].assert_any_call(103)
    m["create_aggregate"].assert_called_once()


def test_setup_routing_crash_recovery_uses_previous_physical():
    # Default output is a STALE ParaEQ aggregate (app crashed last time).
    ctx, m = _patch_setup([SPEAKERS, BLACKHOLE, STALE_AGG], STALE_AGG)
    with ctx:
        state = aggregate.setup_routing(previous_physical_uid="BuiltInSpeakerDevice")

    sub_uids = m["create_aggregate"].call_args.args[0]
    assert sub_uids == ["BlackHole16ch_UID", "BuiltInSpeakerDevice"]
    assert state.restore_output_uid == "BuiltInSpeakerDevice"
    m["destroy_aggregate"].assert_any_call(103)


def test_setup_routing_falls_back_when_default_is_stale_and_no_hint():
    # Crash recovery with no persisted hint: pick a real physical output.
    ctx, m = _patch_setup([HEADPHONES, BLACKHOLE, STALE_AGG], STALE_AGG)
    with ctx:
        state = aggregate.setup_routing(previous_physical_uid=None)
    assert state.restore_output_uid == "ExtHeadphones_UID"


def test_setup_routing_normalizes_blackhole_volume():
    # BlackHole's device volume silently scales the loopback (a user who ever
    # lowered it in Audio MIDI Setup gets a mysteriously quiet system) — setup
    # must force it back to unity.
    ctx, m = _patch_setup([SPEAKERS, BLACKHOLE], SPEAKERS)
    with ctx:
        state = aggregate.setup_routing()
    assert state is not None
    m["set_device_volume"].assert_called_once_with(BLACKHOLE.id, 1.0)


def test_setup_routing_survives_volume_set_failure():
    # Volume normalization is best-effort: a failure means quieter audio, not
    # no audio — routing must still be established.
    ctx, m = _patch_setup([SPEAKERS, BLACKHOLE], SPEAKERS)
    m["set_device_volume"].side_effect = aggregate.AggregateError("boom")
    with ctx:
        state = aggregate.setup_routing()
    assert state is not None
    m["create_aggregate"].assert_called_once()


def test_setup_routing_returns_none_on_create_failure():
    ctx, m = _patch_setup([SPEAKERS, BLACKHOLE], SPEAKERS)
    m["create_aggregate"].side_effect = aggregate.AggregateError("boom")
    with ctx:
        assert aggregate.setup_routing() is None


# --- teardown_routing -----------------------------------------------------

def test_teardown_restores_output_then_destroys_aggregate():
    order = []
    state = RoutingState(
        aggregate_id=200,
        aggregate_uid=PARAEQ_AGGREGATE_UID,
        aggregate_index=8,
        restore_output_uid="BuiltInSpeakerDevice",
        restore_output_id=90,
    )
    with patch.multiple(
        "paraeq.audio.aggregate",
        list_devices=MagicMock(return_value=[SPEAKERS, BLACKHOLE]),
        set_default_output=MagicMock(side_effect=lambda i: order.append(("set", i))),
        destroy_aggregate=MagicMock(side_effect=lambda i: order.append(("destroy", i))),
        reinit_portaudio=MagicMock(),
    ):
        aggregate.teardown_routing(state)

    # Restore the physical output FIRST (re-resolved by UID), then destroy.
    assert order == [("set", 90), ("destroy", 200)]


def test_teardown_noop_on_none():
    # Should not raise.
    aggregate.teardown_routing(None)


# --- regressions for review findings: never leave the Mac silent -----------

def test_find_blackhole_ignores_aggregate_named_blackhole():
    # "blackholeAgg" must NOT be chosen as the BlackHole loopback.
    assert aggregate.find_blackhole([BLACKHOLE_AGG, BLACKHOLE]) is BLACKHOLE
    assert aggregate.find_blackhole([BLACKHOLE_AGG]) is None


def test_setup_routing_never_uses_blackhole_as_physical():
    # BlackHole is the current default → must fall back to a real output,
    # not build a silent [BlackHole, BlackHole] aggregate.
    ctx, m = _patch_setup([BLACKHOLE, SPEAKERS], BLACKHOLE)
    with ctx:
        state = aggregate.setup_routing()
    sub_uids = m["create_aggregate"].call_args.args[0]
    assert sub_uids == ["BlackHole16ch_UID", "BuiltInSpeakerDevice"]
    assert state.restore_output_uid == "BuiltInSpeakerDevice"


def test_setup_routing_rolls_back_when_aggregate_not_visible():
    # If the new aggregate never surfaces in sounddevice, setup must roll back
    # (restore the real default + destroy the aggregate) and report failure —
    # otherwise the aggregate is left as default and the session is silent.
    ctx, m = _patch_setup([SPEAKERS, BLACKHOLE], SPEAKERS)
    m["sd_index_for_name"].return_value = None
    with ctx:
        state = aggregate.setup_routing()
    assert state is None
    m["set_default_output"].assert_any_call(90)  # physical output restored
    m["destroy_aggregate"].assert_any_call(200)  # created aggregate destroyed


def test_teardown_destroys_aggregate_even_if_restore_fails():
    destroy = MagicMock()
    state = RoutingState(
        aggregate_id=200,
        aggregate_uid=PARAEQ_AGGREGATE_UID,
        aggregate_index=8,
        restore_output_uid="BuiltInSpeakerDevice",
        restore_output_id=90,
    )
    with patch.multiple(
        "paraeq.audio.aggregate",
        list_devices=MagicMock(return_value=[SPEAKERS, BLACKHOLE]),
        set_default_output=MagicMock(side_effect=RuntimeError("boom")),
        destroy_aggregate=destroy,
        reinit_portaudio=MagicMock(),
    ):
        aggregate.teardown_routing(state)
    destroy.assert_called_once_with(200)


def test_pick_fallback_skips_input_only_devices():
    # An input-only device (mic) must never be picked as the physical output.
    assert aggregate._pick_fallback_output([MIC, SPEAKERS]) is SPEAKERS
    assert aggregate._pick_fallback_output([MIC]) is None


def test_setup_routing_returns_none_when_only_fallback_is_a_mic():
    # BlackHole is the default and the only other device is an input → no
    # usable output → bail (don't build an aggregate around a microphone).
    ctx, m = _patch_setup([BLACKHOLE, MIC], BLACKHOLE)
    with ctx:
        assert aggregate.setup_routing() is None
    m["create_aggregate"].assert_not_called()
