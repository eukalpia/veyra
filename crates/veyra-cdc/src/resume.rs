use core::fmt;

use tokio_postgres::Client;
use veyra_types::LogSequenceNumber;

use crate::snapshot::{SnapshotError, parse_pg_lsn};

/// A checked proof that a logical slot can resume from one exact local LSN.
///
/// Fields are private so callers cannot manufacture a proof without inspecting
/// the current PostgreSQL slot state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplicationStartProof {
    slot: String,
    requested_lsn: LogSequenceNumber,
    restart_lsn: LogSequenceNumber,
    confirmed_flush_lsn: LogSequenceNumber,
}

impl ReplicationStartProof {
    /// Logical replication slot proven by this value.
    #[must_use]
    pub fn slot(&self) -> &str {
        &self.slot
    }

    /// Exact local durable/snapshot boundary Veyra intends to request.
    #[must_use]
    pub const fn requested_lsn(&self) -> LogSequenceNumber {
        self.requested_lsn
    }

    /// Oldest WAL location PostgreSQL currently guarantees for the slot.
    #[must_use]
    pub const fn restart_lsn(&self) -> LogSequenceNumber {
        self.restart_lsn
    }

    /// Slot position PostgreSQL already considers confirmed by the consumer.
    #[must_use]
    pub const fn confirmed_flush_lsn(&self) -> LogSequenceNumber {
        self.confirmed_flush_lsn
    }
}

/// Proves that PostgreSQL still retains every WAL byte required to resume from
/// `requested_lsn`, and that server-side consumer progress has not moved beyond
/// Veyra's local checkpoint.
///
/// This is deliberately conservative. An ambiguous slot state is a fallback,
/// never permission to guess.
pub async fn prove_replication_start(
    client: &Client,
    slot: &str,
    requested_lsn: LogSequenceNumber,
) -> Result<ReplicationStartProof, ResumeFenceError> {
    if slot.is_empty() {
        return Err(ResumeFenceError::EmptySlotName);
    }

    let row = client
        .query_opt(
            "SELECT restart_lsn::text, confirmed_flush_lsn::text, \
                    wal_status::text, active, plugin \
             FROM pg_replication_slots \
             WHERE slot_name = $1 AND slot_type = 'logical'",
            &[&slot],
        )
        .await?
        .ok_or_else(|| ResumeFenceError::SlotNotFound(slot.to_owned()))?;

    let restart_text: Option<String> = row.try_get(0)?;
    let confirmed_text: Option<String> = row.try_get(1)?;
    let wal_status: Option<String> = row.try_get(2)?;
    let active: bool = row.try_get(3)?;
    let plugin: String = row.try_get(4)?;

    validate_resume_state(
        slot,
        requested_lsn,
        restart_text.as_deref(),
        confirmed_text.as_deref(),
        wal_status.as_deref(),
        active,
        &plugin,
    )
}

fn validate_resume_state(
    slot: &str,
    requested_lsn: LogSequenceNumber,
    restart_text: Option<&str>,
    confirmed_text: Option<&str>,
    wal_status: Option<&str>,
    active: bool,
    plugin: &str,
) -> Result<ReplicationStartProof, ResumeFenceError> {
    if plugin != "pgoutput" {
        return Err(ResumeFenceError::UnexpectedPlugin(plugin.to_owned()));
    }
    if active {
        return Err(ResumeFenceError::SlotAlreadyActive(slot.to_owned()));
    }
    match wal_status {
        Some("reserved" | "extended") => {}
        Some(status) => return Err(ResumeFenceError::UnsafeWalStatus(status.to_owned())),
        None => return Err(ResumeFenceError::MissingWalStatus),
    }

    let restart_text = restart_text.ok_or(ResumeFenceError::MissingRestartLsn)?;
    let confirmed_text = confirmed_text.ok_or(ResumeFenceError::MissingConfirmedFlushLsn)?;
    let restart_lsn = parse_pg_lsn(restart_text).map_err(ResumeFenceError::InvalidLsn)?;
    let confirmed_flush_lsn =
        parse_pg_lsn(confirmed_text).map_err(ResumeFenceError::InvalidLsn)?;

    if requested_lsn < restart_lsn {
        return Err(ResumeFenceError::RequestedBeforeRestart {
            requested: requested_lsn,
            restart: restart_lsn,
        });
    }
    if requested_lsn < confirmed_flush_lsn {
        return Err(ResumeFenceError::ServerConfirmedAheadOfLocal {
            requested: requested_lsn,
            confirmed: confirmed_flush_lsn,
        });
    }

    Ok(ReplicationStartProof {
        slot: slot.to_owned(),
        requested_lsn,
        restart_lsn,
        confirmed_flush_lsn,
    })
}

