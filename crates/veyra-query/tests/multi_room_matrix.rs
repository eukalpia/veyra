use veyra_availability::AvailabilityIndex;
use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector, PricingError};
use veyra_query::{
    MultiRoomStayQuery, QueryError, RoomDocument, RoomPlacement, RoomSpatialProjection,
    RoomTopologyEdge, RoomTopologyRelation, SearchEngine, SolutionProfile, SolverConfig,
};
use veyra_ranking::MAX_TOP_K;
use veyra_restrictions::{RESTRICTION_SCHEMA_V1, RestrictionRule, compile as compile_restrictions};
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile as compile_rule};
use veyra_rules::Rule;
use veyra_solver::{HARD_MAX_ROOMS, SolverError, TopologyError};

fn date() -> CivilDate {
    CivilDate::new(2026, 9, 21).unwrap_or_else(|_| unreachable!())
}

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

fn party(count: u32, relation: Option<RoomingRelation>) -> BookingParty {
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

fn restrictions(minimum: u16, maximum: u16) -> veyra_restrictions::CompiledRestrictions {
    compile_restrictions(
        RESTRICTION_SCHEMA_V1,
        &[
            RestrictionRule::MinStay(minimum),
            RestrictionRule::MaxStay(maximum),
        ],
    )
    .unwrap_or_else(|_| unreachable!())
}

fn room(
    room_id: u32,
    property_id: u32,
    destination_id: u32,
    capacity: u16,
    nightly: i64,
    price_start: i32,
) -> RoomDocument {
    RoomDocument {
        room_id,
        property_id,
        destination_id,
        adult_age: 18,
        occupancy_rule: compile_rule(
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
        .unwrap_or_else(|_| unreachable!()),
        restrictions: restrictions(1, 5),
        prices: PriceVector::try_new(price_start, vec![money(nightly); 8], 1)
            .unwrap_or_else(|_| unreachable!()),
        occupancy_adjustment: OccupancyAdjustment {
            per_adult_per_night: money(0),
            per_child_per_night: money(0),
        },
        distance_meters: 100,
        quality_milli: 800,
        flexibility_milli: 800,
        family_penalty: 0,
    }
}

fn engine(start_day: u32, days: u32, rooms: Vec<RoomDocument>) -> SearchEngine {
    let room_count = u32::try_from(rooms.len()).unwrap_or_else(|_| unreachable!());
    let mut availability =
        AvailabilityIndex::new(start_day, days, room_count).unwrap_or_else(|_| unreachable!());
    for room_id in 0..room_count {
        for day in start_day..start_day + days {
            availability
                .set_available(day, room_id, true)
                .unwrap_or_else(|_| unreachable!());
        }
    }
    SearchEngine::try_new(availability, rooms).unwrap_or_else(|_| unreachable!())
}

fn query(party: &BookingParty, profile: SolutionProfile) -> MultiRoomStayQuery<'_> {
    MultiRoomStayQuery {
        destination_id: 55,
        check_in_day: 10,
        check_out_day: 11,
        check_in_date: date(),
        party,
        budget: None,
        profile,
        solver: SolverConfig::default(),
        limit: 10,
    }
}

#[test]
fn spatial_projection_validates_complete_dense_topology() {
    let empty = RoomSpatialProjection::try_new(vec![], vec![]).unwrap_or_else(|_| unreachable!());
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);

    let placements = vec![
        RoomPlacement {
            room_id: 0,
            floor: 1,
            building: 1,
        },
        RoomPlacement {
            room_id: 1,
            floor: 2,
            building: 1,
        },
    ];
    let projection = RoomSpatialProjection::try_new(placements.clone(), vec![])
        .unwrap_or_else(|_| unreachable!());
    assert!(!projection.is_empty());
    assert_eq!(projection.len(), 2);

    for (edge, expected) in [
        (
            RoomTopologyEdge {
                left_room_id: 99,
                right_room_id: 1,
                relation: RoomTopologyRelation::Near,
            },
            TopologyError::UnknownRoom(99),
        ),
        (
            RoomTopologyEdge {
                left_room_id: 0,
                right_room_id: 99,
                relation: RoomTopologyRelation::Near,
            },
            TopologyError::UnknownRoom(99),
        ),
        (
            RoomTopologyEdge {
                left_room_id: 0,
                right_room_id: 0,
                relation: RoomTopologyRelation::Adjacent,
            },
            TopologyError::SelfEdge(0),
        ),
    ] {
        assert_eq!(
            RoomSpatialProjection::try_new(placements.clone(), vec![edge]),
            Err(QueryError::Topology(expected))
        );
    }

    let edge = RoomTopologyEdge {
        left_room_id: 0,
        right_room_id: 1,
        relation: RoomTopologyRelation::Connected,
    };
    assert!(matches!(
        RoomSpatialProjection::try_new(placements.clone(), vec![edge, edge]),
        Err(QueryError::Topology(TopologyError::DuplicateEdge { .. }))
    ));

    let availability = AvailabilityIndex::new(10, 1, 2).unwrap_or_else(|_| unreachable!());
    let incomplete = RoomSpatialProjection::try_new(
        vec![RoomPlacement {
            room_id: 0,
            floor: 1,
            building: 1,
        }],
        vec![],
    )
    .unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        SearchEngine::try_new_with_spatial(
            availability,
            vec![room(0, 1, 55, 2, 100, 10), room(1, 1, 55, 2, 100, 10)],
            incomplete,
        ),
        Err(QueryError::SpatialRoomCountMismatch {
            expected: 2,
            actual: 1,
        })
    ));
}

