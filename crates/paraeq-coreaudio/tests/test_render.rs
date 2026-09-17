//! The verification helper's render device.
//!
//! Two kinds of test live here. The PURE ones run everywhere and are about the
//! composition dictionary and about the one thing two realtime output paths
//! must never disagree on — routing. The `#[ignore]` ones need a real HAL and
//! are run by hand on the rig; they exist so the create/destroy pair and the
//! panic path have named assertions rather than a reviewer's confidence.
//!
//! Test tier: none of the four oracle tiers applies — platform FFI with no
//! numerical oracle (the `volume.rs` precedent).

use paraeq_coreaudio::render::{
    render_aggregate_composition, DeviceRenderer, RenderAggregate, RenderConfig, RenderTarget,
};

/// The R4 hazard — a spurious microphone-permission prompt from a
/// differently-named binary, mid-wizard — is a KEY in a dictionary, and a key
/// is assertable with no device attached.
#[test]
fn the_render_aggregate_composition_dictionary_has_input_channels_zero() {
    let composition = render_aggregate_composition("BuiltInSpeakerDevice");

    assert_eq!(
        composition.input_channels, 0,
        "kAudioSubDeviceInputChannelsKey must be 0: without it, running our IOProc on a \
         mic-capable default output (AirPods, a USB headset) counts as microphone access and \
         macOS shows a mic permission prompt — indistinguishable from ParaEQ's legitimate one"
    );
    assert!(
        composition.is_private,
        "the render device must never appear in the user's device list"
    );
    assert!(!composition.is_stacked);
    assert_eq!(
        composition.main_sub_device_uid, "BuiltInSpeakerDevice",
        "the clock master IS the physical output, which is what keeps the render path on the \
         measurement aggregate's crystal"
    );
    assert_eq!(composition.sub_device_uid, "BuiltInSpeakerDevice");
    assert_eq!(composition.name, "paraeq-render-agg");
    assert_eq!(
        composition.uid,
        format!("com.paraeq.render.{}", std::process::id()),
        "per-process, so the parent can assert THIS device is gone after the child exits"
    );
}

/// The composition carries no tap, and it must stay that way: a tap on the
/// render device would route the helper's own audio back through the chain a
/// second time.
#[test]
fn the_render_aggregate_carries_no_tap_list_and_no_tap_autostart() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/render.rs"))
        .expect("render.rs is beside this test");
    for forbidden in [
        "kAudioAggregateDeviceTapListKey",
        "kAudioAggregateDeviceTapAutoStartKey",
        "kAudioSubTapUIDKey",
    ] {
        // The doc comment names both keys to say they are absent, so read the
        // code rather than the prose.
        let code: String = src
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains(forbidden),
            "`{forbidden}` reached the render aggregate's composition"
        );
    }
}

