use veyra_availability::AvailabilityIndex;
use veyra_occupancy::OccupancyError;
use veyra_party::{AgeEvidence, BookingParty, CivilDate, Traveler, TravelerId};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector, PricingError};
use veyra_query::{QueryError, RoomDocument, SearchEngine, StayQuery};
use veyra_ranking::{MAX_TOP_K, RankingError, RankingKind, RankingProfile};
use veyra_restrictions::{RESTRICTION_SCHEMA_V1, RestrictionRule, compile as compile_restrictions};
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile as compile_rule};
use veyra_rules::Rule;

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

fn date() -> CivilDate {
    CivilDate::new(2026, 9, 21).unwrap_or_else(|_| unreachable!())
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

fn document(room_id: u32, capacity: u16, start: i32) -> RoomDocument {
    RoomDocument {
        room_id,
        property_id: 10 + room_id,
        destination_id: 7,
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
        restrictions: compile_restrictions(
            RESTRICTION_SCHEMA_V1,
            &[RestrictionRule::MinStay(1), RestrictionRule::MaxStay(5)],
        )
        .unwrap_or_else(|_| unreachable!()),
        prices: PriceVector::try_new(start, vec![money(100); 8], 1)
            .unwrap_or_else(|_| unreachable!()),
        occupancy_adjustment: OccupancyAdjustment {
            per_adult_per_night: money(0),
            per_child_per_night: money(0),
        },
        distance_meters: 100,
        quality_milli: 700,
        flexibility_milli: 700,
        family_penalty: 0,
    }
}

fn engine(room: RoomDocument, start_day: u32) -> SearchEngine {
    let mut availability =
        AvailabilityIndex::new(start_day, 8, 1).unwrap_or_else(|_| unreachable!());
    for day in start_day..start_day + 8 {
        availability
            .set_available(day, 0, true)
            .unwrap_or_else(|_| unreachable!());
    }
    SearchEngine::try_new(availability, vec![room]).unwrap_or_else(|_| unreachable!())
}

fn query(party: &BookingParty, start: u32) -> StayQuery<'_> {
    StayQuery {
        destination_id: 7,
        check_in_day: start,
        check_out_day: start + 2,
        check_in_date: date(),
        party,
        budget: None,
        ranking: RankingProfile::v1(RankingKind::Cheapest),
        limit: 10,
    }
}

#[test]
fn catalog_and_query_validation_fail_closed() {
    let availability = AvailabilityIndex::new(10, 2, 1).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        SearchEngine::try_new(availability.clone(), vec![]),
        Err(QueryError::RoomDocumentCountMismatch {
            expected: 1,
            actual: 0
        })
    ));
    let mut non_dense = document(1, 2, 10);
    assert!(matches!(
        SearchEngine::try_new(availability.clone(), vec![non_dense.clone()]),
        Err(QueryError::NonDenseRoomId {
            expected: 0,
            actual: 1
        })
    ));
    non_dense.room_id = 0;
    non_dense.adult_age = 0;
    assert!(matches!(
        SearchEngine::try_new(availability, vec![non_dense]),
        Err(QueryError::InvalidAdultAge(0))
    ));

    let p = party(1);
    let engine = engine(document(0, 2, 10), 10);
    let mut request = query(&p, 10);
    request.limit = 0;
    assert_eq!(engine.search(&request), Err(QueryError::InvalidLimit(0)));
    let mut request = query(&p, 10);
    request.limit = MAX_TOP_K + 1;
    assert_eq!(
        engine.search(&request),
        Err(QueryError::InvalidLimit(MAX_TOP_K + 1))
    );
    let mut request = query(&p, 10);
    request.check_out_day = request.check_in_day;
    assert_eq!(engine.search(&request), Err(QueryError::InvalidStayRange));
    let mut request = query(&p, 10);
    request.check_out_day = request.check_in_day.saturating_sub(1);
    assert_eq!(engine.search(&request), Err(QueryError::InvalidStayRange));
    let mut request = query(&p, 10);
    request.budget = Some(MoneyMicros::signed(-1));
    assert_eq!(engine.search(&request), Err(QueryError::NegativeBudget));
}

