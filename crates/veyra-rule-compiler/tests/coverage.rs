use veyra_party::TravelerId;
use veyra_rule_compiler::{CompileError, RULE_SCHEMA_V1, compile};
use veyra_rules::{OccupancyContext, Occupant, Rule};

fn occupant(id: u32, age: u16, guardian: bool) -> Occupant {
    Occupant {
        traveler_id: TravelerId::new(id),
        age,
        authorized_guardian_present: guardian,
    }
}

fn context(occupants: Vec<Occupant>) -> OccupancyContext {
    OccupancyContext::new(occupants).unwrap_or_else(|_| unreachable!())
}

fn evaluate(rule: &Rule, context: &OccupancyContext) -> Result<bool, CompileError> {
    compile(RULE_SCHEMA_V1, rule)?.evaluate(context)
}

#[test]
fn every_primitive_and_boolean_opcode_covers_true_and_false_results() {
    let family = context(vec![occupant(1, 38, false), occupant(2, 8, true)]);
    let child = context(vec![occupant(3, 8, false)]);

    for (rule, expected) in [
        (Rule::Capacity { min: 2, max: 2 }, true),
        (Rule::Capacity { min: 3, max: 4 }, false),
        (
            Rule::AgeRangeCount {
                min_age: 0,
                max_age: 17,
                min_count: 1,
                max_count: 1,
            },
            true,
        ),
        (
            Rule::AgeRangeCount {
                min_age: 0,
                max_age: 17,
                min_count: 2,
                max_count: 2,
            },
            false,
        ),
        (
            Rule::RequireAdult {
                adult_age: 18,
                min_adults: 1,
            },
            true,
        ),
        (
            Rule::RequireAdult {
                adult_age: 18,
                min_adults: 2,
            },
            false,
        ),
        (
            Rule::RequireGuardianForMinors {
                minor_below_age: 18,
            },
            true,
        ),
        (
            Rule::Not(Box::new(Rule::Capacity { min: 3, max: 3 })),
            true,
        ),
        (
            Rule::Not(Box::new(Rule::Capacity { min: 2, max: 2 })),
            false,
        ),
        (
            Rule::And(vec![
                Rule::Capacity { min: 2, max: 2 },
                Rule::RequireAdult {
                    adult_age: 18,
                    min_adults: 1,
                },
            ]),
            true,
        ),
        (
            Rule::And(vec![
                Rule::Capacity { min: 2, max: 2 },
                Rule::RequireAdult {
                    adult_age: 18,
                    min_adults: 2,
                },
            ]),
            false,
        ),
        (
            Rule::Or(vec![
                Rule::Capacity { min: 3, max: 3 },
                Rule::RequireAdult {
                    adult_age: 18,
                    min_adults: 1,
                },
            ]),
            true,
        ),
        (
            Rule::Or(vec![
                Rule::Capacity { min: 3, max: 3 },
                Rule::RequireAdult {
                    adult_age: 18,
                    min_adults: 2,
                },
            ]),
            false,
        ),
    ] {
        assert_eq!(evaluate(&rule, &family), Ok(expected));
    }

    assert_eq!(
        evaluate(
            &Rule::RequireGuardianForMinors {
                minor_below_age: 18,
            },
            &child,
        ),
        Ok(false)
    );
}

#[test]
fn compiled_metadata_and_public_errors_are_explicit() {
    let compiled = compile(RULE_SCHEMA_V1, &Rule::Capacity { min: 1, max: 4 })
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(compiled.schema_version(), RULE_SCHEMA_V1);
    assert_eq!(compiled.op_count(), 1);
    assert_eq!(
        compile(2, &Rule::Capacity { min: 1, max: 4 }),
        Err(CompileError::UnsupportedSchema(2))
    );
}