#[test]
fn multi_room_admission_and_solver_bounds_fail_closed() {
    let one = party(1, None);
    let base = engine(10, 8, vec![room(0, 1, 55, 2, 100, 10)]);

    let mut request = query(&one, SolutionProfile::Cheapest);
    request.limit = 0;
    assert_eq!(base.search_multi_room(&request), Err(QueryError::InvalidLimit(0)));
    request.limit = MAX_TOP_K + 1;
    assert_eq!(
        base.search_multi_room(&request),
        Err(QueryError::InvalidLimit(MAX_TOP_K + 1))
    );
    request.limit = 10;
    request.check_out_day = request.check_in_day;
    assert_eq!(base.search_multi_room(&request), Err(QueryError::InvalidStayRange));
    request.check_out_day = 11;
    request.budget = Some(MoneyMicros::signed(-1));
    assert_eq!(base.search_multi_room(&request), Err(QueryError::NegativeBudget));

    let large_party = party(17, None);
    assert_eq!(
        base.search_multi_room(&query(&large_party, SolutionProfile::Cheapest)),
        Err(QueryError::SolverPartyTooLarge(17))
    );

    let topology_party = party(2, Some(RoomingRelation::SameFloor));
    assert_eq!(
        base.search_multi_room(&query(&topology_party, SolutionProfile::Cheapest)),
        Err(QueryError::MissingRoomTopology(RoomingRelation::SameFloor))
    );

    let rooms = (0..=HARD_MAX_ROOMS)
        .map(|index| {
            room(
                u32::try_from(index).unwrap_or_else(|_| unreachable!()),
                9,
                55,
                2,
                100,
                10,
            )
        })
        .collect::<Vec<_>>();
    let too_many = engine(10, 1, rooms);
    assert!(matches!(
        too_many.search_multi_room(&query(&one, SolutionProfile::Cheapest)),
        Err(QueryError::SolverRoomLimit { property_id: 9, rooms })
            if rooms == HARD_MAX_ROOMS + 1
    ));

    let outside_horizon = engine(10, 1, vec![room(0, 1, 55, 2, 100, 20)]);
    assert_eq!(
        outside_horizon.search_multi_room(&query(&one, SolutionProfile::Cheapest)),
        Err(QueryError::Pricing(PricingError::OutsidePriceHorizon))
    );

    let two = party(2, None);
    let bounded = engine(
        10,
        1,
        vec![room(0, 1, 55, 1, 100, 10), room(1, 1, 55, 1, 100, 10)],
    );
    let mut request = query(&two, SolutionProfile::Cheapest);
    request.solver = SolverConfig {
        max_states: 1,
        max_solutions: 10,
    };
    assert_eq!(
        bounded.search_multi_room(&request),
        Err(QueryError::Solver(SolverError::StateBudgetExhausted))
    );
    request.solver = SolverConfig {
        max_states: 0,
        max_solutions: 10,
    };
    assert_eq!(
        bounded.search_multi_room(&request),
        Err(QueryError::Solver(SolverError::InvalidBudget))
    );
}