#[test]
fn service_day_pricing_occupancy_and_ranking_errors_propagate() {
    let p = party(2);

    let boundary = u32::try_from(i32::MAX - 1).unwrap_or_default();
    let boundary_engine = engine(document(0, 4, i32::MAX - 1), boundary);
    assert_eq!(
        boundary_engine.search(&query(&p, boundary)),
        Err(QueryError::ServiceDayOutOfRange)
    );

    let huge = u32::try_from(i32::MAX)
        .unwrap_or_default()
        .saturating_add(1);
    let huge_engine = engine(document(0, 4, i32::MAX), huge);
    assert_eq!(
        huge_engine.search(&query(&p, huge)),
        Err(QueryError::ServiceDayOutOfRange)
    );

    let occupancy_engine = engine(document(0, 1, 10), 10);
    let result = occupancy_engine
        .search(&query(&p, 10))
        .unwrap_or_else(|_| unreachable!());
    assert!(result.hits.is_empty());
    assert_eq!(result.explain.occupancy_candidates, 0);

    let mut negative = document(0, 4, 10);
    negative.occupancy_adjustment = OccupancyAdjustment {
        per_adult_per_night: MoneyMicros::signed(-1_000),
        per_child_per_night: money(0),
    };
    assert_eq!(
        engine(negative, 10).search(&query(&p, 10)),
        Err(QueryError::Pricing(PricingError::NegativeProjectedTotal))
    );

    let mut bad_rank = document(0, 4, 10);
    bad_rank.quality_milli = 1_001;
    assert_eq!(
        engine(bad_rank, 10).search(&query(&p, 10)),
        Err(QueryError::Ranking(RankingError::InvalidNormalizedSignal(
            0
        )))
    );
}

#[test]
fn restriction_and_budget_rejections_are_explainable_not_errors() {
    let p = party(1);
    let mut restricted = document(0, 2, 10);
    restricted.restrictions = compile_restrictions(
        RESTRICTION_SCHEMA_V1,
        &[RestrictionRule::MinStay(3), RestrictionRule::MaxStay(5)],
    )
    .unwrap_or_else(|_| unreachable!());
    let result = engine(restricted, 10)
        .search(&query(&p, 10))
        .unwrap_or_else(|_| unreachable!());
    assert!(result.hits.is_empty());
    assert_eq!(result.explain.destination_candidates, 1);
    assert_eq!(result.explain.restriction_candidates, 0);

    let budget_engine = engine(document(0, 2, 10), 10);
    let mut request = query(&p, 10);
    request.budget = Some(money(1));
    let result = budget_engine
        .search(&request)
        .unwrap_or_else(|_| unreachable!());
    assert!(result.hits.is_empty());
    assert_eq!(result.explain.priced_candidates, 1);
    assert_eq!(result.explain.budget_candidates, 0);

    let mut request = query(&p, 10);
    request.budget = Some(money(200));
    let result = budget_engine
        .search(&request)
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.explain.budget_candidates, 1);
}

#[test]
fn all_query_error_display_variants_are_stable() {
    let errors = [
        QueryError::CatalogInvariant,
        QueryError::RoomDocumentCountMismatch {
            expected: 1,
            actual: 2,
        },
        QueryError::NonDenseRoomId {
            expected: 1,
            actual: 2,
        },
        QueryError::InvalidAdultAge(1),
        QueryError::MissingRoomProjection(1),
        QueryError::InvalidLimit(1),
        QueryError::InvalidStayRange,
        QueryError::ServiceDayOutOfRange,
        QueryError::NegativeBudget,
        QueryError::PartyTooLarge(65),
        QueryError::Occupancy(OccupancyError::PolicyRejected),
        QueryError::Pricing(PricingError::InvalidStay),
        QueryError::Ranking(RankingError::UnsupportedVersion(9)),
    ];
    for error in errors {
        assert!(!error.to_string().is_empty());
    }
}
