from __future__ import annotations

import sys
from pathlib import Path

SERVER_TEST = '\nuse std::sync::Arc;\n\nuse veyra_availability::AvailabilityIndex;\nuse veyra_party::{\n    AgeEvidence, BookingParty, CivilDate, RoomingRelation, Traveler, TravelerId,\n};\nuse veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector};\nuse veyra_query::{\n    MultiRoomStayQuery, QueryError, RoomDocument, SearchEngine, SolutionProfile, SolverConfig,\n    StayQuery,\n};\nuse veyra_ranking::{RankingKind, RankingProfile};\nuse veyra_restrictions::{\n    RESTRICTION_SCHEMA_V1, RestrictionRule, compile as compile_restrictions,\n};\nuse veyra_rule_compiler::{RULE_SCHEMA_V1, compile as compile_rule};\nuse veyra_rules::Rule;\nuse veyra_runtime::{GenerationState, PublishedGeneration, RuntimeSnapshot};\nuse veyra_server::{QueryService, ServiceQueryError};\nuse veyra_types::{GenerationId, LogSequenceNumber, ProjectionProgress};\n\nfn date() -> CivilDate {\n    CivilDate::new(2026, 9, 21).unwrap_or_else(|_| unreachable!())\n}\n\nfn money(value: i64) -> MoneyMicros {\n    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())\n}\n\nfn party() -> BookingParty {\n    let mut builder = BookingParty::builder();\n    builder\n        .add_traveler(Traveler::new(\n            TravelerId::new(1),\n            AgeEvidence::AgeAtCheckIn(30),\n            false,\n        ))\n        .unwrap_or_else(|_| unreachable!());\n    builder.build().unwrap_or_else(|_| unreachable!())\n}\n\nfn engine() -> SearchEngine {\n    let mut availability = AvailabilityIndex::new(10, 2, 1).unwrap_or_else(|_| unreachable!());\n    for day in 10..12 {\n        availability\n            .set_available(day, 0, true)\n            .unwrap_or_else(|_| unreachable!());\n    }\n    SearchEngine::try_new(\n        availability,\n        vec![RoomDocument {\n            room_id: 0,\n            property_id: 7,\n            destination_id: 55,\n            adult_age: 18,\n            occupancy_rule: compile_rule(RULE_SCHEMA_V1, &Rule::Capacity { min: 1, max: 2 })\n                .unwrap_or_else(|_| unreachable!()),\n            restrictions: compile_restrictions(\n                RESTRICTION_SCHEMA_V1,\n                &[RestrictionRule::MinStay(1), RestrictionRule::MaxStay(2)],\n            )\n            .unwrap_or_else(|_| unreachable!()),\n            prices: PriceVector::try_new(10, vec![money(100), money(100)], 1)\n                .unwrap_or_else(|_| unreachable!()),\n            occupancy_adjustment: OccupancyAdjustment {\n                per_adult_per_night: MoneyMicros::signed(0),\n                per_child_per_night: MoneyMicros::signed(0),\n            },\n            distance_meters: 100,\n            quality_milli: 900,\n            flexibility_milli: 900,\n            family_penalty: 0,\n        }],\n    )\n    .unwrap_or_else(|_| unreachable!())\n}\n\nfn service() -> QueryService {\n    let status = RuntimeSnapshot::ready(\n        GenerationId::new(9),\n        ProjectionProgress::at(LogSequenceNumber::new(55)),\n    );\n    let publication = PublishedGeneration::try_with_payload(status, Arc::new(engine()))\n        .unwrap_or_else(|_| unreachable!());\n    QueryService::new(GenerationState::new(publication))\n}\n\n#[test]\nfn query_service_executes_single_and_multi_room_success_paths() {\n    let party = party();\n    let single = service()\n        .search(\n            LogSequenceNumber::new(55),\n            &StayQuery {\n                destination_id: 55,\n                check_in_day: 10,\n                check_out_day: 11,\n                check_in_date: date(),\n                party: &party,\n                budget: None,\n                ranking: RankingProfile::v1(RankingKind::Cheapest),\n                limit: 10,\n            },\n        )\n        .unwrap_or_else(|_| unreachable!());\n    assert_eq!(single.hits.len(), 1);\n\n    let multi = service()\n        .search_multi_room(\n            LogSequenceNumber::new(55),\n            &MultiRoomStayQuery {\n                destination_id: 55,\n                check_in_day: 10,\n                check_out_day: 11,\n                check_in_date: date(),\n                party: &party,\n                budget: None,\n                profile: SolutionProfile::Cheapest,\n                solver: SolverConfig::default(),\n                limit: 10,\n            },\n        )\n        .unwrap_or_else(|_| unreachable!());\n    assert_eq!(multi.hits.len(), 1);\n}\n\n#[test]\nfn admitted_query_rejections_keep_stable_machine_and_display_surfaces() {\n    let party = party();\n    let error = service()\n        .search(\n            LogSequenceNumber::new(55),\n            &StayQuery {\n                destination_id: 55,\n                check_in_day: 10,\n                check_out_day: 11,\n                check_in_date: date(),\n                party: &party,\n                budget: None,\n                ranking: RankingProfile::v1(RankingKind::Cheapest),\n                limit: 0,\n            },\n        )\n        .unwrap_err();\n    assert_eq!(error.code(), "query_rejected");\n    assert!(error.to_string().contains("query_rejected"));\n    assert_eq!(error, ServiceQueryError::Query(QueryError::InvalidLimit(0)));\n\n    let semantic =\n        ServiceQueryError::Query(QueryError::MissingRoomTopology(RoomingRelation::ConnectedRooms));\n    assert_eq!(semantic.code(), "unsupported_semantics");\n    assert!(semantic.to_string().contains("unsupported_semantics"));\n}\n'
SOLVER_MODULE = '\n#[cfg(test)]\nmod private_region_coverage {\n    use super::*;\n    use veyra_party::{AgeEvidence, RoomingIntent, Traveler};\n    use veyra_rule_compiler::{RULE_SCHEMA_V1, compile};\n    use veyra_rules::Rule;\n\n    fn date() -> CivilDate {\n        CivilDate::new(2026, 9, 21).unwrap_or_else(|_| unreachable!())\n    }\n\n    fn rule(min: u16, max: u16) -> CompiledRule {\n        compile(RULE_SCHEMA_V1, &Rule::Capacity { min, max })\n            .unwrap_or_else(|_| unreachable!())\n    }\n\n    fn party_with_intent(\n        left: TravelerId,\n        right: TravelerId,\n        strength: ConstraintStrength,\n        relation: RoomingRelation,\n    ) -> BookingParty {\n        let mut builder = BookingParty::builder();\n        for id in 1..=2 {\n            builder\n                .add_traveler(Traveler::new(\n                    TravelerId::new(id),\n                    AgeEvidence::AgeAtCheckIn(30),\n                    false,\n                ))\n                .unwrap_or_else(|_| unreachable!());\n        }\n        builder\n            .add_rooming_intent(RoomingIntent {\n                left,\n                right,\n                strength,\n                relation,\n            })\n            .unwrap_or_else(|_| unreachable!());\n        builder.build().unwrap_or_else(|_| unreachable!())\n    }\n\n    fn single_party() -> BookingParty {\n        let mut builder = BookingParty::builder();\n        builder\n            .add_traveler(Traveler::new(\n                TravelerId::new(1),\n                AgeEvidence::AgeAtCheckIn(30),\n                false,\n            ))\n            .unwrap_or_else(|_| unreachable!());\n        builder.build().unwrap_or_else(|_| unreachable!())\n    }\n\n    fn prepared<\'a>(\n        room_id: u32,\n        floor: u16,\n        building: u16,\n        policy: &\'a CompiledRule,\n        price: i64,\n    ) -> PreparedOffer<\'a> {\n        PreparedOffer {\n            room_id,\n            floor,\n            building,\n            adult_age: 18,\n            occupancy_rule: policy,\n            price: PriceSource::Static(MoneyMicros::signed(price)),\n        }\n    }\n\n    #[test]\n    fn hard_constraint_and_assignment_truth_table() {\n        let policy_a = rule(1, 2);\n        let policy_b = rule(1, 2);\n        let party = party_with_intent(\n            TravelerId::new(1),\n            TravelerId::new(2),\n            ConstraintStrength::Must,\n            RoomingRelation::SameRoom,\n        );\n        let mut context = SearchContext {\n            party: &party,\n            check_in: date(),\n            offers: vec![\n                prepared(10, 1, 1, &policy_a, 100),\n                prepared(11, 2, 2, &policy_b, 100),\n            ],\n            topology: None,\n            config: SolverConfig::default(),\n            travelers: vec![TravelerId::new(1), TravelerId::new(2)],\n            assignments: vec![0, 1],\n            explored_states: 0,\n            valid: Vec::new(),\n        };\n\n        assert_eq!(layout_hard_constraints_hold(&context), Ok(false));\n        assert_eq!(partial_hard_constraints_hold(&context), Ok(false));\n        assert_eq!(\n            relation_satisfied(&context, 0, 0, RoomingRelation::SameRoom),\n            Ok(true)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 1, RoomingRelation::SameRoom),\n            Ok(false)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 1, RoomingRelation::SeparateRoom),\n            Ok(true)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 0, RoomingRelation::SeparateRoom),\n            Ok(false)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 1, RoomingRelation::SameFloor),\n            Ok(false)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 1, RoomingRelation::SameBuilding),\n            Ok(false)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 0, RoomingRelation::Near),\n            Ok(true)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 1, RoomingRelation::Near),\n            Err(SolverError::InternalInvariant)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 0, RoomingRelation::AdjacentRooms),\n            Ok(false)\n        );\n        assert_eq!(\n            relation_satisfied(&context, 0, 0, RoomingRelation::ConnectedRooms),\n            Ok(false)\n        );\n        assert_eq!(\n            assignment_of(&context, TravelerId::new(99)),\n            Err(SolverError::UnknownTraveler(TravelerId::new(99)))\n        );\n        context.assignments = vec![0];\n        assert_eq!(\n            assignment_of(&context, TravelerId::new(2)),\n            Err(SolverError::InternalInvariant)\n        );\n        assert_eq!(partial_hard_constraints_hold(&context), Ok(true));\n    }\n\n    #[test]\n    fn leaf_paths_cover_rejection_unused_rooms_and_overflow() {\n        let reject = rule(2, 2);\n        let party = single_party();\n        let mut rejected = SearchContext {\n            party: &party,\n            check_in: date(),\n            offers: vec![prepared(10, 1, 1, &reject, 100)],\n            topology: None,\n            config: SolverConfig::default(),\n            travelers: vec![TravelerId::new(1)],\n            assignments: vec![0],\n            explored_states: 0,\n            valid: Vec::new(),\n        };\n        assert_eq!(evaluate_leaf(&mut rejected), Ok(()));\n        assert!(rejected.valid.is_empty());\n\n        let valid_a = rule(1, 1);\n        let valid_b = rule(1, 1);\n        let mut unused = SearchContext {\n            party: &party,\n            check_in: date(),\n            offers: vec![\n                prepared(10, 1, 1, &valid_a, 100),\n                prepared(11, 1, 1, &valid_b, 100),\n            ],\n            topology: None,\n            config: SolverConfig::default(),\n            travelers: vec![TravelerId::new(1)],\n            assignments: vec![0],\n            explored_states: 0,\n            valid: Vec::new(),\n        };\n        assert_eq!(evaluate_leaf(&mut unused), Ok(()));\n        assert_eq!(unused.valid.len(), 1);\n\n        let two = party_with_intent(\n            TravelerId::new(1),\n            TravelerId::new(2),\n            ConstraintStrength::Must,\n            RoomingRelation::SeparateRoom,\n        );\n        let mut overflow = SearchContext {\n            party: &two,\n            check_in: date(),\n            offers: vec![\n                prepared(10, 1, 1, &valid_a, i64::MAX),\n                prepared(11, 1, 1, &valid_b, i64::MAX),\n            ],\n            topology: None,\n            config: SolverConfig::default(),\n            travelers: vec![TravelerId::new(1), TravelerId::new(2)],\n            assignments: vec![0, 1],\n            explored_states: 0,\n            valid: Vec::new(),\n        };\n        assert!(matches!(\n            evaluate_leaf(&mut overflow),\n            Err(SolverError::Price(_))\n        ));\n    }\n\n    #[test]\n    fn soft_penalty_prefer_and_avoid_truth_table() {\n        let policy_a = rule(1, 2);\n        let policy_b = rule(1, 2);\n        for (strength, assignments, expected) in [\n            (ConstraintStrength::Prefer, vec![0, 1], 1),\n            (ConstraintStrength::Prefer, vec![0, 0], 0),\n            (ConstraintStrength::Avoid, vec![0, 0], 1),\n            (ConstraintStrength::Avoid, vec![0, 1], 0),\n        ] {\n            let party = party_with_intent(\n                TravelerId::new(1),\n                TravelerId::new(2),\n                strength,\n                RoomingRelation::SameRoom,\n            );\n            let context = SearchContext {\n                party: &party,\n                check_in: date(),\n                offers: vec![\n                    prepared(10, 1, 1, &policy_a, 100),\n                    prepared(11, 1, 1, &policy_b, 100),\n                ],\n                topology: None,\n                config: SolverConfig::default(),\n                travelers: vec![TravelerId::new(1), TravelerId::new(2)],\n                assignments,\n                explored_states: 0,\n                valid: Vec::new(),\n            };\n            assert_eq!(layout_soft_penalty(&context), Ok(expected));\n        }\n    }\n}\n'
CHECKPOINT_MODULE = '\n#[cfg(test)]\nmod private_transition_coverage {\n    use super::*;\n\n    const fn state(commit: u64, end: u64, fingerprint: u64) -> AppliedState {\n        AppliedState {\n            commit_lsn: LogSequenceNumber::new(commit),\n            end_lsn: LogSequenceNumber::new(end),\n            fingerprint,\n        }\n    }\n\n    #[test]\n    fn transition_boolean_regions_are_independent() {\n        assert_eq!(\n            validate_transition(AppliedState::default(), state(10, 11, 1)),\n            Ok(())\n        );\n        assert!(matches!(\n            validate_transition(AppliedState::default(), state(10, 9, 1)),\n            Err(CheckpointError::EndBeforeCommit)\n        ));\n        let current = state(10, 20, 1);\n        assert!(matches!(\n            validate_transition(current, state(9, 20, 1)),\n            Err(CheckpointError::Regressed)\n        ));\n        assert!(matches!(\n            validate_transition(current, state(11, 19, 1)),\n            Err(CheckpointError::Regressed)\n        ));\n        assert!(matches!(\n            validate_transition(current, state(10, 20, 2)),\n            Err(CheckpointError::ConflictingCommit(_))\n        ));\n        assert_eq!(validate_transition(current, current), Ok(()));\n    }\n}\n'
JOURNAL_MODULE = '\n#[cfg(test)]\nmod private_journal_coverage {\n    use super::*;\n    use std::time::{SystemTime, UNIX_EPOCH};\n\n    fn path(label: &str) -> PathBuf {\n        let nanos = SystemTime::now()\n            .duration_since(UNIX_EPOCH)\n            .unwrap_or_default()\n            .as_nanos();\n        std::env::temp_dir().join(format!(\n            "veyra-journal-private-{label}-{}-{nanos}.bin",\n            std::process::id()\n        ))\n    }\n\n    fn batch(commit: u64, value: u8) -> TransactionBatch {\n        TransactionBatch::try_new(\n            7,\n            LogSequenceNumber::new(commit),\n            LogSequenceNumber::new(commit),\n            LogSequenceNumber::new(commit + 1),\n            vec![RowChange::new(\n                9,\n                ChangeKind::Insert,\n                None,\n                Some(vec![value]),\n            )],\n        )\n        .unwrap_or_else(|_| unreachable!())\n    }\n\n    fn write_record(\n        path: &Path,\n        batch: &TransactionBatch,\n        commit_lsn: u64,\n        fingerprint: u64,\n    ) -> Result<(), Box<dyn std::error::Error>> {\n        let payload = encode_batch(batch)?;\n        let payload_len = u64::try_from(payload.len()).unwrap_or_else(|_| unreachable!());\n        let mut header = [0_u8; HEADER_LEN];\n        header[0..4].copy_from_slice(&MAGIC);\n        header[4..6].copy_from_slice(&VERSION.to_le_bytes());\n        header[8..16].copy_from_slice(&payload_len.to_le_bytes());\n        header[16..24].copy_from_slice(&commit_lsn.to_le_bytes());\n        header[24..32].copy_from_slice(&fingerprint.to_le_bytes());\n        let mut bytes = Vec::new();\n        bytes.extend_from_slice(&header);\n        bytes.extend_from_slice(&payload);\n        bytes.extend_from_slice(&crc32c(&payload).to_le_bytes());\n        std::fs::write(path, bytes)?;\n        Ok(())\n    }\n\n    #[test]\n    fn io_header_tail_and_guard_regions_are_fail_closed() -> Result<(), Box<dyn std::error::Error>> {\n        let holder = path("holder");\n        std::fs::write(&holder, b"")?;\n        let readonly = File::open(&holder)?;\n        let mut journal = Journal {\n            path: holder.clone(),\n            file: readonly,\n            guard: ReplayGuard::new(),\n        };\n        assert!(matches!(\n            journal.append(&batch(10, 1)),\n            Err(JournalError::Io(_))\n        ));\n\n        let missing = path("missing");\n        let holder_file = File::open(&holder)?;\n        let mut missing_journal = Journal {\n            path: missing,\n            file: holder_file,\n            guard: ReplayGuard::new(),\n        };\n        assert!(matches!(\n            missing_journal.replay(),\n            Err(JournalError::Io(_))\n        ));\n        assert!(matches!(\n            missing_journal.recover(),\n            Err(JournalError::Io(_))\n        ));\n\n        let source = batch(20, 2);\n        let commit_bad = path("commit-bad");\n        write_record(&commit_bad, &source, 19, source.fingerprint())?;\n        let mut file = File::open(&commit_bad)?;\n        assert!(matches!(\n            scan(&mut file, false),\n            Err(JournalError::HeaderPayloadMismatch(0))\n        ));\n\n        let fingerprint_bad = path("fingerprint-bad");\n        write_record(\n            &fingerprint_bad,\n            &source,\n            source.commit_lsn().get(),\n            source.fingerprint().wrapping_add(1),\n        )?;\n        let mut file = File::open(&fingerprint_bad)?;\n        assert!(matches!(\n            scan(&mut file, false),\n            Err(JournalError::HeaderPayloadMismatch(0))\n        ));\n\n        assert!(matches!(\n            tail(Vec::new(), 7, false),\n            Err(JournalError::IncompleteTail(7))\n        ));\n        assert_eq!(tail(Vec::new(), 7, true)?, (Vec::new(), Some(7)));\n\n        let mut guard = ReplayGuard::new();\n        assert_eq!(guard.highest_commit_lsn(), LogSequenceNumber::ZERO);\n        guard.observe(&source)?;\n        assert_eq!(guard.highest_commit_lsn(), source.commit_lsn());\n\n        for candidate in [holder, commit_bad, fingerprint_bad] {\n            let _ = std::fs::remove_file(candidate);\n        }\n        Ok(())\n    }\n}\n'
SEGMENT_MODULE = '\n#[cfg(test)]\nmod private_segment_coverage {\n    use super::*;\n    use std::time::{SystemTime, UNIX_EPOCH};\n\n    fn path(label: &str) -> PathBuf {\n        let nanos = SystemTime::now()\n            .duration_since(UNIX_EPOCH)\n            .unwrap_or_default()\n            .as_nanos();\n        std::env::temp_dir().join(format!(\n            "veyra-segment-private-{label}-{}-{nanos}.bin",\n            std::process::id()\n        ))\n    }\n\n    #[test]\n    fn path_and_parent_sync_boundaries_are_explicit() -> Result<(), Box<dyn std::error::Error>> {\n        assert!(matches!(\n            temp_path(Path::new("")),\n            Err(SegmentError::MissingFileName)\n        ));\n        let target = path("normal");\n        let temp = temp_path(&target)?;\n        assert_ne!(temp, target);\n        sync_parent(&target)?;\n        Ok(())\n    }\n}\n'

