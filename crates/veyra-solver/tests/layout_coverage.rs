use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_pricing::MoneyMicros;
use veyra_rule_compiler::{CompiledRule, RULE_SCHEMA_V1, compile};
use veyra_rules::Rule;
use veyra_solver::{RoomOffer, SolverConfig, SolverError, solve};

fn date() -> CivilDate {
    CivilDate::new(2026, 9, 21).unwrap_or_else(|_| unreachable!())
}

fn policy(minimum: u16, maximum: u16) -> CompiledRule {
    compile(
        RULE_SCHEMA_V1,
        &Rule::Capacity {
            min: minimum,
            max: maximum,
        },
    )
    .unwrap_or_else(|_| unreachable!())
}

fn room(id: u32, floor: u16, building: u16, occupancy_rule: CompiledRule) -> RoomOffer {
    RoomOffer {
        room_id: id,
        projected_price: MoneyMicros::try_nonnegative(i64::from(id) + 1)
            .unwrap_or_else(|_| unreachable!()),
        floor,
        building,
        adult_age: 18,
        occupancy_rule,
    }
}

fn party(count: u32, intents: &[(ConstraintStrength, RoomingRelation)]) -> BookingParty {
    let mut builder = BookingParty::builder();
    for id in 1..=count {
        builder
            .add_traveler(Traveler::new(
                TravelerId::new(id),
                AgeEvidence::AgeAtCheckIn(30),
                false,
            ))
            .unwrap_or_else(|_| unreachable!());
    }
    for (strength, relation) in intents {
        builder
            .add_rooming_intent(RoomingIntent {
                left: TravelerId::new(1),
                right: TravelerId::new(2),
                strength: *strength,
                relation: *relation,
            })
            .unwrap_or_else(|_| unreachable!());
    }
    builder.build().unwrap_or_else(|_| unreachable!())
}

#[test]
fn no_valid_rooming_returns_a_proven_empty_result() {
    let one = party(1, &[]);
    let result = solve(
        &one,
        date(),
        &[room(1, 1, 1, policy(2, 2))],
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());

    assert!(result.explored_states > 0);
    assert_eq!(result.valid_solution_count, 0);
    assert!(result.solutions.is_empty());
}

#[test]
fn hard_same_floor_and_same_building_constraints_are_proven() {
    let same_floor = party(
        2,
        &[
            (ConstraintStrength::Must, RoomingRelation::SeparateRoom),
            (ConstraintStrength::Must, RoomingRelation::SameFloor),
        ],
    );
    let accepted = solve(
        &same_floor,
        date(),
        &[room(1, 4, 1, policy(1, 2)), room(2, 4, 2, policy(1, 2))],
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());
    assert!(accepted.valid_solution_count > 0);

    let rejected = solve(
        &same_floor,
        date(),
        &[room(1, 4, 1, policy(1, 2)), room(2, 5, 1, policy(1, 2))],
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());
    assert_eq!(rejected.valid_solution_count, 0);

    let same_building = party(
        2,
        &[
            (ConstraintStrength::Must, RoomingRelation::SeparateRoom),
            (ConstraintStrength::Must, RoomingRelation::SameBuilding),
        ],
    );
    let accepted = solve(
        &same_building,
        date(),
        &[room(1, 1, 7, policy(1, 2)), room(2, 2, 7, policy(1, 2))],
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());
    assert!(accepted.valid_solution_count > 0);

    let rejected = solve(
        &same_building,
        date(),
        &[room(1, 1, 7, policy(1, 2)), room(2, 1, 8, policy(1, 2))],
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());
    assert_eq!(rejected.valid_solution_count, 0);
}

#[test]
fn every_supported_soft_relation_executes_prefer_and_avoid_semantics() {
    for relation in [
        RoomingRelation::SameRoom,
        RoomingRelation::SeparateRoom,
        RoomingRelation::SameFloor,
        RoomingRelation::SameBuilding,
    ] {
        for strength in [ConstraintStrength::Prefer, ConstraintStrength::Avoid] {
            let request = party(2, &[(strength, relation)]);
            let result = solve(
                &request,
                date(),
                &[room(1, 1, 1, policy(1, 2)), room(2, 2, 2, policy(1, 2))],
                SolverConfig::default(),
            )
            .unwrap_or_else(|_| unreachable!());
            assert!(result.valid_solution_count > 0);
            assert_eq!(result.solutions.len(), 3);
        }
    }
}

#[test]
fn every_unsupported_soft_topology_fails_closed() {
    for relation in [
        RoomingRelation::Near,
        RoomingRelation::ConnectedRooms,
        RoomingRelation::AdjacentRooms,
    ] {
        let request = party(2, &[(ConstraintStrength::Prefer, relation)]);
        assert_eq!(
            solve(
                &request,
                date(),
                &[room(1, 1, 1, policy(1, 2)), room(2, 2, 2, policy(1, 2)),],
                SolverConfig::default(),
            ),
            Err(SolverError::UnsupportedPreference(relation))
        );
    }
}
