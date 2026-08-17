use veyra_party::{
    AgeEvidence, BookingParty, CivilDate, ConstraintStrength, RoomingIntent, RoomingRelation,
    Traveler, TravelerId,
};
use veyra_pricing::MoneyMicros;
use veyra_rule_compiler::{RULE_SCHEMA_V1, compile};
use veyra_rules::Rule;
use veyra_solver::{RoomOffer, SolutionProfile, SolverConfig, solve};

fn date(y: i32, m: u8, d: u8) -> CivilDate {
    CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
}

fn money(value: i64) -> MoneyMicros {
    MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
}

fn party() -> BookingParty {
    let mut builder = BookingParty::builder();
    for traveler in [
        Traveler::new(TravelerId::new(1), AgeEvidence::AgeAtCheckIn(40), false),
        Traveler::new(TravelerId::new(2), AgeEvidence::AgeAtCheckIn(38), false),
        Traveler::new(TravelerId::new(3), AgeEvidence::AgeAtCheckIn(30), false),
    ] {
        builder
            .add_traveler(traveler)
            .unwrap_or_else(|_| unreachable!());
    }
    builder
        .add_rooming_intent(RoomingIntent {
            left: TravelerId::new(1),
            right: TravelerId::new(2),
            strength: ConstraintStrength::Must,
            relation: RoomingRelation::SameRoom,
        })
        .unwrap_or_else(|_| unreachable!());
    builder.build().unwrap_or_else(|_| unreachable!())
}

fn policy() -> veyra_rule_compiler::CompiledRule {
    compile(
        RULE_SCHEMA_V1,
        &Rule::And(vec![
            Rule::Capacity { min: 1, max: 3 },
            Rule::RequireAdult {
                adult_age: 18,
                min_adults: 1,
            },
        ]),
    )
    .unwrap_or_else(|_| unreachable!())
}

fn offers() -> Vec<RoomOffer> {
    vec![
        RoomOffer {
            room_id: 10,
            projected_price: money(100),
            floor: 1,
            building: 1,
            adult_age: 18,
            occupancy_rule: policy(),
        },
        RoomOffer {
            room_id: 11,
            projected_price: money(100),
            floor: 1,
            building: 1,
            adult_age: 18,
            occupancy_rule: policy(),
        },
    ]
}

#[test]
fn hard_same_room_constraint_prunes_impossible_partial_assignments() {
    let result = solve(
        &party(),
        date(2026, 9, 21),
        &offers(),
        SolverConfig::default(),
    )
    .unwrap_or_else(|_| unreachable!());

    assert_eq!(result.explored_states, 10);
    assert_eq!(result.valid_solution_count, 4);
    let cheapest = result
        .solutions
        .iter()
        .find(|solution| solution.profile == SolutionProfile::Cheapest)
        .unwrap_or_else(|| unreachable!());
    assert_eq!(cheapest.solution.total_price, money(100));
    let pair_room = cheapest
        .solution
        .rooms
        .iter()
        .find(|allocation| allocation.travelers.contains(&TravelerId::new(1)))
        .unwrap_or_else(|| unreachable!());
    assert!(pair_room.travelers.contains(&TravelerId::new(2)));
}
