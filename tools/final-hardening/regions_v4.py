from pathlib import Path


def append_once(path: str, marker: str, block: str) -> None:
    target = Path(path)
    text = target.read_text()
    if marker in text:
        return
    target.write_text(text.rstrip() + "\n\n" + block.strip() + "\n")


append_once(
    "crates/veyra-cdc/src/checkpoint.rs",
    "mod coverage_v4_checkpoint",
    r'''
#[cfg(test)]
mod coverage_v4_checkpoint {
    use super::*;

    fn state(commit: u64, end: u64, fingerprint: u64) -> AppliedState {
        AppliedState {
            commit_lsn: LogSequenceNumber::new(commit),
            end_lsn: LogSequenceNumber::new(end),
            fingerprint,
        }
    }

    #[test]
    fn transition_predicates_cover_every_independent_ordering_path() {
        assert!(matches!(
            validate_transition(state(0, 0, 0), state(11, 10, 1)),
            Err(CheckpointError::EndBeforeCommit)
        ));
        assert!(matches!(
            validate_transition(state(20, 25, 1), state(19, 26, 2)),
            Err(CheckpointError::Regressed)
        ));
        assert!(matches!(
            validate_transition(state(20, 25, 1), state(21, 24, 2)),
            Err(CheckpointError::Regressed)
        ));
        assert!(matches!(
            validate_transition(state(20, 25, 1), state(20, 25, 2)),
            Err(CheckpointError::ConflictingCommit(lsn)) if lsn == LogSequenceNumber::new(20)
        ));
        assert!(validate_transition(state(20, 25, 1), state(20, 25, 1)).is_ok());
        assert!(validate_transition(state(20, 25, 1), state(21, 26, 2)).is_ok());
    }
}
''',
)

append_once(
    "crates/veyra-cdc/src/live.rs",
    "mod coverage_v4_live",
    r'''
#[cfg(test)]
mod coverage_v4_live {
    use super::*;

    #[test]
    fn progress_updates_cover_greater_equal_and_lower_paths_independently() {
        let mut state = LiveReplicationState::recovered(LogSequenceNumber::new(10));

        state.observe_received(Lsn::from_u64(9));
        assert_eq!(state.progress().received_lsn, LogSequenceNumber::new(10));
        state.observe_received(Lsn::from_u64(10));
        assert_eq!(state.progress().received_lsn, LogSequenceNumber::new(10));
        state.observe_received(Lsn::from_u64(11));
        assert_eq!(state.progress().received_lsn, LogSequenceNumber::new(11));

        state.progress = CdcProgress {
            received_lsn: LogSequenceNumber::new(20),
            durable_lsn: LogSequenceNumber::new(10),
            applied_lsn: LogSequenceNumber::new(10),
        };
        assert_eq!(
            state.acknowledge(LogSequenceNumber::new(15)),
            LiveEventOutcome::Acknowledge(LogSequenceNumber::new(15))
        );
        assert_eq!(state.progress.received_lsn, LogSequenceNumber::new(20));
        assert_eq!(state.progress.durable_lsn, LogSequenceNumber::new(15));
        assert_eq!(state.progress.applied_lsn, LogSequenceNumber::new(15));

        state.progress = CdcProgress {
            received_lsn: LogSequenceNumber::new(20),
            durable_lsn: LogSequenceNumber::new(20),
            applied_lsn: LogSequenceNumber::new(10),
        };
        let _ = state.acknowledge(LogSequenceNumber::new(15));
        assert_eq!(state.progress.durable_lsn, LogSequenceNumber::new(20));
        assert_eq!(state.progress.applied_lsn, LogSequenceNumber::new(15));

        state.progress = CdcProgress {
            received_lsn: LogSequenceNumber::new(20),
            durable_lsn: LogSequenceNumber::new(20),
            applied_lsn: LogSequenceNumber::new(20),
        };
        let _ = state.acknowledge(LogSequenceNumber::new(15));
        assert_eq!(state.progress.applied_lsn, LogSequenceNumber::new(20));
    }
}
''',
)

