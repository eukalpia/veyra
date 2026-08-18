use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use pgwire_replication::{Lsn, ReplicationEvent};
use veyra_cdc::{
    AppliedCheckpoint, DurableTransactionProcessor, LiveEventOutcome, LiveReplicationState,
    TransactionBatch, process_replication_event,
};
use veyra_types::LogSequenceNumber;

static PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "veyra-live-region-{label}-{}-{nanos}-{sequence}.bin",
        std::process::id()
    ))
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

#[test]
fn keepalive_and_clean_stop_never_regress_received_progress() -> Result<(), Box<dyn std::error::Error>> {
    let journal_path = path("keepalive-journal");
    let checkpoint_path = path("keepalive-checkpoint");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let mut state = LiveReplicationState::recovered(LogSequenceNumber::new(50));
    let mut apply = |_batch: &TransactionBatch| -> Result<(), Infallible> { Ok(()) };

    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::KeepAlive {
                wal_end: Lsn::from_u64(40),
                reply_requested: false,
                server_time_micros: 0,
            },
            &mut apply,
        )?,
        LiveEventOutcome::Continue
    );
    assert_eq!(state.progress().received_lsn, LogSequenceNumber::new(50));

    let _ = process_replication_event(
        &mut processor,
        &mut checkpoint,
        &mut state,
        ReplicationEvent::KeepAlive {
            wal_end: Lsn::from_u64(60),
            reply_requested: true,
            server_time_micros: 0,
        },
        &mut apply,
    )?;
    assert_eq!(state.progress().received_lsn, LogSequenceNumber::new(60));

    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::StoppedAt {
                reached: Lsn::from_u64(55),
            },
            &mut apply,
        )?,
        LiveEventOutcome::Stop(LogSequenceNumber::new(55))
    );
    assert_eq!(state.progress().received_lsn, LogSequenceNumber::new(60));

    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
    Ok(())
}

#[test]
fn acknowledgement_never_moves_recovered_progress_backwards() -> Result<(), Box<dyn std::error::Error>> {
    let journal_path = path("ack-journal");
    let checkpoint_path = path("ack-checkpoint");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let mut state = LiveReplicationState::recovered(LogSequenceNumber::new(50));
    let mut apply = |_batch: &TransactionBatch| -> Result<(), Infallible> { Ok(()) };

    let _ = process_replication_event(
        &mut processor,
        &mut checkpoint,
        &mut state,
        ReplicationEvent::Begin {
            final_lsn: Lsn::from_u64(20),
            xid: 7,
            commit_time_micros: 0,
        },
        &mut apply,
    )?;
    let _ = process_replication_event(
        &mut processor,
        &mut checkpoint,
        &mut state,
        ReplicationEvent::XLogData {
            wal_start: Lsn::from_u64(19),
            wal_end: Lsn::from_u64(20),
            server_time_micros: 0,
            data: relation(42).into(),
        },
        &mut apply,
    )?;
    let _ = process_replication_event(
        &mut processor,
        &mut checkpoint,
        &mut state,
        ReplicationEvent::XLogData {
            wal_start: Lsn::from_u64(20),
            wal_end: Lsn::from_u64(20),
            server_time_micros: 0,
            data: insert(42, b"x").into(),
        },
        &mut apply,
    )?;
    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::Commit {
                lsn: Lsn::from_u64(20),
                end_lsn: Lsn::from_u64(21),
                commit_time_micros: 0,
            },
            &mut apply,
        )?,
        LiveEventOutcome::Acknowledge(LogSequenceNumber::new(21))
    );

    let progress = state.progress();
    assert_eq!(progress.received_lsn, LogSequenceNumber::new(50));
    assert_eq!(progress.durable_lsn, LogSequenceNumber::new(50));
    assert_eq!(progress.applied_lsn, LogSequenceNumber::new(50));

    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
    Ok(())
}
