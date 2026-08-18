use veyra_runtime::RuntimeSnapshot;

#[test]
fn serde_rejects_non_ready_runtime_without_fail_closed_reason() {
    let invalid = r#"{
        "phase": "starting",
        "generation": 0,
        "progress": {
            "received": 0,
            "durable": 0,
            "applied": 0,
            "published": 0
        },
        "cannot_prove": null
    }"#;

    assert!(serde_json::from_str::<RuntimeSnapshot>(invalid).is_err());
}

#[test]
fn serde_rejects_ready_runtime_with_cannot_prove_reason() {
    let invalid = r#"{
        "phase": "ready",
        "generation": 7,
        "progress": {
            "received": 10,
            "durable": 10,
            "applied": 10,
            "published": 10
        },
        "cannot_prove": "cdc_gap"
    }"#;

    assert!(serde_json::from_str::<RuntimeSnapshot>(invalid).is_err());
}
