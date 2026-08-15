use core::fmt;

use pgwire_replication::{
    Lsn, PgWireError, ReplicationClient, ReplicationConfig, ReplicationEvent,
};
use veyra_types::LogSequenceNumber;

use crate::assembler::CdcEvent;
use crate::resume::ReplicationStartProof;
use crate::snapshot::parse_pg_lsn;

/// Live `PostgreSQL` logical replication transport.
///
/// Transaction semantics and durability remain owned by Veyra; this wrapper only
/// translates `pgwire-replication` transport events into Veyra types.
pub struct PostgresReplicationStream {
    client: ReplicationClient,
}

impl PostgresReplicationStream {
    /// Connects only with a fresh, checked replication-start proof.
    pub async fn connect(
        config: ReplicationConfig,
        proof: ReplicationStartProof,
    ) -> Result<Self, PostgresCdcError> {
        if config.slot != proof.slot() {
            return Err(PostgresCdcError::ResumeProofSlotMismatch {
                config_slot: config.slot.clone(),
                proof_slot: proof.slot().to_owned(),
            });
        }
        let pg_lsn = to_pg_lsn(proof.requested_lsn())?;
        let client = ReplicationClient::connect(config.with_start_lsn(pg_lsn)).await?;
        Ok(Self { client })
    }

    /// Receives and translates the next replication event.
    pub async fn recv(&mut self) -> Result<Option<CdcEvent>, PostgresCdcError> {
        match self.client.recv().await? {
            Some(event) => Ok(Some(map_event(event)?)),
            None => Ok(None),
        }
    }

    /// Reports only a checkpoint already proven durable by Veyra.
    pub fn acknowledge_durable(
        &self,
        durable_lsn: LogSequenceNumber,
    ) -> Result<(), PostgresCdcError> {
        self.client.update_applied_lsn(to_pg_lsn(durable_lsn)?);
        Ok(())
    }

    /// Requests graceful stream shutdown and waits for `PostgreSQL` cleanup.
    pub async fn shutdown(&mut self) -> Result<(), PostgresCdcError> {
        self.client.shutdown().await?;
        Ok(())
    }

    /// Requests graceful shutdown while allowing buffered events to drain.
    pub fn stop(&self) {
        self.client.stop();
    }
}

fn map_event(event: ReplicationEvent) -> Result<CdcEvent, PostgresCdcError> {
    match event {
        ReplicationEvent::KeepAlive {
            wal_end,
            server_time_micros,
            reply_requested,
        } => Ok(CdcEvent::KeepAlive {
            wal_end: from_pg_lsn(wal_end)?,
            server_time_micros,
            reply_requested,
        }),
        ReplicationEvent::Begin {
            final_lsn,
            xid,
            commit_time_micros,
        } => Ok(CdcEvent::Begin {
            final_lsn: from_pg_lsn(final_lsn)?,
            xid,
            commit_time_micros,
        }),
        ReplicationEvent::XLogData {
            wal_start,
            wal_end,
            server_time_micros,
            data,
        } => Ok(CdcEvent::XLogData {
            wal_start: from_pg_lsn(wal_start)?,
            wal_end: from_pg_lsn(wal_end)?,
            server_time_micros,
            data: data.to_vec(),
        }),
        ReplicationEvent::Commit {
            lsn,
            end_lsn,
            commit_time_micros,
        } => Ok(CdcEvent::Commit {
            lsn: from_pg_lsn(lsn)?,
            end_lsn: from_pg_lsn(end_lsn)?,
            commit_time_micros,
        }),
        ReplicationEvent::Message {
            transactional,
            lsn,
            prefix,
            content,
        } => Ok(CdcEvent::Message {
            transactional,
            lsn: from_pg_lsn(lsn)?,
            prefix,
            content: content.to_vec(),
        }),
        ReplicationEvent::StoppedAt { reached } => Ok(CdcEvent::StoppedAt {
            reached: from_pg_lsn(reached)?,
        }),
    }
}

fn from_pg_lsn(value: Lsn) -> Result<LogSequenceNumber, PostgresCdcError> {
    let text = value.to_string();
    parse_pg_lsn(&text).map_err(|_| PostgresCdcError::InvalidTransportLsn(text))
}

fn to_pg_lsn(value: LogSequenceNumber) -> Result<Lsn, PostgresCdcError> {
    let raw = value.get();
    let text = format!("{:X}/{:X}", raw >> 32, raw & 0xFFFF_FFFF);
    text.parse::<Lsn>()
        .map_err(|_| PostgresCdcError::InvalidTransportLsn(text))
}

