#![forbid(unsafe_code)]

//! Stable, allocation-free value types shared across Veyra.
//!
//! This crate deliberately owns only semantics that are independent from storage,
//! networking, or PostgreSQL client libraries.

use core::fmt;
use serde::{Deserialize, Serialize};

/// PostgreSQL log sequence number represented as the canonical unsigned 64-bit value.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[repr(transparent)]
pub struct LogSequenceNumber(u64);

impl LogSequenceNumber {
    /// The zero LSN used before any replication state has been observed.
    pub const ZERO: Self = Self(0);

    /// Creates an LSN from its canonical integer representation.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the canonical integer representation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the distance from `older`, or `None` if `older` is ahead of this LSN.
    #[must_use]
    pub const fn checked_distance_from(self, older: Self) -> Option<u64> {
        self.0.checked_sub(older.0)
    }
}

/// Immutable generation identifier.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[repr(transparent)]
pub struct GenerationId(u64);

impl GenerationId {
    /// Generation used before a durable projection has been published.
    pub const UNPUBLISHED: Self = Self(0);

    /// Creates an identifier from a monotonic generation number.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the numeric generation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Monotonic replication progress that is safe to expose to readers.
///
/// The constructor enforces:
///
/// `published <= applied <= durable <= received`
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionProgress {
    received: LogSequenceNumber,
    durable: LogSequenceNumber,
    applied: LogSequenceNumber,
    published: LogSequenceNumber,
}

impl ProjectionProgress {
    /// Initial state before CDC has observed any WAL.
    pub const ZERO: Self = Self {
        received: LogSequenceNumber::ZERO,
        durable: LogSequenceNumber::ZERO,
        applied: LogSequenceNumber::ZERO,
        published: LogSequenceNumber::ZERO,
    };

    /// Creates a progress snapshot where all stages have reached the same LSN.
    #[must_use]
    pub const fn at(lsn: LogSequenceNumber) -> Self {
        Self {
            received: lsn,
            durable: lsn,
            applied: lsn,
            published: lsn,
        }
    }

    /// Constructs progress only when all visibility invariants hold.
    pub const fn try_new(
        received: LogSequenceNumber,
        durable: LogSequenceNumber,
        applied: LogSequenceNumber,
        published: LogSequenceNumber,
    ) -> Result<Self, ProjectionProgressError> {
        if durable.0 > received.0 {
            return Err(ProjectionProgressError::DurableAheadOfReceived);
        }
        if applied.0 > durable.0 {
            return Err(ProjectionProgressError::AppliedAheadOfDurable);
        }
        if published.0 > applied.0 {
            return Err(ProjectionProgressError::PublishedAheadOfApplied);
        }

        Ok(Self {
            received,
            durable,
            applied,
            published,
        })
    }

    /// WAL position received from PostgreSQL.
    #[must_use]
    pub const fn received(self) -> LogSequenceNumber {
        self.received
    }

    /// WAL position durably persisted by Veyra.
    #[must_use]
    pub const fn durable(self) -> LogSequenceNumber {
        self.durable
    }

    /// WAL position fully applied to derived state.
    #[must_use]
    pub const fn applied(self) -> LogSequenceNumber {
        self.applied
    }

    /// WAL position visible from the currently published query state.
    #[must_use]
    pub const fn published(self) -> LogSequenceNumber {
        self.published
    }
}

/// A rejected replication progress transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionProgressError {
    /// Durable state cannot contain WAL that has not been received.
    DurableAheadOfReceived,
    /// Applied state cannot contain WAL that has not been made durable.
    AppliedAheadOfDurable,
    /// Published state cannot expose WAL that has not been fully applied.
    PublishedAheadOfApplied,
}

impl fmt::Display for ProjectionProgressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DurableAheadOfReceived => "durable_lsn is ahead of received_lsn",
            Self::AppliedAheadOfDurable => "applied_lsn is ahead of durable_lsn",
            Self::PublishedAheadOfApplied => "published_lsn is ahead of applied_lsn",
        })
    }
}

impl std::error::Error for ProjectionProgressError {}

/// Explicit compatibility axes. They must never be changed silently.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompatibilityVersions {
    /// On-disk storage format.
    pub storage_format: u16,
    /// Rule schema accepted by the compiler.
    pub rule_schema: u16,
    /// Derived projection schema.
    pub projection: u16,
    /// RPC protocol.
    pub protocol: u16,
    /// Deterministic ranking contract.
    pub ranking: u16,
}

