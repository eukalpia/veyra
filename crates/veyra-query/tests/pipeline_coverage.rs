use veyra_availability::{AvailabilityError, AvailabilityIndex};
use veyra_occupancy::OccupancyError;
use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector};
use veyra_query::{QueryError, RoomDocument, SearchEngine, StayQuery};
use veyra_ranking::{RankingKind, RankingProfile};
use veyra_restrictions::{
    CompiledRestrictions, RESTRICTION_SCHEMA_V1, RestrictionRule, compile as compile_restrictions,
};
use veyra_rule_compiler::{CompiledRule, RULE_SCHEMA_V1, compile as compile_rule};
use veyra_rules::Rule;

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

fn date(year: i32, month: u8, day: u8) -> CivilDate {
    CivilDate::new(year, month, day).unwrap_or_else(|_| unreachable!())
}

fn policy() -> CompiledRule {
    compile_rule(RULE_SCHEMA_V1, &Rule::Capacity { min: 1, max: 4 })
        .unwrap_or_else(|_| unreachable!())
}

fn restrictions(extra: &[RestrictionRule]) -> CompiledRestrictions {
    let mut rules = vec![RestrictionRule::MinStay(1), RestrictionRule::MaxStay(5)];
    rules.extend_from_slice(extra);
    compile_restrictions(RESTRICTION_SCHEMA_V1, &rules).unwrap_or_else(|_| unreachable!())
}

fn adult_party() -> BookingParty {
    let mut builder = BookingParty::builder();
    builder
        .add_traveler(Traveler::new(
            TravelerId::new(1),
            AgeEvidence::AgeAtCheckIn(30),
            false,
        ))
        .unwrap_or_else(|_| unreachable!());
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn separate_party() -> BookingParty {
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
            strength: ConstraintStrength::Must,
            relation: RoomingRelation::SeparateRoom,
        })
        .unwrap_or_else(|_| unreachable!());
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn future_birth_party() -> BookingParty {
    let mut builder = BookingParty::builder();
    builder
        .add_traveler(Traveler::new(
            TravelerId::new(1),
            AgeEvidence::BirthDate(date(2030, 1, 1)),
            false,
        ))
        .unwrap_or_else(|_| unreachable!());
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn document(destination_id: u32, restrictions: CompiledRestrictions) -> RoomDocument {
    RoomDocument {
        room_id: 0,
        property_id: 100,
        destination_id,
        adult_age: 18,
        occupancy_rule: policy(),
        restrictions,
        prices: PriceVector::try_new(10, vec![money(100); 8], 1)
            .unwrap_or_else(|_| unreachable!()),
        occupancy_adjustment: OccupancyAdjustment {
            per_adult_per_night: money(0),
            per_child_per_night: money(0),
        },
        distance_meters: 50,
        quality_milli: 900,
        flexibility_milli: 800,
        family_penalty: 0,
    }
}

fn engine(room: RoomDocument) -> SearchEngine {
    let mut availability = AvailabilityIndex::new(10, 8, 1).unwrap_or_else(|_| unreachable!());
    for day in 10..18 {
        availability
            .set_available(day, 0, true)
            .unwrap_or_else(|_| unreachable!());
    }
    SearchEngine::try_new(availability, vec![room]).unwrap_or_else(|_| unreachable!())
}

fn query(party: &BookingParty) -> StayQuery<'_> {
    StayQuery {
        destination_id: 7,
        check_in_day: 10,
        check_out_day: 12,
        check_in_date: date(2026, 9, 21),
        party,
        budget: None,
        ranking: RankingProfile::v1(RankingKind::BestValue),
        limit: 10,
    }
}

#[test]
fn valid_candidate_reaches_ranking_and_hit_projection() {
    let party = adult_party();
    let result = engine(document(7, restrictions(&[])))
        .search(&query(&party))
        .unwrap_or_else(|_| unreachable!());

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].property_id, 100);
    assert_eq!(result.hits[0].room_id, 0);
    assert_eq!(result.hits[0].projected_price, money(200));
    assert_eq!(result.explain.destination_candidates, 1);
    assert_eq!(result.explain.restriction_candidates, 1);
    assert_eq!(result.explain.occupancy_candidates, 1);
    assert_eq!(result.explain.priced_candidates, 1);
    assert_eq!(result.explain.ranked_candidates, 1);
    assert_eq!(result.explain.returned, 1);
}

#[test]
fn destination_and_every_stay_restriction_rejection_are_explainable() {
    let party = adult_party();
    let result = engine(document(8, restrictions(&[])))
        .search(&query(&party))
        .unwrap_or_else(|_| unreachable!());
    assert!(result.hits.is_empty());
    assert_eq!(result.explain.destination_candidates, 0);

    let policies = [
        compile_restrictions(
            RESTRICTION_SCHEMA_V1,
            &[RestrictionRule::MinStay(3), RestrictionRule::MaxStay(5)],
        )
        .unwrap_or_else(|_| unreachable!()),
        compile_restrictions(
            RESTRICTION_SCHEMA_V1,
            &[RestrictionRule::MinStay(1), RestrictionRule::MaxStay(1)],
        )
        .unwrap_or_else(|_| unreachable!()),
        restrictions(&[RestrictionRule::ClosedToArrival(10)]),
        restrictions(&[RestrictionRule::ClosedToDeparture(12)]),
    ];

    for policy in policies {
        let result = engine(document(7, policy))
            .search(&query(&party))
            .unwrap_or_else(|_| unreachable!());
        assert!(result.hits.is_empty());
        assert_eq!(result.explain.destination_candidates, 1);
        assert_eq!(result.explain.restriction_candidates, 0);
    }
}

#[test]
fn hard_separation_is_a_candidate_rejection_but_bad_age_is_an_error() {
    let separate = separate_party();
    let result = engine(document(7, restrictions(&[])))
        .search(&query(&separate))
        .unwrap_or_else(|_| unreachable!());
    assert!(result.hits.is_empty());
    assert_eq!(result.explain.restriction_candidates, 1);
    assert_eq!(result.explain.occupancy_candidates, 0);

    let future = future_birth_party();
    assert_eq!(
        engine(document(7, restrictions(&[]))).search(&query(&future)),
        Err(QueryError::Occupancy(OccupancyError::InvalidAge(
            TravelerId::new(1)
        )))
    );
}

#[test]
fn availability_failures_propagate_without_silently_hiding_results() {
    let party = adult_party();
    let mut request = query(&party);
    request.check_in_day = 9;
    request.check_out_day = 11;
    assert_eq!(
        engine(document(7, restrictions(&[]))).search(&request),
        Err(QueryError::Availability(AvailabilityError::DayOutOfRange(
            9
        )))
    );
}