/// `PostgreSQL` replication transport failure.
#[derive(Debug)]
pub enum PostgresCdcError {
    Transport(PgWireError),
    InvalidTransportLsn(String),
    ResumeProofSlotMismatch {
        config_slot: String,
        proof_slot: String,
    },
}

impl From<PgWireError> for PostgresCdcError {
    fn from(value: PgWireError) -> Self {
        Self::Transport(value)
    }
}

impl fmt::Display for PostgresCdcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "PostgreSQL replication error: {error}"),
            Self::InvalidTransportLsn(value) => {
                write!(formatter, "invalid replication transport LSN '{value}'")
            }
            Self::ResumeProofSlotMismatch {
                config_slot,
                proof_slot,
            } => write!(
                formatter,
                "replication config slot '{config_slot}' differs from resume proof slot '{proof_slot}'"
            ),
        }
    }
}

impl std::error::Error for PostgresCdcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::InvalidTransportLsn(_) | Self::ResumeProofSlotMismatch { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lsn(value: &str) -> Result<Lsn, Box<dyn std::error::Error>> {
        Ok(value.parse::<Lsn>()?)
    }

    #[test]
    fn lsn_conversion_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        for raw in [0, 1, 0xFFFF_FFFF, 0x16_B374_D848] {
            let veyra = LogSequenceNumber::new(raw);
            assert_eq!(from_pg_lsn(to_pg_lsn(veyra)?)?, veyra);
        }
        Ok(())
    }

    #[test]
    fn transport_events_map_without_semantic_loss() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            map_event(ReplicationEvent::KeepAlive {
                wal_end: lsn("0/10")?,
                server_time_micros: 1,
                reply_requested: true,
            })?,
            CdcEvent::KeepAlive {
                wal_end: LogSequenceNumber::new(0x10),
                server_time_micros: 1,
                reply_requested: true,
            }
        );
        assert_eq!(
            map_event(ReplicationEvent::Begin {
                final_lsn: lsn("0/20")?,
                xid: 7,
                commit_time_micros: 2,
            })?,
            CdcEvent::Begin {
                final_lsn: LogSequenceNumber::new(0x20),
                xid: 7,
                commit_time_micros: 2,
            }
        );
        assert_eq!(
            map_event(ReplicationEvent::XLogData {
                wal_start: lsn("0/18")?,
                wal_end: lsn("0/21")?,
                server_time_micros: 3,
                data: vec![1, 2].into(),
            })?,
            CdcEvent::XLogData {
                wal_start: LogSequenceNumber::new(0x18),
                wal_end: LogSequenceNumber::new(0x21),
                server_time_micros: 3,
                data: vec![1, 2],
            }
        );
        assert_eq!(
            map_event(ReplicationEvent::Commit {
                lsn: lsn("0/20")?,
                end_lsn: lsn("0/22")?,
                commit_time_micros: 2,
            })?,
            CdcEvent::Commit {
                lsn: LogSequenceNumber::new(0x20),
                end_lsn: LogSequenceNumber::new(0x22),
                commit_time_micros: 2,
            }
        );
        assert_eq!(
            map_event(ReplicationEvent::Message {
                transactional: true,
                lsn: lsn("0/19")?,
                prefix: "v".to_owned(),
                content: vec![9].into(),
            })?,
            CdcEvent::Message {
                transactional: true,
                lsn: LogSequenceNumber::new(0x19),
                prefix: "v".to_owned(),
                content: vec![9],
            }
        );
        assert_eq!(
            map_event(ReplicationEvent::StoppedAt {
                reached: lsn("0/30")?
            })?,
            CdcEvent::StoppedAt {
                reached: LogSequenceNumber::new(0x30)
            }
        );
        Ok(())
    }

    #[test]
    fn transport_error_diagnostics_are_stable() {
        let invalid = PostgresCdcError::InvalidTransportLsn("bad".to_owned());
        assert_eq!(
            invalid.to_string(),
            "invalid replication transport LSN 'bad'"
        );
        assert!(std::error::Error::source(&invalid).is_none());
        let mismatch = PostgresCdcError::ResumeProofSlotMismatch {
            config_slot: "a".to_owned(),
            proof_slot: "b".to_owned(),
        };
        assert!(mismatch.to_string().contains("differs from resume proof"));
        assert!(std::error::Error::source(&mismatch).is_none());
    }
}
