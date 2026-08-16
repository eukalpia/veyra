use veyra_party::TravelerId;
use veyra_rules::{OccupancyContext, Occupant, Rule, RuleError};

fn occupant(id: u32, age: u16, guardian: bool) -> Occupant {
    Occupant {
        traveler_id: TravelerId::new(id),
        age,
        authorized_guardian_present: guardian,
    }
}

#[test]
fn context_upper_bound_and_every_validation_error_are_explicit() {
    let too_many = (0..65)
        .map(|id| occupant(id, 20, false))
        .collect::<Vec<_>>();
    assert_eq!(
        OccupancyContext::new(too_many),
        Err(RuleError::InvalidOccupantCount(65))
    );
    assert_eq!(
        Rule::AgeRangeCount {
            min_age: 0,
            max_age: 10,
            min_count: 2,
            max_count: 1,
        }
        .validate(),
        Err(RuleError::InvalidCountRange)
    );
    assert_eq!(
        Rule::RequireAdult {
            adult_age: 18,
            min_adults: 0,
        }
        .validate(),
        Err(RuleError::InvalidAdultRule)
    );
    for error in [
        RuleError::InvalidOccupantCount(0),
        RuleError::DuplicateTraveler(TravelerId::new(1)),
        RuleError::InvalidCountRange,
        RuleError::InvalidAdultRule,
        RuleError::InvalidGuardianRule,
        RuleError::EmptyBooleanRule,
        RuleError::RuleTooComplex,
        RuleError::CountOverflow,
    ] {
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn boolean_short_circuit_paths_are_deterministic() {
    let context =
        OccupancyContext::new(vec![occupant(1, 30, false)]).unwrap_or_else(|_| unreachable!());
    let false_first = Rule::And(vec![
        Rule::Capacity { min: 2, max: 2 },
        Rule::RequireAdult {
            adult_age: 18,
            min_adults: 1,
        },
    ]);
    assert_eq!(false_first.evaluate(&context), Ok(false));

    let true_last = Rule::Or(vec![
        Rule::Capacity { min: 2, max: 2 },
        Rule::RequireAdult {
            adult_age: 18,
            min_adults: 1,
        },
    ]);
    assert_eq!(true_last.evaluate(&context), Ok(true));

    let all_true = Rule::And(vec![
        Rule::Capacity { min: 1, max: 1 },
        Rule::RequireAdult {
            adult_age: 18,
            min_adults: 1,
        },
    ]);
    assert_eq!(all_true.evaluate(&context), Ok(true));

    let all_false = Rule::Or(vec![
        Rule::Capacity { min: 2, max: 2 },
        Rule::AgeRangeCount {
            min_age: 0,
            max_age: 17,
            min_count: 1,
            max_count: 1,
        },
    ]);
    assert_eq!(all_false.evaluate(&context), Ok(false));
}

#[test]
fn primitive_rules_not_and_complexity_bound_cover_both_outcomes() {
    let supervised = OccupancyContext::new(vec![
        occupant(1, 40, false),
        occupant(2, 8, true),
    ])
    .unwrap_or_else(|_| unreachable!());
    let unsupervised = OccupancyContext::new(vec![occupant(3, 8, false)])
        .unwrap_or_else(|_| unreachable!());

    assert_eq!(Rule::Capacity { min: 2, max: 2 }.evaluate(&supervised), Ok(true));
    assert_eq!(Rule::Capacity { min: 3, max: 4 }.evaluate(&supervised), Ok(false));
    assert_eq!(
        Rule::AgeRangeCount {
            min_age: 0,
            max_age: 17,
            min_count: 1,
            max_count: 1,
        }
        .evaluate(&supervised),
        Ok(true)
    );
    assert_eq!(
        Rule::AgeRangeCount {
            min_age: 0,
            max_age: 17,
            min_count: 2,
            max_count: 2,
        }
        .evaluate(&supervised),
        Ok(false)
    );
    assert_eq!(
        Rule::RequireGuardianForMinors {
            minor_below_age: 18,
        }
        .evaluate(&supervised),
        Ok(true)
    );
    assert_eq!(
        Rule::RequireGuardianForMinors {
            minor_below_age: 18,
        }
        .evaluate(&unsupervised),
        Ok(false)
    );
    assert_eq!(
        Rule::Not(Box::new(Rule::Capacity { min: 3, max: 3 })).evaluate(&supervised),
        Ok(true)
    );
    assert_eq!(
        Rule::Not(Box::new(Rule::Capacity { min: 2, max: 2 })).evaluate(&supervised),
        Ok(false)
    );

    let mut too_deep = Rule::Capacity { min: 1, max: 1 };
    for _ in 0..256 {
        too_deep = Rule::Not(Box::new(too_deep));
    }
    assert_eq!(too_deep.validate(), Err(RuleError::RuleTooComplex));
}