/// Failure to prove gap-free replication resume semantics.
#[derive(Debug)]
pub enum ResumeFenceError {
    Postgres(tokio_postgres::Error),
    InvalidLsn(SnapshotError),
    EmptySlotName,
    SlotNotFound(String),
    UnexpectedPlugin(String),
    SlotAlreadyActive(String),
    MissingWalStatus,
    UnsafeWalStatus(String),
    MissingRestartLsn,
    MissingConfirmedFlushLsn,
    RequestedBeforeRestart {
        requested: LogSequenceNumber,
        restart: LogSequenceNumber,
    },
    ServerConfirmedAheadOfLocal {
        requested: LogSequenceNumber,
        confirmed: LogSequenceNumber,
    },
}

impl From<tokio_postgres::Error> for ResumeFenceError {
    fn from(value: tokio_postgres::Error) -> Self {
        Self::Postgres(value)
    }
}

impl fmt::Display for ResumeFenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Postgres(error) => write!(formatter, "PostgreSQL resume-fence error: {error}"),
            Self::InvalidLsn(error) => write!(formatter, "invalid slot LSN: {error}"),
            Self::EmptySlotName => formatter.write_str("logical replication slot name is empty"),
            Self::SlotNotFound(slot) => write!(formatter, "logical replication slot '{slot}' was not found"),
            Self::UnexpectedPlugin(plugin) => write!(formatter, "logical slot uses unsupported plugin '{plugin}'"),
            Self::SlotAlreadyActive(slot) => write!(formatter, "logical replication slot '{slot}' is already active"),
            Self::MissingWalStatus => formatter.write_str("logical replication slot has no WAL retention status"),
            Self::UnsafeWalStatus(status) => write!(formatter, "logical replication slot WAL status '{status}' cannot prove retained WAL"),
            Self::MissingRestartLsn => formatter.write_str("logical replication slot has no restart LSN"),
            Self::MissingConfirmedFlushLsn => formatter.write_str("logical replication slot has no confirmed flush LSN"),
            Self::RequestedBeforeRestart { requested, restart } => write!(
                formatter,
                "CDC_GAP: requested LSN {} precedes slot restart LSN {}",
                requested.get(),
                restart.get()
            ),
            Self::ServerConfirmedAheadOfLocal { requested, confirmed } => write!(
                formatter,
                "CDC_GAP: server confirmed LSN {} is ahead of local resume LSN {}",
                confirmed.get(),
                requested.get()
            ),
        }
    }
}

