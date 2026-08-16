#![forbid(unsafe_code)]

//! Immutable generation assembly and atomic reader publication.

use core::fmt;
use std::collections::BTreeMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use veyra_segment::{Segment, SegmentType};
use veyra_types::{GenerationId, LogSequenceNumber};

#[derive(Clone, Debug)]
pub struct Generation {
    id: GenerationId,
    start_lsn: LogSequenceNumber,
    end_lsn: LogSequenceNumber,
    segments: BTreeMap<SegmentType, Arc<Segment>>,
}

impl Generation {
    pub fn try_new(
        id: GenerationId,
        start_lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
        segments: impl IntoIterator<Item = Arc<Segment>>,
    ) -> Result<Self, StorageError> {
        if id == GenerationId::UNPUBLISHED {
            return Err(StorageError::UnpublishedGeneration);
        }
        if end_lsn < start_lsn {
            return Err(StorageError::LsnRangeReversed);
        }
        let mut by_type = BTreeMap::new();
        for segment in segments {
            let header = segment.header();
            if header.generation != id {
                return Err(StorageError::SegmentGenerationMismatch {
                    expected: id,
                    actual: header.generation,
                });
            }
            if header.start_lsn != start_lsn || header.end_lsn != end_lsn {
                return Err(StorageError::SegmentLsnMismatch(header.segment_type));
            }
            if by_type.insert(header.segment_type, segment).is_some() {
                return Err(StorageError::DuplicateSegmentType(header.segment_type));
            }
        }
        if by_type.is_empty() {
            return Err(StorageError::EmptyGeneration);
        }
        Ok(Self { id, start_lsn, end_lsn, segments: by_type })
    }

    #[must_use]
    pub const fn id(&self) -> GenerationId { self.id }
    #[must_use]
    pub const fn start_lsn(&self) -> LogSequenceNumber { self.start_lsn }
    #[must_use]
    pub const fn end_lsn(&self) -> LogSequenceNumber { self.end_lsn }
    #[must_use]
    pub fn segment(&self, segment_type: SegmentType) -> Option<&Arc<Segment>> {
        self.segments.get(&segment_type)
    }
    #[must_use]
    pub fn segment_count(&self) -> usize { self.segments.len() }
}

#[derive(Debug)]
pub struct GenerationStore {
    current: ArcSwap<Generation>,
}

impl GenerationStore {
    #[must_use]
    pub fn new(initial: Arc<Generation>) -> Self {
        Self { current: ArcSwap::from(initial) }
    }

    #[must_use]
    pub fn load(&self) -> Arc<Generation> { self.current.load_full() }