def transform() -> None:
    query = Path("crates/veyra-query/src/lib.rs")
    text = query.read_text()

    single_budget_old = """            if query.budget.is_some_and(|budget| projected.total > budget) {
                continue;
            }
"""
    single_budget_new = """            if let Some(budget) = query.budget
                && projected.total > budget
            {
                continue;
            }
"""
    if single_budget_new not in text:
        if text.count(single_budget_old) != 1:
            raise SystemExit("single-room budget site not unique")
        text = text.replace(single_budget_old, single_budget_new, 1)

    negative_old = """    if query.budget.is_some_and(|budget| budget.get() < 0) {
        return Err(QueryError::NegativeBudget);
    }
"""
    negative_new = """    if let Some(budget) = query.budget
        && budget.get() < 0
    {
        return Err(QueryError::NegativeBudget);
    }
"""
    if negative_old in text:
        text = text.replace(negative_old, negative_new)

    replacements = [
        (
            """    if party_size == 0 || party_size > MAX_QUERY_PARTY {
        return Err(QueryError::PartyTooLarge(party_size));
    }
""",
            """    if party_size > MAX_QUERY_PARTY {
        return Err(QueryError::PartyTooLarge(party_size));
    }
""",
            "single party bound",
        ),
        (
            """    if party_size == 0 || party_size > HARD_MAX_TRAVELERS {
        return Err(QueryError::SolverPartyTooLarge(party_size));
    }
""",
            """    if party_size > HARD_MAX_TRAVELERS {
        return Err(QueryError::SolverPartyTooLarge(party_size));
    }
""",
            "multi party bound",
        ),
        (
            """        if query
            .budget
            .is_some_and(|budget| tagged.solution.total_price > budget)
        {
            explain.budget_rejected_properties += 1;
            continue;
        }
""",
            """        if let Some(budget) = query.budget
            && tagged.solution.total_price > budget
        {
            explain.budget_rejected_properties += 1;
            continue;
        }
""",
            "multi-room budget",
        ),
    ]
    for old, new, label in replacements:
        if new not in text:
            if text.count(old) != 1:
                raise SystemExit(f"{label} site not unique")
            text = text.replace(old, new, 1)

    old_compare = """fn compare_multi_room_hits(
    left: &MultiRoomSearchHit,
    right: &MultiRoomSearchHit,
    profile: SolutionProfile,
) -> Ordering {
    let profile_order = match profile {
        SolutionProfile::Cheapest => left
            .projected_price
            .cmp(&right.projected_price)
            .then_with(|| left.rooms.len().cmp(&right.rooms.len()))
            .then_with(|| left.soft_penalty.cmp(&right.soft_penalty)),
        SolutionProfile::FewestRooms => left
            .rooms
            .len()
            .cmp(&right.rooms.len())
            .then_with(|| left.projected_price.cmp(&right.projected_price))
            .then_with(|| left.soft_penalty.cmp(&right.soft_penalty)),
        SolutionProfile::BestFamilyLayout => left
            .soft_penalty
            .cmp(&right.soft_penalty)
            .then_with(|| left.projected_price.cmp(&right.projected_price))
            .then_with(|| left.rooms.len().cmp(&right.rooms.len())),
    };
    profile_order
        .then_with(|| left.property_id.cmp(&right.property_id))
        .then_with(|| left.rooms.cmp(&right.rooms))
}
"""
    new_compare = """fn compare_multi_room_hits(
    left: &MultiRoomSearchHit,
    right: &MultiRoomSearchHit,
    profile: SolutionProfile,
) -> Ordering {
    match profile {
        SolutionProfile::Cheapest => (
            left.projected_price,
            left.rooms.len(),
            left.soft_penalty,
            left.property_id,
            &left.rooms,
        )
            .cmp(&(
                right.projected_price,
                right.rooms.len(),
                right.soft_penalty,
                right.property_id,
                &right.rooms,
            )),
        SolutionProfile::FewestRooms => (
            left.rooms.len(),
            left.projected_price,
            left.soft_penalty,
            left.property_id,
            &left.rooms,
        )
            .cmp(&(
                right.rooms.len(),
                right.projected_price,
                right.soft_penalty,
                right.property_id,
                &right.rooms,
            )),
        SolutionProfile::BestFamilyLayout => (
            left.soft_penalty,
            left.projected_price,
            left.rooms.len(),
            left.property_id,
            &left.rooms,
        )
            .cmp(&(
                right.soft_penalty,
                right.projected_price,
                right.rooms.len(),
                right.property_id,
                &right.rooms,
            )),
    }
}
"""
    if new_compare not in text:
        if text.count(old_compare) != 1:
            raise SystemExit("query comparator site not unique")
        text = text.replace(old_compare, new_compare, 1)
    query.write_text(text)

    cargo = Path("crates/veyra-server/Cargo.toml")
    text = cargo.read_text()
    if "veyra-availability = " not in text:
        marker = "[dev-dependencies]\n"
        if marker not in text:
            raise SystemExit("server dev-dependencies marker missing")
        additions = """veyra-availability = { version = "0.1.0", path = "../veyra-availability" }
veyra-pricing = { version = "0.1.0", path = "../veyra-pricing" }
veyra-ranking = { version = "0.1.0", path = "../veyra-ranking" }
veyra-restrictions = { version = "0.1.0", path = "../veyra-restrictions" }
veyra-rule-compiler = { version = "0.1.0", path = "../veyra-rule-compiler" }
veyra-rules = { version = "0.1.0", path = "../veyra-rules" }
"""
        cargo.write_text(text.replace(marker, marker + additions, 1))

