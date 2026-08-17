use std::sync::Arc;

use veyra_runtime::{
    CannotProveReason, GenerationState, PublicationError, PublishedGeneration, RuntimeSnapshot,
};
use veyra_types::{GenerationId, LogSequenceNumber, ProjectionProgress};

fn progress(applied: u64) -> ProjectionProgress {
    ProjectionProgress::at(LogSequenceNumber::new(applied))
}

#[test]
fn publication_rejects_ready_metadata_without_payload() {
    let result = PublishedGeneration::<u32>::try_without_payload(RuntimeSnapshot::ready(
        GenerationId::new(1),
        progress(10),
    ));

    assert!(matches!(
        result,
        Err(PublicationError::ReadyWithoutPayload)
    ));
}

#[test]
fn publication_rejects_payload_for_non_ready_metadata() {
    let result = PublishedGeneration::try_with_payload(
        RuntimeSnapshot::starting(GenerationId::UNPUBLISHED, ProjectionProgress::ZERO),
        Arc::new(7_u32),
    );

    assert!(matches!(
        result,
        Err(PublicationError::PayloadRequiresReadyPhase)
    ));
}

#[test]
fn generation_state_admits_only_coherent_ready_payload_at_requested_lsn() {
    let initial = PublishedGeneration::try_without_payload(RuntimeSnapshot::starting(
        GenerationId::UNPUBLISHED,
        ProjectionProgress::ZERO,
    ))
    .unwrap_or_else(|_| unreachable!());
    let state = GenerationState::new(initial);

    assert_eq!(
        state.admit(LogSequenceNumber::ZERO),
        Err(CannotProveReason::NotReady)
    );

    let ready = PublishedGeneration::try_with_payload(
        RuntimeSnapshot::ready(GenerationId::new(9), progress(100)),
        Arc::new(42_u32),
    )
    .unwrap_or_else(|_| unreachable!());
    state.publish(ready);

    let payload = state
        .admit(LogSequenceNumber::new(100))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(*payload, 42);
    assert_eq!(
        state.admit(LogSequenceNumber::new(101)),
        Err(CannotProveReason::StaleProjection)
    );

    let snapshot = state.snapshot();
    assert_eq!(snapshot.status().generation(), GenerationId::new(9));
    assert!(snapshot.payload().is_some());
}
