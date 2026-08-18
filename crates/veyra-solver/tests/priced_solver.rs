use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, GuardianRelationship, Traveler, TravelerId,
};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector};
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile};
use veyra_rules::Rule;
use veyra_solver::{PricedRoomOffer, SolutionProfile, SolverConfig, solve_priced};

fn date(y: i32, m: u8, d: u8) -> CivilDate {
    CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
}

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

fn policy() -> veyra_rule_compiler::CompiledRule {
    compile(
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
    .unwrap_or_else(|_| unreachable!())
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

fn room(id: u32, nightly: i64, child_surcharge: i64) -> PricedRoomOffer {
    PricedRoomOffer {
        room_id: id,
        prices: PriceVector::try_new(10, vec![money(nightly)], 1)
            .unwrap_or_else(|_| unreachable!()),
        occupancy_adjustment: OccupancyAdjustment {
            per_adult_per_night: money(0),
            per_child_per_night: money(child_surcharge),
        },
        floor: 1,
        building: 1,
        adult_age: 18,
        occupancy_rule: policy(),
    }
}

#[test]
fn cheapest_allocation_uses_actual_room_occupancy_for_pricing() {
    let result = solve_priced(
        &party(),
        date(2026, 9, 21),
        10,
        11,
        &[room(10, 100, 500), room(11, 120, 0)],
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());

    let cheapest = result
        .solutions
        .iter()
        .find(|solution| solution.profile == SolutionProfile::Cheapest)
        .unwrap_or_else(|| unreachable!());

    assert_eq!(cheapest.solution.total_price, money(220));
    let child_room = cheapest
        .solution
        .rooms
        .iter()
        .find(|allocation| allocation.travelers.contains(&TravelerId::new(3)))
        .unwrap_or_else(|| unreachable!());
    assert_eq!(child_room.room_id, 11);
}