def lifetime() -> None:
    path = Path("crates/veyra-query/tests/multi_room_matrix.rs")
    text = path.read_text()
    old = """fn query(party: &BookingParty, profile: SolutionProfile) -> MultiRoomStayQuery<'_> {
"""
    new = """fn query<'a>(party: &'a BookingParty, profile: SolutionProfile) -> MultiRoomStayQuery<'a> {
"""
    if new not in text:
        if text.count(old) != 1:
            raise SystemExit("multi-room query helper site not unique")
        path.write_text(text.replace(old, new, 1))

def append_once(path: Path, marker: str, block: str) -> None:
    text = path.read_text()
    if marker not in text:
        path.write_text(text.rstrip() + "\n\n" + block.strip() + "\n")

def tests() -> None:
    Path("crates/veyra-server/tests/query_success_matrix.rs").write_text(SERVER_TEST)
    append_once(Path("crates/veyra-solver/src/lib.rs"), "mod private_region_coverage", SOLVER_MODULE)
    append_once(Path("crates/veyra-cdc/src/checkpoint.rs"), "mod private_transition_coverage", CHECKPOINT_MODULE)
    append_once(Path("crates/veyra-cdc/src/journal_v2.rs"), "mod private_journal_coverage", JOURNAL_MODULE)
    append_once(Path("crates/veyra-segment/src/lib.rs"), "mod private_segment_coverage", SEGMENT_MODULE)

ACTIONS = {
    "transform": transform,
    "lifetime": lifetime,
    "tests": tests,
}

if len(sys.argv) != 2 or sys.argv[1] not in ACTIONS:
    raise SystemExit("usage: apply.py transform|lifetime|tests")
ACTIONS[sys.argv[1]]()
