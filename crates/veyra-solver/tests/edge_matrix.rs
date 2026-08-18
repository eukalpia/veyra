use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector};
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile};
use veyra_rules::Rule;
use veyra_solver::{
    PricedRoomOffer, RoomRelationIndex, RoomTopologyEdge, RoomTopologyRelation, SolverConfig,
    SolverError, solve_priced, solve_priced_with_topology,
};

fn date() -> CivilDate {
    CivilDate::new(2026, 9, 21).unwrap_or_else(|_| unreachable!())
}

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

fn party(relation: Option<RoomingRelation>) -> BookingParty {
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
    if let Some(relation) = relation {
        builder
            .add_rooming_intent(RoomingIntent {
                left: TravelerId::new(1),
                right: TravelerId::new(2),
                strength: ConstraintStrength::Must,
                relation,
            })
            .unwrap_or_else(|_| unreachable!());
    }
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn room(id: u32, floor: u16, building: u16, capacity: u16) -> PricedRoomOffer {
    PricedRoomOffer {
        room_id: id,
        prices: PriceVector::try_new(10, vec![money(100)], 1).unwrap_or_else(|_| unreachable!()),
        occupancy_adjustment: OccupancyAdjustment {
            per_adult_per_night: money(0),
            per_child_per_night: money(0),
        },
        floor,
        building,
        adult_age: 18,
        occupancy_rule: compile(
            RULE_SCHEMA_V1,
            &Rule::Capacity {
                min: 1,
                max: capacity,
            },
        )
        .unwrap_or_else(|_| unreachable!()),
    }
}

#[test]
fn priced_entry_points_reject_invalid_stays_before_search() {
    let travelers = party(None);
    let offers = [room(10, 1, 1, 2)];
    assert_eq!(
        solve_priced(&travelers, date(), 10, 10, &offers, SolverConfig::default()),
        Err(SolverError::InvalidStayRange)
    );
    let topology = RoomRelationIndex::try_new(&[10], &[]).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        solve_priced_with_topology(
            &travelers,
            date(),
            11,
            10,
            &offers,
            &topology,
            SolverConfig::default(),
        ),
        Err(SolverError::InvalidStayRange)
    );
}

#[test]
fn topology_must_cover_every_offered_room() {
    let topology = RoomRelationIndex::try_new(&[10], &[]).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        solve_priced_with_topology(
            &party(None),
            date(),
            10,
            11,
            &[room(11, 1, 1, 2)],
            &topology,
            SolverConfig::default(),
        ),
        Err(SolverError::TopologyMissingRoom(11))
    );
}

#[test]
fn every_spatial_relation_executes_true_and_false_paths() {
    let offers = [room(10, 1, 1, 1), room(11, 2, 2, 1)];
    let topology = RoomRelationIndex::try_new(
        &[10, 11],
        &[
            RoomTopologyEdge {
                left_room_id: 10,
                right_room_id: 11,
                relation: RoomTopologyRelation::Near,
            },
            RoomTopologyEdge {
                left_room_id: 10,
                right_room_id: 11,
                relation: RoomTopologyRelation::Adjacent,
            },
            RoomTopologyEdge {
                left_room_id: 10,
                right_room_id: 11,
                relation: RoomTopologyRelation::Connected,
            },
        ],
    )
    .unwrap_or_else(|_| unreachable!());

    for relation in [
        RoomingRelation::Near,
        RoomingRelation::AdjacentRooms,
        RoomingRelation::ConnectedRooms,
    ] {
        let result = solve_priced_with_topology(
            &party(Some(relation)),
            date(),
            10,
            11,
            &offers,
            &topology,
            SolverConfig::default(),
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(result.valid_solution_count, 2);
    }

    for relation in [RoomingRelation::SameFloor, RoomingRelation::SameBuilding] {
        let result = solve_priced_with_topology(
            &party(Some(relation)),
            date(),
            10,
            11,
            &offers,
            &topology,
            SolverConfig::default(),
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(result.valid_solution_count, 0);
    }

    let one_room = [room(10, 1, 1, 2)];
    let one_topology = RoomRelationIndex::try_new(&[10], &[]).unwrap_or_else(|_| unreachable!());
    for relation in [
        RoomingRelation::AdjacentRooms,
        RoomingRelation::ConnectedRooms,
    ] {
        let result = solve_priced_with_topology(
            &party(Some(relation)),
            date(),
            10,
            11,
            &one_room,
            &one_topology,
            SolverConfig::default(),
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(result.valid_solution_count, 0);
    }
}
