use proptest::prelude::*;
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

fn policy(capacity: u16) -> veyra_rule_compiler::CompiledRule {
    compile(
        RULE_SCHEMA_V1,
        &Rule::Capacity {
            min: 1,
            max: capacity,
        },
    )
    .unwrap_or_else(|_| unreachable!())
}

fn party(traveler_count: usize, relation_kind: u8) -> BookingParty {
    let mut builder = BookingParty::builder();
    for index in 0..traveler_count {
        builder
            .add_traveler(Traveler::new(
                TravelerId::new(u32::try_from(index + 1).unwrap_or_else(|_| unreachable!())),
                AgeEvidence::AgeAtCheckIn(30),
                false,
            ))
            .unwrap_or_else(|_| unreachable!());
    }
    let relation = match relation_kind {
        1 => Some(RoomingRelation::SameRoom),
        2 => Some(RoomingRelation::SeparateRoom),
        _ => None,
    };
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

fn offers(room_count: usize, traveler_count: usize) -> Vec<RoomOffer> {
    (0..room_count)
        .map(|index| RoomOffer {
            room_id: u32::try_from(100 + index).unwrap_or_else(|_| unreachable!()),
            projected_price: money(
                i64::try_from((index + 1) * 100).unwrap_or_else(|_| unreachable!()),
            ),
            floor: 1,
            building: 1,
            adult_age: 18,
            occupancy_rule: policy(
                u16::try_from(traveler_count).unwrap_or_else(|_| unreachable!()),
            ),
        })
        .collect()
}

fn reference(traveler_count: usize, room_count: usize, relation_kind: u8) -> (usize, i64) {
    let assignment_count =
        room_count.pow(u32::try_from(traveler_count).unwrap_or_else(|_| unreachable!()));
    let mut valid_count = 0_usize;
    let mut cheapest = i64::MAX;

    for encoded in 0..assignment_count {
        let mut value = encoded;
        let mut assignment = vec![0_usize; traveler_count];
        for slot in &mut assignment {
            *slot = value % room_count;
            value /= room_count;
        }
        let hard_valid = match relation_kind {
            1 => assignment[0] == assignment[1],
            2 => assignment[0] != assignment[1],
            _ => true,
        };
        if !hard_valid {
            continue;
        }
        valid_count += 1;
        let mut used = vec![false; room_count];
        for room in assignment {
            used[room] = true;
        }
        let total = used
            .iter()
            .enumerate()
            .filter(|(_, is_used)| **is_used)
            .map(|(index, _)| i64::try_from((index + 1) * 100).unwrap_or_else(|_| unreachable!()))
            .sum::<i64>();
        cheapest = cheapest.min(total);
    }

    (valid_count, cheapest)
}

proptest! {
    #[test]
    fn pruned_solver_matches_exhaustive_reference(
        traveler_count in 2usize..5,
        room_count in 2usize..4,
        relation_kind in 0u8..3,
    ) {
        let party = party(traveler_count, relation_kind);
        let offers = offers(room_count, traveler_count);
        let result = solve(
            &party,
            date(2026, 9, 21),
            &offers,
            SolverConfig::default(),
        )
        .unwrap_or_else(|_| unreachable!());
        let (reference_count, reference_cheapest) =
            reference(traveler_count, room_count, relation_kind);

        prop_assert_eq!(result.valid_solution_count, reference_count);
        let cheapest = result
            .solutions
            .iter()
            .find(|solution| solution.profile == SolutionProfile::Cheapest)
            .unwrap_or_else(|| unreachable!());
        prop_assert_eq!(cheapest.solution.total_price.get(), reference_cheapest);
    }
}
