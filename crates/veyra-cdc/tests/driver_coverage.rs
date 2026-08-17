use std::cell::Cell;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pgwire_replication::{Lsn, ReplicationEvent};
use veyra_cdc::{LiveEventOutcome, LiveReplicationDriver, TransactionBatch};
use veyra_types::LogSequenceNumber;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ApplyFailure;

impl std::fmt::Display for ApplyFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("apply failed")
    }
}

impl std::error::Error for ApplyFailure {}

fn paths(label: &str) -> (PathBuf, PathBuf) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = format!("veyra-driver-{label}-{}-{nanos}", std::process::id());
    (
        std::env::temp_dir().join(format!("{base}.journal")),
        std::env::temp_dir().join(format!("{base}.checkpoint")),
    )
}

fn relation(relation_id: u32) -> Vec<u8> {
    let mut bytes = vec![b'R'];
    bytes.extend_from_slice(&relation_id.to_be_bytes());
    bytes.extend_from_slice(b"public\0inventory\0");
    bytes.push(b'd');
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes
}

fn insert(relation_id: u32, value: &[u8]) -> Vec<u8> {
    let mut bytes = vec![b'I'];
    bytes.extend_from_slice(&relation_id.to_be_bytes());
    bytes.push(b'N');
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.push(b't');
    bytes.extend_from_slice(&u32::try_from(value.len()).unwrap_or_default().to_be_bytes());
    bytes.extend_from_slice(value);
    bytes
}

fn xlog(wal_start: u64, wal_end: u64, data: Vec<u8>) -> ReplicationEvent {
    ReplicationEvent::XLogData {
        wal_start: Lsn::from_u64(wal_start),
        wal_end: Lsn::from_u64(wal_end),
        server_time_micros: 0,
        data: data.into(),
    }
}

#[test]
fn driver_recovers_processes_acknowledges_stops_and_summarizes() {
    let (journal_path, checkpoint_path) = paths("lifecycle");
    let applied = Cell::new(0_u32);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(applied.get().saturating_add(1));
        Ok(())
    };
    let mut driver = LiveReplicationDriver::open(&journal_path, &checkpoint_path, &mut apply)
        .unwrap_or_else(|_| unreachable!());

    assert_eq!(driver.resume_lsn(), LogSequenceNumber::ZERO);
    assert_eq!(driver.summary().events_seen, 0);
    assert_eq!(driver.summary().acknowledgements, 0);

    assert!(matches!(
        driver.process(
            ReplicationEvent::KeepAlive {
                wal_end: Lsn::from_u64(9),
                reply_requested: false,
                server_time_micros: 0,
            },
            &mut apply,
        ),
        Ok(LiveEventOutcome::Continue)
    ));
    assert!(matches!(
        driver.process(
            ReplicationEvent::Begin {
                final_lsn: Lsn::from_u64(20),
                xid: 7,
                commit_time_micros: 0,
            },
            &mut apply,
        ),
        Ok(LiveEventOutcome::Continue)
    ));
    for data in [relation(11), insert(11, b"x")] {
        assert!(matches!(
            driver.process(xlog(20, 20, data), &mut apply),
            Ok(LiveEventOutcome::Continue)
        ));
    }
    assert!(matches!(
        driver.process(
            ReplicationEvent::Commit {
                lsn: Lsn::from_u64(20),
                end_lsn: Lsn::from_u64(21),
                commit_time_micros: 0,
            },
            &mut apply,
        ),
        Ok(LiveEventOutcome::Acknowledge(lsn)) if lsn == LogSequenceNumber::new(21)
    ));
    assert!(matches!(
        driver.process(
            ReplicationEvent::StoppedAt {
                reached: Lsn::from_u64(22),
            },
            &mut apply,
        ),
        Ok(LiveEventOutcome::Stop(lsn)) if lsn == LogSequenceNumber::new(22)
    ));

    let summary = driver.summary();
    assert_eq!(summary.events_seen, 6);
    assert_eq!(summary.acknowledgements, 1);
    assert_eq!(summary.progress.received_lsn, LogSequenceNumber::new(22));
    assert_eq!(summary.progress.durable_lsn, LogSequenceNumber::new(21));
    assert_eq!(summary.progress.applied_lsn, LogSequenceNumber::new(21));
    assert_eq!(applied.get(), 1);
    drop(driver);

    let recovered_applies = Cell::new(0_u32);
    let mut recover = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        recovered_applies.set(recovered_applies.get().saturating_add(1));
        Ok(())
    };
    let recovered = LiveReplicationDriver::open(&journal_path, &checkpoint_path, &mut recover)
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(recovered.resume_lsn(), LogSequenceNumber::new(21));
    assert_eq!(recovered.summary().events_seen, 0);
    assert_eq!(recovered.summary().acknowledgements, 0);
    assert_eq!(recovered_applies.get(), 0);

    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
}
