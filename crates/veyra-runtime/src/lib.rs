#![forbid(unsafe_code)]

//! Fault-closed publication state for the Veyra process.
//!
//! Query-serving code reads an immutable snapshot through `ArcSwap`. Writers publish a
//! complete replacement; readers never observe a partially-mutated runtime status.

use std::fmt;
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use veyra_types::{GenerationId, LogSequenceNumber, ProjectionProgress};

/// Coarse lifecycle state exposed to health and query admission.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicePhase {
    /// Process is alive but no queryable projection has been proven.
    Starting,
    /// Projection is valid and query admission may proceed.
    Ready,
    /// Process is alive, but affected queries must use `PostgreSQL` fallback.
    Degraded,
    /// Runtime encountered a fatal condition and must not serve Veyra results.
    Failed,
}

/// Why Veyra cannot prove a query result is safe to serve.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CannotProveReason {
    /// No validated generation is published yet.
    NotReady,
    /// Published projection does not satisfy a requested minimum LSN.
    StaleProjection,
    /// Logical replication continuity could not be proven.
    CdcGap,
    /// A segment, manifest, or generation failed integrity validation.
    CorruptGeneration,
    /// Storage, rule, projection, protocol, or engine semantics are incompatible.
    VersionMismatch,
    /// The query or rules require semantics this engine version does not implement.
    UnsupportedSemantics,
    /// Bounded queues or CPU budgets are exhausted.
    Overloaded,
    /// An internal invariant failed.
    InternalInvariantFailure,
}

/// Immutable runtime metadata published atomically to all readers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeSnapshot {
    phase: ServicePhase,
    generation: GenerationId,
    progress: ProjectionProgress,
    cannot_prove: Option<CannotProveReason>,
}

impl RuntimeSnapshot {
    /// Creates the initial fail-closed state.
    #[must_use]
    pub const fn starting(generation: GenerationId, progress: ProjectionProgress) -> Self {
        Self {
            phase: ServicePhase::Starting,
            generation,
            progress,
            cannot_prove: Some(CannotProveReason::NotReady),
        }
    }

    /// Creates a fully queryable state after generation validation and publication.
    #[must_use]
    pub const fn ready(generation: GenerationId, progress: ProjectionProgress) -> Self {
        Self {
            phase: ServicePhase::Ready,
            generation,
            progress,
            cannot_prove: None,
        }
    }

    /// Creates a recoverable fail-closed state.
    #[must_use]
    pub const fn degraded(
        generation: GenerationId,
        progress: ProjectionProgress,
        reason: CannotProveReason,
    ) -> Self {
        Self {
            phase: ServicePhase::Degraded,
            generation,
            progress,
            cannot_prove: Some(reason),
        }
    }

    /// Creates a fatal fail-closed state.
    #[must_use]
    pub const fn failed(
        generation: GenerationId,
        progress: ProjectionProgress,
        reason: CannotProveReason,
    ) -> Self {
        Self {
            phase: ServicePhase::Failed,
            generation,
            progress,
            cannot_prove: Some(reason),
        }
    }

    /// Current service phase.
    #[must_use]
    pub const fn phase(&self) -> ServicePhase {
        self.phase
    }

    /// Currently published immutable generation.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Current replication progress.
    #[must_use]
    pub const fn progress(&self) -> ProjectionProgress {
        self.progress
    }

    /// Explicit reason a result cannot currently be proven.
    #[must_use]
    pub const fn cannot_prove_reason(&self) -> Option<CannotProveReason> {
        self.cannot_prove
    }

    /// Applies read-your-writes admission semantics.
    ///
    /// `Ok(())` means the runtime is ready and its applied LSN is at least `minimum_lsn`.
    pub fn prove_queryable(&self, minimum_lsn: LogSequenceNumber) -> Result<(), CannotProveReason> {
        if let Some(reason) = self.cannot_prove {
            return Err(reason);
        }

        if self.progress.applied() < minimum_lsn {
            return Err(CannotProveReason::StaleProjection);
        }

        Ok(())
    }
}