/// The drift hazard two realtime output paths create, closed by a test rather
/// than by a move: the in-process stimulus path and the helper's renderer must
/// interpret `Only(n)` identically, including its refusal to fall back to
/// channel 0 on a device that cannot honour it.
#[test]
fn write_frame_is_the_only_place_routing_is_interpreted() {
    let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut interpreters = Vec::new();
    for entry in std::fs::read_dir(src_dir).expect("src/ is readable") {
        let path = entry.expect("a readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("a readable source file");
        for (i, line) in src.lines().enumerate() {
            let line = line.trim_start();
            if line.starts_with("//") {
                continue;
            }
            // A `StimulusRouting::Both` MATCH ARM is what interpreting the
            // routing looks like. Constructing one (`routing: StimulusRouting::Both,`)
            // is not.
            if line.contains("StimulusRouting::Both =>") {
                interpreters.push(format!("{}:{}", path.display(), i + 1));
            }
        }
    }
    assert_eq!(
        interpreters.len(),
        1,
        "routing is interpreted in more than one place: {interpreters:?}. A second `match` on \
         StimulusRouting is a second chance to lose the refusal-to-fall-back rule — silence is \
         an obvious failure, a wrong channel is a plausible wrong answer."
    );
    assert!(
        interpreters[0].contains("measure_aggregate.rs"),
        "the one interpreter should be `write_frame` in measure_aggregate.rs, got {}",
        interpreters[0]
    );
}

// ------------------------------------------------------------ hardware tests
//
// These need a real HAL. They never play audio: they create the render
// aggregate, look at it, and destroy it.

#[test]
#[ignore = "requires audio hardware"]
fn render_aggregate_create_and_destroy_round_trips() {
    let out = paraeq_coreaudio::properties::default_output_device().expect("a default output");
    let uid = paraeq_coreaudio::properties::device_uid(out).expect("its UID");

    let mut aggregate = RenderAggregate::create(&uid).expect("the render aggregate");
    assert_ne!(aggregate.id(), 0, "the HAL handed back a real device id");
    assert_eq!(
        paraeq_coreaudio::properties::translate_uid_to_device(aggregate.uid())
            .expect("the UID resolves"),
        aggregate.id(),
        "the aggregate is reachable by the UID we composed it with — which is the UID the \
         parent's teardown asserts is gone"
    );

    assert!(
        aggregate.teardown().is_empty(),
        "destroy reported no OSStatus failure"
    );
    assert!(
        aggregate.teardown().is_empty(),
        "a second destroy is a no-op, not a second HAL call: `torn_down` guards it exactly as \
         TapSystem and MeasureAggregate do"
    );
}

#[test]
#[ignore = "requires audio hardware"]
fn render_aggregate_is_destroyed_on_drop_and_on_panic() {
    let out = paraeq_coreaudio::properties::default_output_device().expect("a default output");
    let uid = paraeq_coreaudio::properties::device_uid(out).expect("its UID");
    let expected_uid = format!("com.paraeq.render.{}", std::process::id());

    let caught = std::panic::catch_unwind(|| {
        let _aggregate = RenderAggregate::create(&uid).expect("the render aggregate");
        panic!("injected: the helper dies mid-render");
    });
    assert!(caught.is_err(), "the injected panic unwound");

    // The assertion the teardown hole needs, and the same query the parent's
    // teardown runs: no device with our render UID survives.
    assert_eq!(
        paraeq_coreaudio::properties::translate_uid_to_device(&expected_uid)
            .expect("the lookup itself succeeds"),
        0,
        "a private aggregate survived an unwinding helper and is now wrapping the user's \
         output device"
    );
}

#[test]
#[ignore = "requires audio hardware"]
fn a_renderer_opens_on_the_default_output_and_reports_a_usable_format() {
    let mut renderer = DeviceRenderer::open(RenderConfig {
        target: RenderTarget::DefaultOutput,
        ..RenderConfig::default()
    })
    .expect("the default output opens");

    let format = paraeq_measure::RenderSink::format(&renderer);
    assert!(
        format.channels >= 1,
        "a device with no output channels is refused at open"
    );
    assert!(format.frames_per_block >= 1);
    assert!(format.sample_rate_hz > 0.0);
    assert!(
        !renderer.render_device_uid().is_empty(),
        "the aggregate's UID is what the parent's rate fence reads"
    );
    assert!(renderer.sub_device_sample_rate_hz() > 0.0);

    paraeq_measure::RenderSink::stop(&mut renderer);
    assert_eq!(
        paraeq_coreaudio::properties::translate_uid_to_device(&format!(
            "com.paraeq.render.{}",
            std::process::id()
        ))
        .expect("the lookup itself succeeds"),
        0,
        "stop destroyed the render aggregate"
    );
}

#[test]
#[ignore = "requires audio hardware"]
fn an_empty_uid_is_refused_before_any_hal_call() {
    // Selector validation BEFORE any HAL call (the volume.rs shape). Listed
    // with the hardware tests because it sits beside them, but it touches no
    // device by construction — which is exactly what it asserts.
    let opened = DeviceRenderer::open(RenderConfig {
        target: RenderTarget::Uid(String::new()),
        ..RenderConfig::default()
    });
    match opened {
        Err(paraeq_coreaudio::render::RenderError::EmptyUid) => {}
        Err(other) => panic!("wrong refusal: {other}"),
        Ok(_) => panic!("an empty UID is a caller bug, not a device question"),
    }
}
