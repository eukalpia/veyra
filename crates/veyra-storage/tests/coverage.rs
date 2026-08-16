use std::sync::Arc;

use veyra_segment::{Segment, SegmentType};
use veyra_storage::{Generation, GenerationStore, StorageError};
use veyra_types::{GenerationId, LogSequenceNumber};

fn segment(kind: SegmentType, generation: u64, start: u64, end: u64) -> Arc<Segment> {
    Arc::new(
        Segment::build(
            kind,
            0,
            GenerationId::new(generation),
            LogSequenceNumber::new(start),
            LogSequenceNumber::new(end),
            1,
            vec![1],
        )
        .unwrap_or_else(|_| unreachable!()),
    )
}

fn generation(id: u64, end: u64) -> Arc<Generation> {
    Arc::new(
        Generation::try_new(
            GenerationId::new(id),
            LogSequenceNumber::new(1),
            LogSequenceNumber::new(end),
            [segment(SegmentType::Availability, id, 1, end)],
        )
        .unwrap_or_else(|_| unreachable!()),
    )
}

#[test]
fn store_debug_and_atomic_replacement_are_observable() {
    let initial = generation(1, 10);
    assert_eq!(initial.id(), GenerationId::new(1));
    assert_eq!(initial.start_lsn(), LogSequenceNumber::new(1));
    assert_eq!(initial.end_lsn(), LogSequenceNumber::new(10));
    assert_eq!(initial.segment_count(), 1);
    assert!(initial.segment(SegmentType::Availability).is_some());
    assert!(initial.segment(SegmentType::Pricing).is_none());

    let store = GenerationStore::new(Arc::clone(&initial));
    assert!(format!("{store:?}").contains("GenerationStore"));
    assert_eq!(store.load().id(), GenerationId::new(1));

    let old = store
        .publish(generation(2, 20))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(old.id(), GenerationId::new(1));
    let current = store.load();
    assert_eq!(current.id(), GenerationId::new(2));
    assert_eq!(current.start_lsn(), LogSequenceNumber::new(1));
    assert_eq!(current.end_lsn(), LogSequenceNumber::new(20));
}

#[test]
fn every_storage_error_has_stable_display() {
    for error in [
        StorageError::UnpublishedGeneration,
        StorageError::LsnRangeReversed,
        StorageError::EmptyGeneration,
        StorageError::SegmentGenerationMismatch {
            expected: GenerationId::new(1),
            actual: GenerationId::new(2),
        },
        StorageError::SegmentLsnMismatch(SegmentType::Pricing),
        StorageError::DuplicateSegmentType(SegmentType::Rules),
        StorageError::GenerationNotNewer {
            current: GenerationId::new(2),
            proposed: GenerationId::new(1),
        },
        StorageError::PublishedLsnRegressed {
            current: LogSequenceNumber::new(20),
            proposed: LogSequenceNumber::new(10),
        },
    ] {
        assert!(!error.to_string().is_empty());
    }
}
