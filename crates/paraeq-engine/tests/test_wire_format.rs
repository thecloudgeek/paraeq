//! Stage-4 wire contract: `desktop/ui/src/ipc/types.ts` mirrors THESE shapes
//! by hand (no codegen). If a field name or the `EngineStatus` tagging
//! changes here, the TS types must change in the same PR -- this golden test
//! is the tripwire.

use paraeq_engine::backend::StreamInfo;
use paraeq_engine::controller::EngineState;
use paraeq_engine::status::EngineStatus;

#[test]
fn engine_state_wire_format_is_pinned() {
    let state = EngineState {
        bypass: false,
        correction: Some("iir:2-band".into()),
        correction_rate_mismatch: None,
        enabled: true,
        frame_mismatch_blocks: 0,
        gain_db: -3.0,
        input_peak: 0.25,
        latency_ms: Some(62.3),
        status: EngineStatus::NoInputDetected { since_ms: 1200 },
        stream: Some(StreamInfo {
            buffer_frames: 512,
            channels: 2,
            device_uid: "uid-1".into(),
            sample_rate: 48000.0,
        }),
    };
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        serde_json::json!({
            "bypass": false,
            "correction": "iir:2-band",
            "correction_rate_mismatch": null,
            "enabled": true,
            "frame_mismatch_blocks": 0,
            "gain_db": -3.0,
            "input_peak": 0.25,
            "latency_ms": 62.3,
            "status": { "kind": "no_input_detected", "since_ms": 1200 },
            "stream": {
                "buffer_frames": 512,
                "channels": 2,
                "device_uid": "uid-1",
                "sample_rate": 48000.0
            }
        })
    );
}

#[test]
fn engine_status_variants_are_snake_case_internally_tagged() {
    // Unit variant.
    assert_eq!(
        serde_json::to_value(EngineStatus::Running).unwrap(),
        serde_json::json!({ "kind": "running" })
    );
    // Struct variant.
    assert_eq!(
        serde_json::to_value(EngineStatus::AutoDisabledNoInput { after_ms: 15000 }).unwrap(),
        serde_json::json!({ "kind": "auto_disabled_no_input", "after_ms": 15000 })
    );
    assert_eq!(
        serde_json::to_value(EngineStatus::Stopped).unwrap(),
        serde_json::json!({ "kind": "stopped" })
    );
    assert_eq!(
        serde_json::to_value(EngineStatus::Failed {
            reason: "boom".into()
        })
        .unwrap(),
        serde_json::json!({ "kind": "failed", "reason": "boom" })
    );
}
