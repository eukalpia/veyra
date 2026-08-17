use veyra_occupancy::{OccupancyError, validate_room};
use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_rule_compiler::{CompileError, RULE_SCHEMA_V1, compile};
use veyra_rules::{Rule, RuleError};

fn date(year: i32, month: u8, day: u8) -> CivilDate {
    CivilDate::new(year, month, day).unwrap_or_else(|_| unreachable!())
}

fn policy() -> veyra_rule_compiler::CompiledRule {
    compile(RULE_SCHEMA_V1, &Rule::Capacity { min: 1, max: 4 }).unwrap_or_else(|_| unreachable!())
}

fn two_adults(intent: Option<RoomingIntent>) -> BookingParty {
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
    if let Some(intent) = intent {
        builder
            .add_rooming_intent(intent)
            .unwrap_or_else(|_| unreachable!());
    }
    builder.build().unwrap_or_else(|_| unreachable!())
}

#[test]
fn must_same_room_and_future_birth_date_fail_closed() {
    let party = two_adults(Some(RoomingIntent {
        left: TravelerId::new(1),
        right: TravelerId::new(2),
        strength: ConstraintStrength::Must,
        relation: RoomingRelation::SameRoom,
    }));
    assert!(matches!(
        validate_room(
            &policy(),
            &party,
            date(2026, 1, 1),
            &[TravelerId::new(1)],
            18
        ),
        Err(OccupancyError::MustStayTogether { .. })
    ));

    let mut builder = BookingParty::builder();
    builder
        .add_traveler(Traveler::new(
            TravelerId::new(9),
            AgeEvidence::BirthDate(date(2030, 1, 1)),
            false,
        ))
        .unwrap_or_else(|_| unreachable!());
    let future = builder.build().unwrap_or_else(|_| unreachable!());
    assert_eq!(
        validate_room(
            &policy(),
            &future,
            date(2026, 1, 1),
            &[TravelerId::new(9)],
            18
        ),
        Err(OccupancyError::InvalidAge(TravelerId::new(9)))
    );
}

#[test]
fn soft_rooming_relations_never_become_hard_rejections() {
    for relation in [
        RoomingRelation::SameRoom,
        RoomingRelation::SeparateRoom,
        RoomingRelation::Near,
        RoomingRelation::AdjacentRooms,
        RoomingRelation::ConnectedRooms,
        RoomingRelation::SameFloor,
        RoomingRelation::SameBuilding,
    ] {
        let party = two_adults(Some(RoomingIntent {
            left: TravelerId::new(1),
            right: TravelerId::new(2),
            strength: ConstraintStrength::Prefer,
            relation,
        }));
        assert!(
            validate_room(
                &policy(),
                &party,
                date(2026, 1, 1),
                &[TravelerId::new(1)],
                18
            )
            .is_ok()
        );
    }
}

#[test]
fn every_occupancy_error_has_stable_display() {
    for error in [
        OccupancyError::EmptyRoom,
        OccupancyError::InvalidAdultAge,
        OccupancyError::DuplicateOccupant,
        OccupancyError::UnknownTraveler(TravelerId::new(1)),
        OccupancyError::InvalidAge(TravelerId::new(2)),
        OccupancyError::CountOverflow,
        OccupancyError::MustStayTogether {
            left: TravelerId::new(1),
            right: TravelerId::new(2),
        },
        OccupancyError::MustStaySeparate {
            left: TravelerId::new(1),
            right: TravelerId::new(2),
        },
        OccupancyError::InvalidContext(RuleError::InvalidOccupantCount(0)),
        OccupancyError::CompiledRule(CompileError::RuntimeInvariant),
        OccupancyError::PolicyRejected,
    ] {
        assert!(!error.to_string().is_empty());
    }
}