/// Atomically published runtime status shared by server tasks.
#[derive(Clone)]
pub struct RuntimeState {
    inner: Arc<ArcSwap<RuntimeSnapshot>>,
}

impl RuntimeState {
    /// Creates publication state from a complete immutable snapshot.
    #[must_use]
    pub fn new(snapshot: RuntimeSnapshot) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(snapshot)),
        }
    }

    /// Loads one coherent immutable snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Arc<RuntimeSnapshot> {
        self.inner.load_full()
    }

    /// Atomically replaces the published snapshot.
    pub fn publish(&self, snapshot: RuntimeSnapshot) {
        self.inner.store(Arc::new(snapshot));
    }
}

/// One coherent immutable runtime publication containing both status and query payload.
pub struct PublishedGeneration<T> {
    status: RuntimeSnapshot,
    payload: Option<Arc<T>>,
}

impl<T> PublishedGeneration<T> {
    /// Creates a fail-closed publication with no query payload.
    pub fn try_without_payload(status: RuntimeSnapshot) -> Result<Self, PublicationError> {
        if status.phase() == ServicePhase::Ready {
            return Err(PublicationError::ReadyWithoutPayload);
        }
        Ok(Self {
            status,
            payload: None,
        })
    }

    /// Creates a queryable publication. Payload and Ready metadata become visible atomically.
    pub fn try_with_payload(
        status: RuntimeSnapshot,
        payload: Arc<T>,
    ) -> Result<Self, PublicationError> {
        if status.phase() != ServicePhase::Ready {
            return Err(PublicationError::PayloadRequiresReadyPhase);
        }
        Ok(Self {
            status,
            payload: Some(payload),
        })
    }

    /// Runtime status paired with this exact payload generation.
    #[must_use]
    pub const fn status(&self) -> &RuntimeSnapshot {
        &self.status
    }

    /// Immutable query payload, present only for Ready publications.
    #[must_use]
    pub fn payload(&self) -> Option<&Arc<T>> {
        self.payload.as_ref()
    }
}

/// Invalid attempt to construct an incoherent runtime publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationError {
    /// Ready metadata must always carry the payload it claims is queryable.
    ReadyWithoutPayload,
    /// A query payload cannot be published while metadata is fail-closed.
    PayloadRequiresReadyPhase,
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for PublicationError {}

/// Atomically published status + immutable generation payload.
#[derive(Clone)]
pub struct GenerationState<T> {
    inner: Arc<ArcSwap<PublishedGeneration<T>>>,
}

impl<T> GenerationState<T> {
    /// Creates generation state from one already-coherent publication.
    #[must_use]
    pub fn new(initial: PublishedGeneration<T>) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(initial)),
        }
    }

    /// Loads status and payload from the same atomic publication.
    #[must_use]
    pub fn snapshot(&self) -> Arc<PublishedGeneration<T>> {
        self.inner.load_full()
    }

    /// Atomically replaces status and payload together.
    pub fn publish(&self, next: PublishedGeneration<T>) {
        self.inner.store(Arc::new(next));
    }

    /// Admits one query and returns the exact immutable payload generation that proved safe.
    pub fn admit(&self, minimum_lsn: LogSequenceNumber) -> Result<Arc<T>, CannotProveReason> {
        let publication = self.snapshot();
        publication.status().prove_queryable(minimum_lsn)?;
        publication
            .payload()
            .cloned()
            .ok_or(CannotProveReason::InternalInvariantFailure)
    }
}