    /// Atomically publishes a strictly newer validated generation and returns the previous one.
    pub fn publish(&self, next: Arc<Generation>) -> Result<Arc<Generation>, StorageError> {
        let current = self.current.load_full();
        if next.id() <= current.id() {
            return Err(StorageError::GenerationNotNewer {
                current: current.id(),
                proposed: next.id(),
            });
        }
        if next.end_lsn() < current.end_lsn() {
            return Err(StorageError::PublishedLsnRegressed {
                current: current.end_lsn(),
                proposed: next.end_lsn(),
            });
        }
        Ok(self.current.swap(next))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageError {
    UnpublishedGeneration,
    LsnRangeReversed,
    EmptyGeneration,
    SegmentGenerationMismatch { expected: GenerationId, actual: GenerationId },
    SegmentLsnMismatch(SegmentType),
    DuplicateSegmentType(SegmentType),
    GenerationNotNewer { current: GenerationId, proposed: GenerationId },
    PublishedLsnRegressed { current: LogSequenceNumber, proposed: LogSequenceNumber },
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { write!(formatter, "{self:?}") }
}
impl std::error::Error for StorageError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(kind: SegmentType, generation: u64, start: u64, end: u64) -> Arc<Segment> {
        Arc::new(Segment::build(
            kind,
            0,
            GenerationId::new(generation),
            LogSequenceNumber::new(start),
            LogSequenceNumber::new(end),
            1,
            vec![u8::try_from(generation).unwrap_or_default()],
        ).unwrap_or_else(|_| unreachable!()))
    }

    fn generation(id: u64, start: u64, end: u64) -> Arc<Generation> {
        Arc::new(Generation::try_new(
            GenerationId::new(id),
            LogSequenceNumber::new(start),
            LogSequenceNumber::new(end),
            [segment(SegmentType::Availability, id, start, end)],
        ).unwrap_or_else(|_| unreachable!()))
    }

    #[test]
    fn generation_validates_segment_identity_and_ranges() {
        let gen = generation(2, 10, 20);
        assert_eq!(gen.id().get(), 2);
        assert_eq!(gen.start_lsn().get(), 10);
        assert_eq!(gen.end_lsn().get(), 20);
        assert_eq!(gen.segment_count(), 1);
        assert!(gen.segment(SegmentType::Availability).is_some());
        assert!(gen.segment(SegmentType::Pricing).is_none());

        assert_eq!(Generation::try_new(GenerationId::UNPUBLISHED, 0.into(), 0.into(), [segment(SegmentType::Availability, 0, 0, 0)]).err(), Some(StorageError::UnpublishedGeneration));
        assert_eq!(Generation::try_new(GenerationId::new(1), LogSequenceNumber::new(2), LogSequenceNumber::new(1), [segment(SegmentType::Availability, 1, 2, 1)]).err(), Some(StorageError::LsnRangeReversed));
        assert_eq!(Generation::try_new(GenerationId::new(1), 0.into(), 0.into(), std::iter::empty()).err(), Some(StorageError::EmptyGeneration));
    }

    #[test]
    fn generation_rejects_mismatched_and_duplicate_segments() {
        let wrong_generation = Generation::try_new(
            GenerationId::new(2),
            LogSequenceNumber::new(10),
            LogSequenceNumber::new(20),
            [segment(SegmentType::Availability, 3, 10, 20)],
        );
        assert!(matches!(wrong_generation, Err(StorageError::SegmentGenerationMismatch { .. })));

        let wrong_lsn = Generation::try_new(
            GenerationId::new(2),
            LogSequenceNumber::new(10),
            LogSequenceNumber::new(20),
            [segment(SegmentType::Availability, 2, 11, 20)],
        );
        assert_eq!(wrong_lsn.err(), Some(StorageError::SegmentLsnMismatch(SegmentType::Availability)));

        let duplicate = Generation::try_new(
            GenerationId::new(2),
            LogSequenceNumber::new(10),
            LogSequenceNumber::new(20),
            [
                segment(SegmentType::Availability, 2, 10, 20),
                segment(SegmentType::Availability, 2, 10, 20),
            ],
        );
        assert_eq!(duplicate.err(), Some(StorageError::DuplicateSegmentType(SegmentType::Availability)));
    }

    #[test]
    fn publication_keeps_old_readers_alive() {
        let first = generation(1, 1, 10);
        let store = GenerationStore::new(Arc::clone(&first));
        let old_reader = store.load();
        let second = generation(2, 1, 20);
        let replaced = store.publish(Arc::clone(&second)).unwrap_or_else(|_| unreachable!());
        assert_eq!(replaced.id().get(), 1);
        assert_eq!(old_reader.id().get(), 1);
        assert_eq!(store.load().id().get(), 2);
        assert_eq!(second.id().get(), 2);
    }

    #[test]
    fn publication_rejects_generation_and_lsn_regression() {
        let store = GenerationStore::new(generation(5, 1, 50));
        assert!(matches!(store.publish(generation(5, 1, 50)), Err(StorageError::GenerationNotNewer { .. })));
        assert!(matches!(store.publish(generation(6, 1, 40)), Err(StorageError::PublishedLsnRegressed { .. })));
        assert_eq!(store.load().id().get(), 5);
    }
}

#[cfg(test)]
impl From<u64> for LogSequenceNumber {
    fn from(value: u64) -> Self { Self::new(value) }
}
