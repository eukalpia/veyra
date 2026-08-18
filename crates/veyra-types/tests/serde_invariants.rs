use veyra_types::ProjectionProgress;

#[test]
fn serde_rejects_durable_progress_ahead_of_received() {
    let invalid = r#"{
        "received": 10,
        "durable": 11,
        "applied": 9,
        "published": 8
    }"#;

    assert!(serde_json::from_str::<ProjectionProgress>(invalid).is_err());
}

#[test]
fn serde_rejects_published_progress_ahead_of_applied() {
    let invalid = r#"{
        "received": 10,
        "durable": 10,
        "applied": 8,
        "published": 9
    }"#;

    assert!(serde_json::from_str::<ProjectionProgress>(invalid).is_err());
}
