use core::fmt;

use veyra_types::{LogSequenceNumber, ProjectionProgress};

/// Single-writer CDC progress tracker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CdcProgressTracker {
    progress: ProjectionProgress,
}

impl CdcProgressTracker {
    /// Initial zero state.
    pub const ZERO: Self = Self {
        progress: ProjectionProgress::ZERO,
    };

    /// Recovers progress after restart. Received is never reconstructed below durable.
    pub fn recover(
        durable: LogSequenceNumber,
        applied: LogSequenceNumber,
        published: LogSequenceNumber,
    ) -> Result<Self, CdcProgressError> {
        Ok(Self {
            progress: ProjectionProgress::try_new(durable, durable, applied, published)?,
        })
    }

    /// Current validated projection progress.
    #[must_use]
    pub const fn snapshot(self) -> ProjectionProgress {
        self.progress
    }

    /// Records the greatest WAL coordinate observed from PostgreSQL.
    pub fn observe_received(
        &mut self,
        lsn: LogSequenceNumber,
    ) -> Result<(), CdcProgressError> {
        let received = self.progress.received().max(lsn);
        self.progress = ProjectionProgress::try_new(
            received,
            self.progress.durable(),
            self.progress.applied(),
            self.progress.published(),
        )?;
        Ok(())
    }

    /// Advances durable state after local fsync. Duplicate/stale replay is a no-op.
    pub fn mark_durable(&mut self, lsn: LogSequenceNumber) -> Result<(), CdcProgressError> {
        let durable = self.progress.durable().max(lsn);
        let received = self.progress.received().max(durable);
        self.progress = ProjectionProgress::try_new(
            received,
            durable,
            self.progress.applied(),
            self.progress.published(),
        )?;
        Ok(())
    }

    /// Advances applied state only within the durable fence.
    pub fn mark_applied(&mut self, lsn: LogSequenceNumber) -> Result<(), CdcProgressError> {
        if lsn > self.progress.durable() {
            return Err(CdcProgressError::AppliedAheadOfDurable {
                attempted: lsn,
                durable: self.progress.durable(),
            });
        }
        let applied = self.progress.applied().max(lsn);
        self.progress = ProjectionProgress::try_new(
            self.progress.received(),
            self.progress.durable(),
            applied,
            self.progress.published(),
        )?;
        Ok(())
    }

    /// Advances published state only within the applied fence.
    pub fn mark_published(&mut self, lsn: LogSequenceNumber) -> Result<(), CdcProgressError> {
        if lsn > self.progress.applied() {
            return Err(CdcProgressError::PublishedAheadOfApplied {
                attempted: lsn,
                applied: self.progress.applied(),
            });
        }
        let published = self.progress.published().max(lsn);
        self.progress = ProjectionProgress::try_new(
            self.progress.received(),
            self.progress.durable(),
            self.progress.applied(),
            published,
        )?;
        Ok(())
    }
}

/// Invalid CDC progress transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CdcProgressError {
    Projection(veyra_types::ProjectionProgressError),
    AppliedAheadOfDurable {
        attempted: LogSequenceNumber,
        durable: LogSequenceNumber,
    },
    PublishedAheadOfApplied {
        attempted: LogSequenceNumber,
        applied: LogSequenceNumber,
    },
}

impl From<veyra_types::ProjectionProgressError> for CdcProgressError {
    fn from(value: veyra_types::ProjectionProgressError) -> Self {
        Self::Projection(value)
    }
}

impl fmt::Display for CdcProgressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Projection(error) => write!(formatter, "invalid projection progress: {error}"),
            Self::AppliedAheadOfDurable { attempted, durable } => write!(
                formatter,
                "cannot apply LSN {} beyond durable LSN {}",
                attempted.get(),
                durable.get()
            ),
            Self::PublishedAheadOfApplied { attempted, applied } => write!(
                formatter,
                "cannot publish LSN {} beyond applied LSN {}",
                attempted.get(),
                applied.get()
            ),
        }
    }
}

impl std::error::Error for CdcProgressError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Projection(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_preserves_monotonic_chain() -> Result<(), CdcProgressError> {
        let mut tracker = CdcProgressTracker::ZERO;
        tracker.observe_received(LogSequenceNumber::new(10))?;
        tracker.mark_durable(LogSequenceNumber::new(8))?;
        tracker.mark_applied(LogSequenceNumber::new(7))?;
        tracker.mark_published(LogSequenceNumber::new(6))?;
        assert_eq!(
            tracker.snapshot(),
            ProjectionProgress::try_new(
                LogSequenceNumber::new(10),
                LogSequenceNumber::new(8),
                LogSequenceNumber::new(7),
                LogSequenceNumber::new(6),
            )?
        );
        tracker.observe_received(LogSequenceNumber::new(2))?;
        tracker.mark_durable(LogSequenceNumber::new(2))?;
        assert_eq!(tracker.snapshot().received(), LogSequenceNumber::new(10));
        assert_eq!(tracker.snapshot().durable(), LogSequenceNumber::new(8));
        Ok(())
    }

    #[test]
    fn recovery_and_fences_fail_closed() -> Result<(), CdcProgressError> {
        let mut tracker = CdcProgressTracker::recover(
            LogSequenceNumber::new(10),
            LogSequenceNumber::new(8),
            LogSequenceNumber::new(7),
        )?;
        assert_eq!(tracker.snapshot().received(), LogSequenceNumber::new(10));
        assert_eq!(
            tracker.mark_applied(LogSequenceNumber::new(11)),
            Err(CdcProgressError::AppliedAheadOfDurable {
                attempted: LogSequenceNumber::new(11),
                durable: LogSequenceNumber::new(10),
            })
        );
        assert_eq!(
            tracker.mark_published(LogSequenceNumber::new(9)),
            Err(CdcProgressError::PublishedAheadOfApplied {
                attempted: LogSequenceNumber::new(9),
                applied: LogSequenceNumber::new(8),
            })
        );
        Ok(())
    }

    #[test]
    fn diagnostics_and_sources_are_stable() {
        let projection = CdcProgressError::Projection(
            veyra_types::ProjectionProgressError::AppliedAheadOfDurable,
        );
        assert!(projection.to_string().contains("invalid projection progress"));
        assert!(std::error::Error::source(&projection).is_some());
        let applied = CdcProgressError::AppliedAheadOfDurable {
            attempted: LogSequenceNumber::new(2),
            durable: LogSequenceNumber::new(1),
        };
        assert!(applied.to_string().contains("beyond durable"));
        assert!(std::error::Error::source(&applied).is_none());
        let published = CdcProgressError::PublishedAheadOfApplied {
            attempted: LogSequenceNumber::new(2),
            applied: LogSequenceNumber::new(1),
        };
        assert!(published.to_string().contains("beyond applied"));
        assert!(std::error::Error::source(&published).is_none());
    }
}
