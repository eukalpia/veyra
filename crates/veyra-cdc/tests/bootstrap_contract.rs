use veyra_cdc::{
    BootstrapError, BootstrapPhase, BootstrapState, SnapshotAppend, SnapshotDescriptor, SnapshotRow,
    SnapshotSink,
};
use veyra_types::{GenerationId, LogSequenceNumber};

fn lsn(value: u64) -> LogSequenceNumber {
    LogSequenceNumber::new(value)
}

fn descriptor() -> SnapshotDescriptor {
    SnapshotDescriptor::try_new("snap-1", lsn(100), 1, 1, vec![10, 20])
        .unwrap_or_else(|_| unreachable!())
}

#[test]
fn bootstrap_reaches_ready_only_after_snapshot_replay_and_validation() {
    let mut state = BootstrapState::empty();
    state
        .begin_snapshot(descriptor(), GenerationId::new(7))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(state.phase(), BootstrapPhase::Snapshotting);
    state
        .record_snapshot_rows("snap-1", 3)
        .unwrap_or_else(|_| unreachable!());
    state
        .finish_snapshot("snap-1")
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(state.phase(), BootstrapPhase::ReplayingWal);
    state
        .replay_wal("snap-1", lsn(100), lsn(120), lsn(120), lsn(120))
        .unwrap_or_else(|_| unreachable!());
    state
        .begin_validation("snap-1", lsn(120))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(state.phase(), BootstrapPhase::Validating);
    state.mark_ready("snap-1").unwrap_or_else(|_| unreachable!());
    assert_eq!(state.phase(), BootstrapPhase::Ready);
    assert_eq!(state.progress().rows_read(), 3);
    assert_eq!(state.progress().applied_lsn(), lsn(120));
    assert_eq!(state.progress().generation_candidate(), GenerationId::new(7));
}

#[test]
fn bootstrap_rejects_identity_regression_gap_and_premature_publication() {
    let mut state = BootstrapState::empty();
    state
        .begin_snapshot(descriptor(), GenerationId::new(7))
        .unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        state.mark_ready("snap-1"),
        Err(BootstrapError::InvalidPhase { .. })
    ));
    assert!(matches!(
        state.record_snapshot_rows("other", 1),
        Err(BootstrapError::SnapshotMismatch)
    ));
    state
        .finish_snapshot("snap-1")
        .unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        state.replay_wal("snap-1", lsn(101), lsn(120), lsn(120), lsn(120)),
        Err(BootstrapError::WalGap { expected, actual })
            if expected == lsn(100) && actual == lsn(101)
    ));
    state
        .replay_wal("snap-1", lsn(100), lsn(120), lsn(120), lsn(120))
        .unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        state.replay_wal("snap-1", lsn(120), lsn(119), lsn(119), lsn(119)),
        Err(BootstrapError::ProgressRegression)
    ));
    assert!(matches!(
        state.begin_validation("snap-1", lsn(121)),
        Err(BootstrapError::CatchupIncomplete { .. })
    ));
}

#[test]
fn snapshot_sink_is_ordered_bounded_idempotent_and_not_publishable_after_abort() {
    let mut sink = SnapshotSink::begin(descriptor()).unwrap_or_else(|_| unreachable!());
    let first = SnapshotRow::try_new(10, b"a".to_vec(), b"one".to_vec())
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(
        sink.append(first.clone()).unwrap_or_else(|_| unreachable!()),
        SnapshotAppend::Applied
    );
    assert_eq!(
        sink.append(first).unwrap_or_else(|_| unreachable!()),
        SnapshotAppend::Duplicate
    );
    assert!(matches!(
        sink.append(
            SnapshotRow::try_new(10, b"a".to_vec(), b"different".to_vec())
                .unwrap_or_else(|_| unreachable!())
        ),
        Err(BootstrapError::ConflictingSnapshotRow { table_id: 10 })
    ));
    assert!(matches!(
        sink.append(
            SnapshotRow::try_new(20, b"a".to_vec(), b"too-early".to_vec())
                .unwrap_or_else(|_| unreachable!())
        ),
        Err(BootstrapError::UnexpectedSnapshotTable { expected: 10, actual: 20 })
    ));
    sink.append(
        SnapshotRow::try_new(10, b"b".to_vec(), b"two".to_vec())
            .unwrap_or_else(|_| unreachable!()),
    )
    .unwrap_or_else(|_| unreachable!());
    sink.complete_table(10).unwrap_or_else(|_| unreachable!());
    sink.append(
        SnapshotRow::try_new(20, b"a".to_vec(), b"three".to_vec())
            .unwrap_or_else(|_| unreachable!()),
    )
    .unwrap_or_else(|_| unreachable!());
    sink.complete_table(20).unwrap_or_else(|_| unreachable!());
    let completed = sink.finish().unwrap_or_else(|_| unreachable!());
    assert_eq!(completed.replay_after_lsn(), lsn(100));
    assert_eq!(completed.row_count(), 3);

    let mut aborted = SnapshotSink::begin(descriptor()).unwrap_or_else(|_| unreachable!());
    aborted.abort();
    assert!(matches!(aborted.finish(), Err(BootstrapError::SnapshotAborted)));
}

#[test]
fn snapshot_descriptor_rejects_unbounded_and_ambiguous_identity() {
    assert!(matches!(
        SnapshotDescriptor::try_new("", lsn(100), 1, 1, vec![10]),
        Err(BootstrapError::InvalidSnapshotId)
    ));
    assert!(matches!(
        SnapshotDescriptor::try_new("snap", lsn(100), 1, 1, vec![]),
        Err(BootstrapError::InvalidTableSet)
    ));
    assert!(matches!(
        SnapshotDescriptor::try_new("snap", lsn(100), 1, 1, vec![10, 10]),
        Err(BootstrapError::InvalidTableSet)
    ));
    assert!(matches!(
        SnapshotDescriptor::try_new("snap", lsn(100), 1, 1, vec![20, 10]),
        Err(BootstrapError::InvalidTableSet)
    ));
}
