use veyra_availability::AvailabilityIndex;
use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector};
use veyra_query::{
    MultiRoomStayQuery, QueryError, RoomDocument, RoomPlacement, RoomSpatialProjection,
    RoomTopologyEdge, RoomTopologyRelation, SearchEngine, SolutionProfile, SolverConfig,
};
use veyra_restrictions::{
    RESTRICTION_SCHEMA_V1, RestrictionRule, compile as compile_restrictions,
};
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile as compile_rule};
use veyra_rules::Rule;

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

fn room(room_id: u32) -> RoomDocument {
    RoomDocument {
        room_id,
        property_id: 7,
        destination_id: 55,
        adult_age: 18,
        occupancy_rule: compile_rule(
            RULE_SCHEMA_V1,
            &Rule::Capacity { min: 1, max: 1 },
        )
        .unwrap_or_else(|_| unreachable!()),
        restrictions: compile_restrictions(
            RESTRICTION_SCHEMA_V1,
            &[RestrictionRule::MinStay(1), RestrictionRule::MaxStay(5)],
        )
        .unwrap_or_else(|_| unreachable!()),
        prices: PriceVector::try_new(10, vec![money(100)], 1)
            .unwrap_or_else(|_| unreachable!()),
        occupancy_adjustment: OccupancyAdjustment {
            per_adult_per_night: money(0),
            per_child_per_night: money(0),
        },
        distance_meters: 100,
        quality_milli: 900,
        flexibility_milli: 900,
        family_penalty: 0,
    }
}

fn availability() -> AvailabilityIndex {
    let mut availability = AvailabilityIndex::new(10, 1, 2).unwrap_or_else(|_| unreachable!());
    for room_id in 0..2 {
        availability
            .set_available(10, room_id, true)
            .unwrap_or_else(|_| unreachable!());
    }
    availability
}

fn query(party: &BookingParty) -> MultiRoomStayQuery<'_> {
    MultiRoomStayQuery {
        destination_id: 55,
        check_in_day: 10,
        check_out_day: 11,
        check_in_date: date(2026, 9, 21),
        party,
        budget: None,
        profile: SolutionProfile::Cheapest,
        solver: SolverConfig::default(),
        limit: 10,
    }
}

fn spatial() -> RoomSpatialProjection {
    RoomSpatialProjection::try_new(
        vec![
            RoomPlacement {
                room_id: 0,
                floor: 3,
                building: 1,
            },
            RoomPlacement {
                room_id: 1,
                floor: 3,
                building: 1,
            },
        ],
        vec![RoomTopologyEdge {
            left_room_id: 0,
            right_room_id: 1,
            relation: RoomTopologyRelation::Connected,
        }],
    )
    .unwrap_or_else(|_| unreachable!())
}

#[test]
fn spatial_semantics_fail_closed_without_projection() {
    let party = party(&[
        RoomingRelation::SeparateRoom,
        RoomingRelation::ConnectedRooms,
    ]);
    let engine = SearchEngine::try_new(availability(), vec![room(0), room(1)])
        .unwrap_or_else(|_| unreachable!());

    assert_eq!(
        engine.search_multi_room(&query(&party)),
        Err(QueryError::MissingRoomTopology(
            RoomingRelation::ConnectedRooms
        ))
    );
}

#[test]
fn connected_and_same_floor_constraints_use_explicit_spatial_projection() {
    let party = party(&[
        RoomingRelation::SeparateRoom,
        RoomingRelation::ConnectedRooms,
        RoomingRelation::SameFloor,
        RoomingRelation::SameBuilding,
    ]);
    let engine = SearchEngine::try_new_with_spatial(
        availability(),
        vec![room(0), room(1)],
        spatial(),
    )
    .unwrap_or_else(|_| unreachable!());

    let result = engine
        .search_multi_room(&query(&party))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].property_id, 7);
    assert_eq!(result.hits[0].rooms.len(), 2);
    assert_eq!(result.hits[0].projected_price, money(200));
}

#[test]
fn spatial_projection_rejects_non_dense_placement_ids() {
    assert!(matches!(
        RoomSpatialProjection::try_new(
            vec![RoomPlacement {
                room_id: 1,
                floor: 1,
                building: 1,
            }],
            vec![],
        ),
        Err(QueryError::SpatialNonDenseRoomId {
            expected: 0,
            actual: 1,
        })
    ));
}