append_once(
    "crates/veyra-cdc/src/journal_v2.rs",
    "mod coverage_v4_journal",
    r'''
#[cfg(test)]
mod coverage_v4_journal {
    use super::*;

    fn batch(commit: u64, fingerprint_byte: u8) -> TransactionBatch {
        TransactionBatch::try_new(
            1,
            LogSequenceNumber::new(commit),
            LogSequenceNumber::new(commit),
            LogSequenceNumber::new(commit + 1),
            vec![RowChange::new(
                7,
                ChangeKind::Insert,
                None,
                Some(vec![fingerprint_byte]),
            )],
        )
        .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn replay_guard_and_tail_cover_both_sides_of_internal_decisions() {
        let mut guard = ReplayGuard::new();
        let first = batch(10, 1);
        assert_eq!(guard.observe(&first), Ok(ReplayDecision::Apply));
        assert_eq!(guard.observe(&first), Ok(ReplayDecision::Duplicate));
        assert!(matches!(
            guard.observe(&batch(10, 2)),
            Err(JournalError::ConflictingReplay(lsn)) if lsn == LogSequenceNumber::new(10)
        ));
        assert_eq!(guard.highest_commit_lsn(), LogSequenceNumber::new(10));

        let records = vec![first.clone()];
        assert_eq!(
            tail(records.clone(), 77, true),
            Ok((records, Some(77)))
        );
        assert!(matches!(
            tail(Vec::new(), 88, false),
            Err(JournalError::IncompleteTail(88))
        ));
    }
}
''',
)

append_once(
    "crates/veyra-query/src/lib.rs",
    "mod coverage_v4_query",
    r'''
#[cfg(test)]
mod coverage_v4_query {
    use super::*;

    #[test]
    fn rejection_classifiers_cover_every_runtime_error_family() {
        for error in [
            RestrictionError::BelowMinStay { nights: 1, minimum: 2 },
            RestrictionError::AboveMaxStay { nights: 6, maximum: 5 },
            RestrictionError::ClosedToArrival(10),
            RestrictionError::ClosedToDeparture(11),
        ] {
            assert!(is_restriction_rejection(error));
        }
        for error in [
            RestrictionError::InvalidStayInterval,
            RestrictionError::StayTooLong,
            RestrictionError::UnsupportedSchema(9),
        ] {
            assert!(!is_restriction_rejection(error));
        }

        for error in [
            OccupancyError::PolicyRejected,
            OccupancyError::MustStayTogether {
                left: veyra_party::TravelerId::new(1),
                right: veyra_party::TravelerId::new(2),
            },
            OccupancyError::MustStaySeparate {
                left: veyra_party::TravelerId::new(1),
                right: veyra_party::TravelerId::new(2),
            },
        ] {
            assert!(is_occupancy_rejection(error));
        }
        assert!(!is_occupancy_rejection(OccupancyError::EmptyRoom));
    }
}
''',
)

append_once(
    "crates/veyra-solver/src/lib.rs",
    "mod coverage_v4_solver",
    r'''
#[cfg(test)]
mod coverage_v4_solver {
    use super::*;

    fn allocation(room_id: u32, traveler: u32) -> RoomAllocation {
        RoomAllocation {
            room_id,
            travelers: vec![TravelerId::new(traveler)],
        }
    }

    fn solution(price: i64, rooms: Vec<RoomAllocation>, penalty: u32) -> StaySolution {
        StaySolution {
            rooms,
            total_price: MoneyMicros::signed(price),
            soft_penalty: penalty,
        }
    }

    #[test]
    fn comparator_tie_breakers_and_best_selection_execute_every_stage() {
        let cheap = solution(10, vec![allocation(1, 1)], 5);
        let expensive = solution(20, vec![allocation(1, 1)], 0);
        assert!(compare_canonical(&cheap, &expensive).is_lt());

        let one_room = solution(10, vec![allocation(1, 1)], 5);
        let two_rooms = solution(10, vec![allocation(1, 1), allocation(2, 2)], 0);
        assert!(compare_fewest_rooms(&one_room, &two_rooms).is_lt());

        let low_penalty = solution(20, vec![allocation(2, 2)], 0);
        let high_penalty = solution(10, vec![allocation(1, 1)], 1);
        assert!(compare_family_layout(&low_penalty, &high_penalty).is_lt());

        let same_price_rooms_low_penalty = solution(10, vec![allocation(1, 1)], 0);
        let same_price_rooms_high_penalty = solution(10, vec![allocation(1, 1)], 1);
        assert!(compare_canonical(
            &same_price_rooms_low_penalty,
            &same_price_rooms_high_penalty
        )
        .is_lt());

        let lower_room_id = solution(10, vec![allocation(1, 1)], 0);
        let higher_room_id = solution(10, vec![allocation(2, 1)], 0);
        assert!(compare_canonical(&lower_room_id, &higher_room_id).is_lt());

        let candidates = vec![expensive.clone(), cheap.clone()];
        assert_eq!(select_best(&candidates, compare_cheapest), cheap);
        let already_best = vec![cheap.clone(), expensive];
        assert_eq!(select_best(&already_best, compare_cheapest), cheap);
    }
}
''',
)

print("region v4 tests staged")