impl CompatibilityVersions {
    /// Compatibility versions for the first public development line.
    pub const CURRENT: Self = Self {
        storage_format: 1,
        rule_schema: 1,
        projection: 1,
        protocol: 1,
        ranking: 1,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn lsn_round_trips_and_computes_distance() {
        let newer = LogSequenceNumber::new(42);
        let older = LogSequenceNumber::new(12);

        assert_eq!(newer.get(), 42);
        assert_eq!(newer.checked_distance_from(older), Some(30));
        assert_eq!(older.checked_distance_from(newer), None);
        assert_eq!(LogSequenceNumber::ZERO.get(), 0);
    }

    #[test]
    fn generation_round_trips() {
        assert_eq!(GenerationId::UNPUBLISHED.get(), 0);
        assert_eq!(GenerationId::new(9).get(), 9);
    }

    #[test]
    fn projection_progress_accepts_monotonic_lsn_chain() {
        let progress = ProjectionProgress::try_new(
            LogSequenceNumber::new(40),
            LogSequenceNumber::new(30),
            LogSequenceNumber::new(20),
            LogSequenceNumber::new(10),
        );

        assert_eq!(
            progress,
            Ok(ProjectionProgress {
                received: LogSequenceNumber::new(40),
                durable: LogSequenceNumber::new(30),
                applied: LogSequenceNumber::new(20),
                published: LogSequenceNumber::new(10),
            })
        );
    }

    #[test]
    fn projection_progress_rejects_durable_ahead_of_received() {
        let error = ProjectionProgress::try_new(
            LogSequenceNumber::new(1),
            LogSequenceNumber::new(2),
            LogSequenceNumber::ZERO,
            LogSequenceNumber::ZERO,
        );

        assert_eq!(
            error,
            Err(ProjectionProgressError::DurableAheadOfReceived)
        );
    }

    #[test]
    fn projection_progress_rejects_applied_ahead_of_durable() {
        let error = ProjectionProgress::try_new(
            LogSequenceNumber::new(3),
            LogSequenceNumber::new(1),
            LogSequenceNumber::new(2),
            LogSequenceNumber::ZERO,
        );

        assert_eq!(error, Err(ProjectionProgressError::AppliedAheadOfDurable));
    }

    #[test]
    fn projection_progress_rejects_published_ahead_of_applied() {
        let error = ProjectionProgress::try_new(
            LogSequenceNumber::new(3),
            LogSequenceNumber::new(2),
            LogSequenceNumber::new(1),
            LogSequenceNumber::new(2),
        );

        assert_eq!(
            error,
            Err(ProjectionProgressError::PublishedAheadOfApplied)
        );
    }

    #[test]
    fn progress_getters_and_zero_are_exact() -> Result<(), ProjectionProgressError> {
        let progress = ProjectionProgress::try_new(
            LogSequenceNumber::new(8),
            LogSequenceNumber::new(7),
            LogSequenceNumber::new(6),
            LogSequenceNumber::new(5),
        )?;

        assert_eq!(progress.received(), LogSequenceNumber::new(8));
        assert_eq!(progress.durable(), LogSequenceNumber::new(7));
        assert_eq!(progress.applied(), LogSequenceNumber::new(6));
        assert_eq!(progress.published(), LogSequenceNumber::new(5));
        assert_eq!(ProjectionProgress::ZERO.received(), LogSequenceNumber::ZERO);
        assert_eq!(
            ProjectionProgress::at(LogSequenceNumber::new(9)),
            ProjectionProgress::try_new(
                LogSequenceNumber::new(9),
                LogSequenceNumber::new(9),
                LogSequenceNumber::new(9),
                LogSequenceNumber::new(9),
            )?
        );
        Ok(())
    }

    #[test]
    fn progress_error_messages_are_stable() {
        assert_eq!(
            ProjectionProgressError::DurableAheadOfReceived.to_string(),
            "durable_lsn is ahead of received_lsn"
        );
        assert_eq!(
            ProjectionProgressError::AppliedAheadOfDurable.to_string(),
            "applied_lsn is ahead of durable_lsn"
        );
        assert_eq!(
            ProjectionProgressError::PublishedAheadOfApplied.to_string(),
            "published_lsn is ahead of applied_lsn"
        );
    }

    #[test]
    fn compatibility_versions_are_explicit() {
        assert_eq!(
            CompatibilityVersions::CURRENT,
            CompatibilityVersions {
                storage_format: 1,
                rule_schema: 1,
                projection: 1,
                protocol: 1,
                ranking: 1,
            }
        );
    }

    proptest! {
        #[test]
        fn any_ordered_chain_is_accepted(
            published in any::<u16>(),
            applied_gap in any::<u16>(),
            durable_gap in any::<u16>(),
            received_gap in any::<u16>(),
        ) {
            let published = u64::from(published);
            let applied = published + u64::from(applied_gap);
            let durable = applied + u64::from(durable_gap);
            let received = durable + u64::from(received_gap);

            let result = ProjectionProgress::try_new(
                LogSequenceNumber::new(received),
                LogSequenceNumber::new(durable),
                LogSequenceNumber::new(applied),
                LogSequenceNumber::new(published),
            );

            prop_assert!(result.is_ok());
        }
    }
}
