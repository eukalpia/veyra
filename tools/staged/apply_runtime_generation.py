from pathlib import Path

path = Path("crates/veyra-runtime/src/lib.rs")
text = path.read_text()

marker = '''impl fmt::Debug for RuntimeState {
'''
block = '''/// One coherent immutable runtime publication containing both status and query payload.
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
    pub fn admit(
        &self,
        minimum_lsn: LogSequenceNumber,
    ) -> Result<Arc<T>, CannotProveReason> {
        let publication = self.snapshot();
        publication.status().prove_queryable(minimum_lsn)?;
        publication
            .payload()
            .cloned()
            .ok_or(CannotProveReason::InternalInvariantFailure)
    }
}

'''
if text.count(marker) != 1:
    raise SystemExit("runtime debug anchor changed")
path.write_text(text.replace(marker, block + marker, 1))