impl std::error::Error for ResumeFenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Postgres(error) => Some(error),
            Self::InvalidLsn(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(requested: u64) -> Result<ReplicationStartProof, ResumeFenceError> {
        validate_resume_state(
            "slot",
            LogSequenceNumber::new(requested),
            Some("0/10"),
            Some("0/20"),
            Some("reserved"),
            false,
            "pgoutput",
        )
    }

    #[test]
    fn valid_resume_proof_exposes_exact_slot_state() -> Result<(), ResumeFenceError> {
        let proof = valid(0x20)?;
        assert_eq!(proof.slot(), "slot");
        assert_eq!(proof.requested_lsn(), LogSequenceNumber::new(0x20));
        assert_eq!(proof.restart_lsn(), LogSequenceNumber::new(0x10));
        assert_eq!(proof.confirmed_flush_lsn(), LogSequenceNumber::new(0x20));
        let extended = validate_resume_state(
            "slot",
            LogSequenceNumber::new(0x30),
            Some("0/10"),
            Some("0/20"),
            Some("extended"),
            false,
            "pgoutput",
        )?;
        assert_eq!(extended.requested_lsn(), LogSequenceNumber::new(0x30));
        Ok(())
    }

    #[test]
    fn unknown_or_unsafe_slot_semantics_fail_closed() {
        let cases = [
            validate_resume_state("slot", LogSequenceNumber::new(0x20), Some("0/10"), Some("0/20"), Some("reserved"), false, "test_decoding"),
            validate_resume_state("slot", LogSequenceNumber::new(0x20), Some("0/10"), Some("0/20"), Some("reserved"), true, "pgoutput"),
            validate_resume_state("slot", LogSequenceNumber::new(0x20), Some("0/10"), Some("0/20"), None, false, "pgoutput"),
            validate_resume_state("slot", LogSequenceNumber::new(0x20), Some("0/10"), Some("0/20"), Some("unreserved"), false, "pgoutput"),
            validate_resume_state("slot", LogSequenceNumber::new(0x20), Some("0/10"), Some("0/20"), Some("lost"), false, "pgoutput"),
            validate_resume_state("slot", LogSequenceNumber::new(0x20), None, Some("0/20"), Some("reserved"), false, "pgoutput"),
            validate_resume_state("slot", LogSequenceNumber::new(0x20), Some("0/10"), None, Some("reserved"), false, "pgoutput"),
        ];
        for result in cases {
            assert!(result.is_err());
        }
    }

    #[test]
    fn gaps_and_invalid_lsn_fail_closed() {
        assert!(matches!(
            validate_resume_state("slot", LogSequenceNumber::new(0x0F), Some("0/10"), Some("0/0F"), Some("reserved"), false, "pgoutput"),
            Err(ResumeFenceError::RequestedBeforeRestart { .. })
        ));
        assert!(matches!(
            validate_resume_state("slot", LogSequenceNumber::new(0x1F), Some("0/10"), Some("0/20"), Some("reserved"), false, "pgoutput"),
            Err(ResumeFenceError::ServerConfirmedAheadOfLocal { .. })
        ));
        assert!(matches!(
            validate_resume_state("slot", LogSequenceNumber::new(0x20), Some("bad"), Some("0/20"), Some("reserved"), false, "pgoutput"),
            Err(ResumeFenceError::InvalidLsn(_))
        ));
    }

    #[test]
    fn diagnostics_are_explicit_and_fail_closed() {
        let errors = [
            ResumeFenceError::EmptySlotName,
            ResumeFenceError::SlotNotFound("x".to_owned()),
            ResumeFenceError::UnexpectedPlugin("x".to_owned()),
            ResumeFenceError::SlotAlreadyActive("x".to_owned()),
            ResumeFenceError::MissingWalStatus,
            ResumeFenceError::UnsafeWalStatus("lost".to_owned()),
            ResumeFenceError::MissingRestartLsn,
            ResumeFenceError::MissingConfirmedFlushLsn,
            ResumeFenceError::RequestedBeforeRestart {
                requested: LogSequenceNumber::new(1),
                restart: LogSequenceNumber::new(2),
            },
            ResumeFenceError::ServerConfirmedAheadOfLocal {
                requested: LogSequenceNumber::new(1),
                confirmed: LogSequenceNumber::new(2),
            },
        ];
        for error in errors {
            assert!(!error.to_string().is_empty());
            assert!(std::error::Error::source(&error).is_none());
        }
        let invalid = ResumeFenceError::InvalidLsn(SnapshotError::InvalidLsn("x".to_owned()));
        assert!(std::error::Error::source(&invalid).is_some());
    }
}
