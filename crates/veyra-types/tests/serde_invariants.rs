use veyra_types::{LogSequenceNumber, ProjectionProgress};

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
fn serde_rejects_applied_progress_ahead_of_durable() {
    let invalid = r#"{
        "received": 10,
        "durable": 8,
        "applied": 9,
        "published": 7
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

#[test]
fn serde_accepts_an_ordered_progress_chain() {
    let valid = r#"{
        "received": 10,
        "durable": 9,
        "applied": 8,
        "published": 7
    }"#;

    let progress =
        serde_json::from_str::<ProjectionProgress>(valid).unwrap_or_else(|_| unreachable!());
    assert_eq!(progress.received(), LogSequenceNumber::new(10));
    assert_eq!(progress.durable(), LogSequenceNumber::new(9));
    assert_eq!(progress.applied(), LogSequenceNumber::new(8));
    assert_eq!(progress.published(), LogSequenceNumber::new(7));
}