impl fmt::Debug for RuntimeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeState")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    fn progress(applied: u64) -> ProjectionProgress {
        ProjectionProgress::at(LogSequenceNumber::new(applied))
    }

    #[test]
    fn starting_state_fails_closed() {
        let snapshot =
            RuntimeSnapshot::starting(GenerationId::UNPUBLISHED, ProjectionProgress::ZERO);

        assert_eq!(snapshot.phase(), ServicePhase::Starting);
        assert_eq!(snapshot.generation(), GenerationId::UNPUBLISHED);
        assert_eq!(snapshot.progress(), ProjectionProgress::ZERO);
        assert_eq!(
            snapshot.cannot_prove_reason(),
            Some(CannotProveReason::NotReady)
        );
        assert_eq!(
            snapshot.prove_queryable(LogSequenceNumber::ZERO),
            Err(CannotProveReason::NotReady)
        );
    }

    #[test]
    fn ready_state_honors_minimum_lsn() {
        let snapshot = RuntimeSnapshot::ready(GenerationId::new(7), progress(100));

        assert_eq!(snapshot.phase(), ServicePhase::Ready);
        assert_eq!(snapshot.generation(), GenerationId::new(7));
        assert_eq!(snapshot.cannot_prove_reason(), None);
        assert_eq!(
            snapshot.prove_queryable(LogSequenceNumber::new(100)),
            Ok(())
        );
        assert_eq!(
            snapshot.prove_queryable(LogSequenceNumber::new(101)),
            Err(CannotProveReason::StaleProjection)
        );
    }

    #[test]
    fn degraded_and_failed_states_preserve_reason() {
        let degraded = RuntimeSnapshot::degraded(
            GenerationId::new(4),
            progress(50),
            CannotProveReason::CdcGap,
        );
        let failed = RuntimeSnapshot::failed(
            GenerationId::new(4),
            progress(50),
            CannotProveReason::CorruptGeneration,
        );

        assert_eq!(degraded.phase(), ServicePhase::Degraded);
        assert_eq!(
            degraded.prove_queryable(LogSequenceNumber::ZERO),
            Err(CannotProveReason::CdcGap)
        );
        assert_eq!(failed.phase(), ServicePhase::Failed);
        assert_eq!(
            failed.prove_queryable(LogSequenceNumber::ZERO),
            Err(CannotProveReason::CorruptGeneration)
        );
    }

    #[test]
    fn publication_replaces_whole_snapshot_atomically() {
        let state = RuntimeState::new(RuntimeSnapshot::starting(
            GenerationId::UNPUBLISHED,
            ProjectionProgress::ZERO,
        ));
        let reader = state.clone();

        let handle = thread::spawn(move || reader.snapshot());
        assert!(handle.join().is_ok());

        let before = state.snapshot();
        assert_eq!(before.phase(), ServicePhase::Starting);

        state.publish(RuntimeSnapshot::ready(GenerationId::new(1), progress(10)));

        let after = state.snapshot();
        assert_eq!(after.phase(), ServicePhase::Ready);
        assert_eq!(after.generation(), GenerationId::new(1));
        assert_eq!(after.progress().published(), LogSequenceNumber::new(10));
    }

    #[test]
    fn debug_output_contains_current_snapshot() {
        let state = RuntimeState::new(RuntimeSnapshot::ready(GenerationId::new(2), progress(20)));

        let debug = format!("{state:?}");
        assert!(debug.contains("RuntimeState"));
        assert!(debug.contains("Ready"));
    }

    #[test]
    fn every_cannot_prove_reason_is_distinct() {
        let reasons = [
            CannotProveReason::NotReady,
            CannotProveReason::StaleProjection,
            CannotProveReason::CdcGap,
            CannotProveReason::CorruptGeneration,
            CannotProveReason::VersionMismatch,
            CannotProveReason::UnsupportedSemantics,
            CannotProveReason::Overloaded,
            CannotProveReason::InternalInvariantFailure,
        ];

        for (index, left) in reasons.iter().enumerate() {
            for right in &reasons[index + 1..] {
                assert_ne!(left, right);
            }
        }
    }
}
