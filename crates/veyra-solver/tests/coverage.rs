use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_pricing::{MoneyMicros, PricingError};
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile};
use veyra_rules::Rule;
use veyra_solver::{
    HARD_MAX_ROOMS, HARD_MAX_SOLUTIONS, HARD_MAX_STATES, RoomOffer, SolverConfig, SolverError,
    solve,
};

fn date() -> CivilDate {
    CivilDate::new(2026, 9, 21).unwrap_or_else(|_| unreachable!())
}

fn policy(capacity: u16) -> veyra_rule_compiler::CompiledRule {
    compile(
        RULE_SCHEMA_V1,
        &Rule::Capacity {
            min: 1,
            max: capacity,
        },
    )
    .unwrap_or_else(|_| unreachable!())
}

fn party(count: u32) -> BookingParty {
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
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn party_with_intent(strength: ConstraintStrength, relation: RoomingRelation) -> BookingParty {
    let mut builder = BookingParty::builder();
    for id in 1..=2 {
        builder
            .add_traveler(Traveler::new(
                TravelerId::new(id),
                AgeEvidence::AgeAtCheckIn(30),
                false,
            ))
            .unwrap_or_else(|_| unreachable!());
    }
    builder
        .add_rooming_intent(RoomingIntent {
            left: TravelerId::new(1),
            right: TravelerId::new(2),
            strength,
            relation,
        })
        .unwrap_or_else(|_| unreachable!());
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn room(id: u32, price: i64) -> RoomOffer {
    RoomOffer {
        room_id: id,
        projected_price: MoneyMicros::try_nonnegative(price).unwrap_or_else(|_| unreachable!()),
        floor: u16::try_from(id).unwrap_or(u16::MAX),
        building: 1,
        adult_age: 18,
        occupancy_rule: policy(4),
    }
}

#[test]
fn input_bounds_fail_closed() {
    let p = party(1);
    assert_eq!(
        solve(&p, date(), &[], SolverConfig::default()),
        Err(SolverError::InvalidRoomCount(0))
    );
    let rooms = (0..=u32::try_from(HARD_MAX_ROOMS).unwrap_or_default())
        .map(|id| room(id, 1))
        .collect::<Vec<_>>();
    assert!(matches!(
        solve(&p, date(), &rooms, SolverConfig::default()),
        Err(SolverError::InvalidRoomCount(_))
    ));

    for config in [
        SolverConfig {
            max_states: 0,
            max_solutions: 1,
        },
        SolverConfig {
            max_states: HARD_MAX_STATES + 1,
            max_solutions: 1,
        },
        SolverConfig {
            max_states: 1,
            max_solutions: 0,
        },
        SolverConfig {
            max_states: 1,
            max_solutions: HARD_MAX_SOLUTIONS + 1,
        },
    ] {
        assert_eq!(
            solve(&p, date(), &[room(1, 1)], config),
            Err(SolverError::InvalidBudget)
        );
    }

    let mut bad_age = room(1, 1);
    bad_age.adult_age = 0;
    assert_eq!(
        solve(&p, date(), &[bad_age], SolverConfig::default()),
        Err(SolverError::InvalidAdultAge(1))
    );
    let mut negative = room(1, 1);
    negative.projected_price = MoneyMicros::signed(-1);
    assert_eq!(
        solve(&p, date(), &[negative], SolverConfig::default()),
        Err(SolverError::NegativeRoomPrice(1))
    );
    assert_eq!(
        solve(
            &p,
            date(),
            &[room(1, 1), room(1, 2)],
            SolverConfig::default()
        ),
        Err(SolverError::DuplicateRoomId(1))
    );
    assert!(matches!(
        solve(&party(17), date(), &[room(1, 1)], SolverConfig::default()),
        Err(SolverError::TooManyTravelers(17))
    ));
}

#[test]
fn search_budget_solution_limit_and_semantic_unknowns_fail_closed() {
    assert_eq!(
        solve(
            &party(3),
            date(),
            &[room(1, 1), room(2, 1)],
            SolverConfig {
                max_states: 1,
                max_solutions: 10
            }
        ),
        Err(SolverError::StateBudgetExhausted)
    );
    assert!(matches!(
        solve(
            &party(1),
            date(),
            &[room(1, 1), room(2, 1)],
            SolverConfig {
                max_states: 100,
                max_solutions: 1
            }
        ),
        Err(SolverError::TooManyValidSolutions(_))
    ));
    assert_eq!(
        solve(
            &party_with_intent(ConstraintStrength::Must, RoomingRelation::Near),
            date(),
            &[room(1, 1)],
            SolverConfig::default()
        ),
        Err(SolverError::UnsupportedHardConstraint(
            RoomingRelation::Near
        ))
    );
    assert_eq!(
        solve(
            &party_with_intent(ConstraintStrength::Prefer, RoomingRelation::Near),
            date(),
            &[room(1, 1), room(2, 1)],
            SolverConfig::default()
        ),
        Err(SolverError::UnsupportedPreference(RoomingRelation::Near))
    );
}

#[test]
fn checked_money_overflow_propagates_from_solution_cost() {
    let p = party_with_intent(ConstraintStrength::Must, RoomingRelation::SeparateRoom);
    assert_eq!(
        solve(
            &p,
            date(),
            &[room(1, i64::MAX), room(2, i64::MAX)],
            SolverConfig::default()
        ),
        Err(SolverError::Price(PricingError::Overflow))
    );
}

#[test]
fn all_solver_errors_have_stable_display_surface() {
    for error in [
        SolverError::EmptyParty,
        SolverError::TooManyTravelers(17),
        SolverError::InvalidRoomCount(0),
        SolverError::InvalidBudget,
        SolverError::InvalidAdultAge(1),
        SolverError::NegativeRoomPrice(1),
        SolverError::DuplicateRoomId(1),
        SolverError::StateBudgetExhausted,
        SolverError::TooManyValidSolutions(2),
        SolverError::UnsupportedHardConstraint(RoomingRelation::Near),
        SolverError::UnsupportedPreference(RoomingRelation::AdjacentRooms),
        SolverError::UnknownTraveler(TravelerId::new(99)),
        SolverError::Price(PricingError::Overflow),
        SolverError::PenaltyOverflow,
        SolverError::InternalInvariant,
    ] {
        assert!(!error.to_string().is_empty());
    }
}
