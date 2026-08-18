use veyra_runtime::{CannotProveReason, RuntimeSnapshot, ServicePhase};
use veyra_types::{GenerationId, LogSequenceNumber};

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

#[test]
fn serde_accepts_ready_runtime_without_reason() {
    let valid = r#"{
        "phase": "ready",
        "generation": 7,
        "progress": {
            "received": 10,
            "durable": 10,
            "applied": 10,
            "published": 10
        },
        "cannot_prove": null
    }"#;

    let snapshot =
        serde_json::from_str::<RuntimeSnapshot>(valid).unwrap_or_else(|_| unreachable!());
    assert_eq!(snapshot.phase(), ServicePhase::Ready);
    assert_eq!(snapshot.generation(), GenerationId::new(7));
    assert_eq!(snapshot.cannot_prove_reason(), None);
    assert_eq!(snapshot.prove_queryable(LogSequenceNumber::new(10)), Ok(()));
}

#[test]
fn serde_accepts_non_ready_runtime_with_reason() {
    let valid = r#"{
        "phase": "degraded",
        "generation": 7,
        "progress": {
            "received": 10,
            "durable": 10,
            "applied": 10,
            "published": 10
        },
        "cannot_prove": "cdc_gap"
    }"#;

    let snapshot =
        serde_json::from_str::<RuntimeSnapshot>(valid).unwrap_or_else(|_| unreachable!());
    assert_eq!(snapshot.phase(), ServicePhase::Degraded);
    assert_eq!(
        snapshot.cannot_prove_reason(),
        Some(CannotProveReason::CdcGap)
    );
    assert_eq!(
        snapshot.prove_queryable(LogSequenceNumber::ZERO),
        Err(CannotProveReason::CdcGap)
    );
}
