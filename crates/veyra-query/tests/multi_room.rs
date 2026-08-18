use veyra_availability::AvailabilityIndex;
use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, GuardianRelationship, Traveler, TravelerId,
};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector};
use veyra_query::{
    MultiRoomStayQuery, RoomDocument, SearchEngine, SolutionProfile, SolverConfig, StayQuery,
};
use veyra_ranking::{RankingKind, RankingProfile};
use veyra_restrictions::{RESTRICTION_SCHEMA_V1, RestrictionRule, compile as compile_restrictions};
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile as compile_rule};
use veyra_rules::Rule;

fn date(y: i32, m: u8, d: u8) -> CivilDate {
    CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
}

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

fn party() -> BookingParty {
    let mut builder = BookingParty::builder();
    for traveler in [
        Traveler::new(TravelerId::new(1), AgeEvidence::AgeAtCheckIn(38), false),
        Traveler::new(TravelerId::new(2), AgeEvidence::AgeAtCheckIn(35), false),
        Traveler::new(TravelerId::new(3), AgeEvidence::AgeAtCheckIn(8), false),
    ] {
        builder
            .add_traveler(traveler)
            .unwrap_or_else(|_| unreachable!());
    }
    builder
        .add_guardian(GuardianRelationship {
            guardian: TravelerId::new(1),
            dependent: TravelerId::new(3),
            valid_for_rooming: true,
        })
        .unwrap_or_else(|_| unreachable!());
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn room(room_id: u32, nightly: i64, child_surcharge: i64) -> RoomDocument {
    RoomDocument {
        room_id,
        property_id: 7,
        destination_id: 55,
        adult_age: 18,
        occupancy_rule: compile_rule(
            RULE_SCHEMA_V1,
            &Rule::And(vec![
                Rule::Capacity { min: 1, max: 2 },
                Rule::RequireAdult {
                    adult_age: 18,
                    min_adults: 1,
                },
                Rule::RequireGuardianForMinors {
                    minor_below_age: 16,
                },
            ]),
        )
        .unwrap_or_else(|_| unreachable!()),
        restrictions: compile_restrictions(
            RESTRICTION_SCHEMA_V1,
            &[RestrictionRule::MinStay(1), RestrictionRule::MaxStay(5)],
        )
        .unwrap_or_else(|_| unreachable!()),
        prices: PriceVector::try_new(10, vec![money(nightly); 5], 1)
            .unwrap_or_else(|_| unreachable!()),
        occupancy_adjustment: OccupancyAdjustment {
            per_adult_per_night: money(0),
            per_child_per_night: money(child_surcharge),
        },
        distance_meters: 500,
        quality_milli: 800,
        flexibility_milli: 800,
        family_penalty: 0,
    }
}

fn engine() -> SearchEngine {
    let mut availability = AvailabilityIndex::new(10, 5, 2).unwrap_or_else(|_| unreachable!());
    for room_id in 0..2 {
        for day in 10..15 {
            availability
                .set_available(day, room_id, true)
                .unwrap_or_else(|_| unreachable!());
        }
    }
    SearchEngine::try_new(availability, vec![room(0, 100, 500), room(1, 120, 0)])
        .unwrap_or_else(|_| unreachable!())
}

#[test]
fn multi_room_search_solves_one_property_with_exact_occupancy_pricing() {
    let party = party();
    let single = engine()
        .search(&StayQuery {
            destination_id: 55,
            check_in_day: 10,
            check_out_day: 11,
            check_in_date: date(2026, 9, 21),
            party: &party,
            budget: None,
            ranking: RankingProfile::v1(RankingKind::Cheapest),
            limit: 10,
        })
        .unwrap_or_else(|_| unreachable!());
    assert!(single.hits.is_empty());

    let multi = engine()
        .search_multi_room(&MultiRoomStayQuery {
            destination_id: 55,
            check_in_day: 10,
            check_out_day: 11,
            check_in_date: date(2026, 9, 21),
            party: &party,
            budget: None,
            profile: SolutionProfile::Cheapest,
            solver: SolverConfig::default(),
            limit: 10,
        })
        .unwrap_or_else(|_| unreachable!());

    assert_eq!(multi.hits.len(), 1);
    let hit = &multi.hits[0];
    assert_eq!(hit.property_id, 7);
    assert_eq!(hit.projected_price, money(220));
    assert_eq!(hit.rooms.len(), 2);
    let child_room = hit
        .rooms
        .iter()
        .find(|allocation| allocation.travelers.contains(&TravelerId::new(3)))
        .unwrap_or_else(|| unreachable!());
    assert_eq!(child_room.room_id, 1);
    assert_eq!(multi.explain.properties_considered, 1);
    assert_eq!(multi.explain.properties_solved, 1);
    assert_eq!(multi.explain.returned, 1);
}
