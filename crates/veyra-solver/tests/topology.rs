use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector};
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile};
use veyra_rules::Rule;
use veyra_solver::{
    PricedRoomOffer, RoomRelationIndex, RoomTopologyEdge, RoomTopologyRelation, SolverConfig,
    TopologyError, solve_priced_with_topology,
};

fn date(y: i32, m: u8, d: u8) -> CivilDate {
    CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
}

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

fn party(relations: &[RoomingRelation]) -> BookingParty {
    let mut builder = BookingParty::builder();
    for traveler in [
        Traveler::new(TravelerId::new(1), AgeEvidence::AgeAtCheckIn(40), false),
        Traveler::new(TravelerId::new(2), AgeEvidence::AgeAtCheckIn(38), false),
    ] {
        builder
            .add_traveler(traveler)
            .unwrap_or_else(|_| unreachable!());
    }
    for &relation in relations {
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

fn policy(capacity: u16) -> veyra_rule_compiler::CompiledRule {
    compile(
        RULE_SCHEMA_V1,
        &Rule::And(vec![
            Rule::Capacity {
                min: 1,
                max: capacity,
            },
            Rule::RequireAdult {
                adult_age: 18,
                min_adults: 1,
            },
        ]),
    )
    .unwrap_or_else(|_| unreachable!())
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
        occupancy_rule: policy(capacity),
    }
}

#[test]
fn explicit_connected_edge_makes_connected_rooms_constraint_provable() {
    let topology = RoomRelationIndex::try_new(
        &[10, 11],
        &[RoomTopologyEdge {
            left_room_id: 10,
            right_room_id: 11,
            relation: RoomTopologyRelation::Connected,
        }],
    )
    .unwrap_or_else(|_| unreachable!());
    let result = solve_priced_with_topology(
        &party(&[
            RoomingRelation::SeparateRoom,
            RoomingRelation::ConnectedRooms,
        ]),
        date(2026, 9, 21),
        10,
        11,
        &[room(10, 1, 1, 1), room(11, 1, 1, 1)],
        &topology,
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());

    assert_eq!(result.valid_solution_count, 2);
}

#[test]
fn complete_topology_with_no_edge_proves_relation_false() {
    let topology = RoomRelationIndex::try_new(&[10, 11], &[]).unwrap_or_else(|_| unreachable!());
    let result = solve_priced_with_topology(
        &party(&[
            RoomingRelation::SeparateRoom,
            RoomingRelation::AdjacentRooms,
        ]),
        date(2026, 9, 21),
        10,
        11,
        &[room(10, 1, 1, 1), room(11, 1, 1, 1)],
        &topology,
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());

    assert_eq!(result.valid_solution_count, 0);
}

#[test]
fn near_is_satisfied_by_same_room_without_inventing_an_edge() {
    let topology = RoomRelationIndex::try_new(&[10], &[]).unwrap_or_else(|_| unreachable!());
    let result = solve_priced_with_topology(
        &party(&[RoomingRelation::Near]),
        date(2026, 9, 21),
        10,
        11,
        &[room(10, 1, 1, 2)],
        &topology,
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());

    assert_eq!(result.valid_solution_count, 1);
}

#[test]
fn topology_constructor_rejects_unknown_self_and_duplicate_edges() {
    assert_eq!(
        RoomRelationIndex::try_new(
            &[10, 11],
            &[RoomTopologyEdge {
                left_room_id: 10,
                right_room_id: 99,
                relation: RoomTopologyRelation::Near,
            }],
        ),
        Err(TopologyError::UnknownRoom(99))
    );
    assert_eq!(
        RoomRelationIndex::try_new(
            &[10, 11],
            &[RoomTopologyEdge {
                left_room_id: 10,
                right_room_id: 10,
                relation: RoomTopologyRelation::Adjacent,
            }],
        ),
        Err(TopologyError::SelfEdge(10))
    );
    let duplicate = RoomTopologyEdge {
        left_room_id: 10,
        right_room_id: 11,
        relation: RoomTopologyRelation::Connected,
    };
    assert_eq!(
        RoomRelationIndex::try_new(&[10, 11], &[duplicate, duplicate]),
        Err(TopologyError::DuplicateEdge {
            left_room_id: 10,
            right_room_id: 11,
            relation: RoomTopologyRelation::Connected,
        })
    );
}