#[test]
fn multi_room_service_day_conversions_fail_closed() {
    let one = party(1, None);
    let beyond = u32::try_from(i32::MAX)
        .unwrap_or_else(|_| unreachable!())
        .saturating_add(1);
    let high = engine(beyond, 2, vec![room(0, 1, 55, 2, 100, 0)]);
    let request = MultiRoomStayQuery {
        destination_id: 55,
        check_in_day: beyond,
        check_out_day: beyond + 1,
        check_in_date: date(),
        party: &one,
        budget: None,
        profile: SolutionProfile::Cheapest,
        solver: SolverConfig::default(),
        limit: 10,
    };
    assert_eq!(
        high.search_multi_room(&request),
        Err(QueryError::ServiceDayOutOfRange)
    );

    let boundary = u32::try_from(i32::MAX).unwrap_or_else(|_| unreachable!());
    let high = engine(boundary, 2, vec![room(0, 1, 55, 2, 100, 0)]);
    let request = MultiRoomStayQuery {
        destination_id: 55,
        check_in_day: boundary,
        check_out_day: boundary + 1,
        check_in_date: date(),
        party: &one,
        budget: None,
        profile: SolutionProfile::Cheapest,
        solver: SolverConfig::default(),
        limit: 10,
    };
    assert_eq!(
        high.search_multi_room(&request),
        Err(QueryError::ServiceDayOutOfRange)
    );
}

#[test]
fn explain_tracks_infeasible_budget_destination_and_restriction_paths() {
    let travelers = party(2, None);
    let mut rooms = vec![
        room(0, 1, 55, 2, 200, 10),
        room(1, 2, 55, 1, 100, 10),
        room(2, 3, 55, 2, 1_000, 10),
        room(3, 4, 99, 2, 100, 10),
        room(4, 5, 55, 2, 100, 10),
    ];
    rooms[4].restrictions = restrictions(2, 5);
    let engine = engine(10, 1, rooms);
    let mut request = query(&travelers, SolutionProfile::Cheapest);
    request.budget = Some(money(500));

    let result = engine
        .search_multi_room(&request)
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].property_id, 1);
    assert_eq!(result.explain.destination_candidates, 4);
    assert_eq!(result.explain.restriction_candidates, 3);
    assert_eq!(result.explain.properties_considered, 3);
    assert_eq!(result.explain.properties_solved, 2);
    assert_eq!(result.explain.infeasible_properties, 1);
    assert_eq!(result.explain.budget_rejected_properties, 1);
    assert_eq!(result.explain.returned, 1);
}

#[test]
fn all_solution_profiles_sort_and_limit_deterministically() {
    let travelers = party(2, None);
    let rooms = vec![
        room(0, 1, 55, 2, 200, 10),
        room(1, 2, 55, 1, 100, 10),
        room(2, 2, 55, 1, 100, 10),
        room(3, 3, 55, 2, 200, 10),
        room(4, 4, 55, 2, 200, 10),
    ];
    let engine = engine(10, 1, rooms);

    for profile in [
        SolutionProfile::Cheapest,
        SolutionProfile::FewestRooms,
        SolutionProfile::BestFamilyLayout,
    ] {
        let mut request = query(&travelers, profile);
        request.limit = 3;
        let result = engine
            .search_multi_room(&request)
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            result
                .hits
                .iter()
                .map(|hit| hit.property_id)
                .collect::<Vec<_>>(),
            vec![1, 3, 4]
        );
        assert_eq!(result.explain.returned, 3);
    }
}
