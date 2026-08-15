use core::fmt;

use tokio_postgres::{Client, IsolationLevel, Transaction};
use veyra_types::LogSequenceNumber;

/// Conservative snapshot/WAL overlap boundary.
///
/// `replay_from_lsn` is captured from the logical replication slot before the
/// repeatable-read snapshot is opened. Replaying from it may duplicate changes
/// already present in the snapshot, but cannot intentionally create a gap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotBoundary {
    replay_from_lsn: LogSequenceNumber,
    snapshot_lsn: LogSequenceNumber,
    snapshot_id: String,
}

impl SnapshotBoundary {
    /// WAL position from which catch-up must replay after snapshot build.
    #[must_use]
    pub const fn replay_from_lsn(&self) -> LogSequenceNumber {
        self.replay_from_lsn
    }

    /// WAL position observed inside the established snapshot.
    #[must_use]
    pub const fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }

    /// PostgreSQL snapshot identifier, useful for diagnostics and replay fixtures.
    #[must_use]
    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }
}

/// Open repeatable-read, read-only snapshot session.
pub struct SnapshotSession<'a> {
    transaction: Transaction<'a>,
    boundary: SnapshotBoundary,
}

impl<'a> SnapshotSession<'a> {
    /// Gives snapshot builders read-only access to the established transaction.
    #[must_use]
    pub fn transaction(&self) -> &Transaction<'a> {
        &self.transaction
    }

    /// Commits the read-only snapshot after the generation has been built.
    pub async fn finish(self) -> Result<SnapshotBoundary, SnapshotError> {
        self.transaction.commit().await?;
        Ok(self.boundary)
    }

    /// Explicitly rolls back an abandoned snapshot.
    pub async fn rollback(self) -> Result<(), SnapshotError> {
        self.transaction.rollback().await?;
        Ok(())
    }
}

/// Opens a correctness-first initial snapshot.
///
/// The replication slot must already exist. Veyra captures its durable restart
/// boundary first, then starts a repeatable-read read-only transaction. This
/// overlap strategy deliberately chooses possible duplicate replay over a gap.
pub async fn begin_consistent_snapshot<'a>(
    client: &'a mut Client,
    slot: &str,
) -> Result<SnapshotSession<'a>, SnapshotError> {
    if slot.is_empty() {
        return Err(SnapshotError::EmptySlotName);
    }

    let row = client
        .query_opt(
            "SELECT COALESCE(confirmed_flush_lsn, restart_lsn)::text \
             FROM pg_replication_slots \
             WHERE slot_name = $1 AND slot_type = 'logical'",
            &[&slot],
        )
        .await?
        .ok_or_else(|| SnapshotError::SlotNotFound(slot.to_owned()))?;

    let replay_text: Option<String> = row.try_get(0)?;
    let replay_text =
        replay_text.ok_or_else(|| SnapshotError::SlotHasNoRestartLsn(slot.to_owned()))?;
    let replay_from_lsn = parse_pg_lsn(&replay_text)?;

    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await?;

    let snapshot_row = transaction
        .query_one(
            "SELECT pg_current_snapshot()::text, pg_current_wal_lsn()::text",
            &[],
        )
        .await?;
    let snapshot_id: String = snapshot_row.try_get(0)?;
    let snapshot_lsn_text: String = snapshot_row.try_get(1)?;
    let snapshot_lsn = parse_pg_lsn(&snapshot_lsn_text)?;

    Ok(SnapshotSession {
        transaction,
        boundary: SnapshotBoundary {
            replay_from_lsn,
            snapshot_lsn,
            snapshot_id,
        },
    })
}

/// Parses PostgreSQL `X/Y` LSN text into Veyra's canonical `u64`.
pub fn parse_pg_lsn(value: &str) -> Result<LogSequenceNumber, SnapshotError> {
    let (high, low) = value
        .split_once('/')
        .ok_or_else(|| SnapshotError::InvalidLsn(value.to_owned()))?;
    if high.is_empty() || low.is_empty() {
        return Err(SnapshotError::InvalidLsn(value.to_owned()));
    }
    let high =
        u64::from_str_radix(high, 16).map_err(|_| SnapshotError::InvalidLsn(value.to_owned()))?;
    let low =
        u64::from_str_radix(low, 16).map_err(|_| SnapshotError::InvalidLsn(value.to_owned()))?;
    if high > u64::from(u32::MAX) || low > u64::from(u32::MAX) {
        return Err(SnapshotError::InvalidLsn(value.to_owned()));
    }
    Ok(LogSequenceNumber::new((high << 32) | low))
}

/// Initial snapshot failure. Every variant requires PostgreSQL slow-path fallback.
#[derive(Debug)]
pub enum SnapshotError {
    Postgres(tokio_postgres::Error),
    EmptySlotName,
    SlotNotFound(String),
    SlotHasNoRestartLsn(String),
    InvalidLsn(String),
}

impl From<tokio_postgres::Error> for SnapshotError {
    fn from(value: tokio_postgres::Error) -> Self {
        Self::Postgres(value)
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Postgres(error) => write!(formatter, "PostgreSQL snapshot error: {error}"),
            Self::EmptySlotName => formatter.write_str("logical replication slot name is empty"),
            Self::SlotNotFound(slot) => {
                write!(formatter, "logical replication slot '{slot}' was not found")
            }
            Self::SlotHasNoRestartLsn(slot) => {
                write!(
                    formatter,
                    "logical replication slot '{slot}' has no restart LSN"
                )
            }
            Self::InvalidLsn(value) => write!(formatter, "invalid PostgreSQL LSN '{value}'"),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Postgres(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_lsn_parser_is_exact_and_case_insensitive() -> Result<(), SnapshotError> {
        assert_eq!(parse_pg_lsn("0/0")?, LogSequenceNumber::ZERO);
        assert_eq!(
            parse_pg_lsn("16/B374D848")?,
            LogSequenceNumber::new((0x16u64 << 32) | 0xB374_D848)
        );
        assert_eq!(
            parse_pg_lsn("a/ff")?,
            LogSequenceNumber::new((0xAu64 << 32) | 0xFF)
        );
        Ok(())
    }

    #[test]
    fn invalid_lsn_text_fails_closed() {
        for value in [
            "",
            "0",
            "/1",
            "1/",
            "GG/1",
            "1/GG",
            "100000000/0",
            "0/100000000",
        ] {
            assert!(matches!(
                parse_pg_lsn(value),
                Err(SnapshotError::InvalidLsn(_))
            ));
        }
    }

    #[test]
    fn snapshot_error_diagnostics_are_stable() {
        assert_eq!(
            SnapshotError::EmptySlotName.to_string(),
            "logical replication slot name is empty"
        );
        assert_eq!(
            SnapshotError::SlotNotFound("x".to_owned()).to_string(),
            "logical replication slot 'x' was not found"
        );
        assert_eq!(
            SnapshotError::SlotHasNoRestartLsn("x".to_owned()).to_string(),
            "logical replication slot 'x' has no restart LSN"
        );
        assert_eq!(
            SnapshotError::InvalidLsn("x".to_owned()).to_string(),
            "invalid PostgreSQL LSN 'x'"
        );
        assert!(std::error::Error::source(&SnapshotError::InvalidLsn("x".to_owned())).is_none());
    }
}
