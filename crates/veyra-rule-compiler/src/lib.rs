#![forbid(unsafe_code)]

//! Deterministic compiler for Veyra occupancy Rule IR.
//!
//! Flexible rule trees are accepted only at compile/update time. The hot path executes
//! validated postfix bytecode with a fixed-size stack and no heap allocation.

use core::fmt;
use veyra_rules::{OccupancyContext, Rule, RuleError};

/// First supported public rule schema.
pub const RULE_SCHEMA_V1: u16 = 1;
/// Hard compiler/runtime bound inherited from the Rule IR complexity limit.
pub const MAX_OPS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    Capacity {
        min: u16,
        max: u16,
    },
    AgeRangeCount {
        min_age: u16,
        max_age: u16,
        min_count: u16,
        max_count: u16,
    },
    RequireAdult {
        adult_age: u16,
        min_adults: u16,
    },
    RequireGuardianForMinors {
        minor_below_age: u16,
    },
    And(usize),
    Or(usize),
    Not,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reduction {
    And,
    Or,
}

/// Versioned, validated bytecode executable by the occupancy hot path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledRule {
    schema_version: u16,
    ops: Vec<Op>,
}

impl CompiledRule {
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    #[must_use]
    pub fn op_count(&self) -> usize {
        self.ops.len()
    }

    /// Evaluates compiled Rule V1 without allocating.
    pub fn evaluate(&self, context: &OccupancyContext) -> Result<bool, CompileError> {
        if self.schema_version != RULE_SCHEMA_V1 {
            return Err(CompileError::UnsupportedSchema(self.schema_version));
        }
        if self.ops.len() > MAX_OPS {
            return Err(CompileError::TooManyOps(self.ops.len()));
        }
        let mut stack = [false; MAX_OPS];
        let mut stack_len = 0_usize;

        for op in &self.ops {
            match *op {
                Op::Capacity { min, max } => {
                    let count = context.occupants().len();
                    push(
                        &mut stack,
                        &mut stack_len,
                        (usize::from(min)..=usize::from(max)).contains(&count),
                    );
                }
                Op::AgeRangeCount {
                    min_age,
                    max_age,
                    min_count,
                    max_count,
                } => {
                    let count = context.count_age_range(min_age, max_age);
                    push(
                        &mut stack,
                        &mut stack_len,
                        (usize::from(min_count)..=usize::from(max_count)).contains(&count),
                    );
                }
                Op::RequireAdult {
                    adult_age,
                    min_adults,
                } => {
                    let adults = context
                        .occupants()
                        .iter()
                        .filter(|occupant| occupant.age >= adult_age)
                        .count();
                    push(
                        &mut stack,
                        &mut stack_len,
                        adults >= usize::from(min_adults),
                    );
                }
                Op::RequireGuardianForMinors { minor_below_age } => {
                    let valid = context.occupants().iter().all(|occupant| {
                        occupant.age >= minor_below_age || occupant.authorized_guardian_present
                    });
                    push(&mut stack, &mut stack_len, valid);
                }
                Op::Not => {
                    let value = pop(&stack, &mut stack_len)?;
                    push(&mut stack, &mut stack_len, !value);
                }
                Op::And(count) => {
                    reduce(&mut stack, &mut stack_len, count, Reduction::And)?;
                }
                Op::Or(count) => {
                    reduce(&mut stack, &mut stack_len, count, Reduction::Or)?;
                }
            }
        }

        if stack_len != 1 {
            return Err(CompileError::RuntimeInvariant);
        }
        Ok(stack[0])
    }
}

/// Compiles one typed Rule tree. Unknown schema or malformed IR never reaches execution.
pub fn compile(schema_version: u16, rule: &Rule) -> Result<CompiledRule, CompileError> {
    if schema_version != RULE_SCHEMA_V1 {
        return Err(CompileError::UnsupportedSchema(schema_version));
    }
    rule.validate().map_err(CompileError::InvalidRule)?;
    let mut ops = Vec::new();
    compile_inner(rule, &mut ops);
    Ok(CompiledRule {
        schema_version,
        ops,
    })
}

fn compile_inner(rule: &Rule, ops: &mut Vec<Op>) {
    match rule {
        Rule::Capacity { min, max } => ops.push(Op::Capacity {
            min: *min,
            max: *max,
        }),
        Rule::AgeRangeCount {
            min_age,
            max_age,
            min_count,
            max_count,
        } => ops.push(Op::AgeRangeCount {
            min_age: *min_age,
            max_age: *max_age,
            min_count: *min_count,
            max_count: *max_count,
        }),
        Rule::RequireAdult {
            adult_age,
            min_adults,
        } => ops.push(Op::RequireAdult {
            adult_age: *adult_age,
            min_adults: *min_adults,
        }),
        Rule::RequireGuardianForMinors { minor_below_age } => {
            ops.push(Op::RequireGuardianForMinors {
                minor_below_age: *minor_below_age,
            });
        }
        Rule::And(children) => {
            for child in children {
                compile_inner(child, ops);
            }
            ops.push(Op::And(children.len()));
        }
        Rule::Or(children) => {
            for child in children {
                compile_inner(child, ops);
            }
            ops.push(Op::Or(children.len()));
        }
        Rule::Not(child) => {
            compile_inner(child, ops);
            ops.push(Op::Not);
        }
    }
}

fn push(stack: &mut [bool; MAX_OPS], stack_len: &mut usize, value: bool) {
    stack[*stack_len] = value;
    *stack_len += 1;
}

fn pop(stack: &[bool; MAX_OPS], stack_len: &mut usize) -> Result<bool, CompileError> {
    if *stack_len == 0 {
        return Err(CompileError::RuntimeInvariant);
    }
    *stack_len -= 1;
    Ok(stack[*stack_len])
}

fn reduce(
    stack: &mut [bool; MAX_OPS],
    stack_len: &mut usize,
    count: usize,
    reduction: Reduction,
) -> Result<(), CompileError> {
    if count == 0 || count > *stack_len {
        return Err(CompileError::RuntimeInvariant);
    }
    let mut value = matches!(reduction, Reduction::And);
    for _ in 0..count {
        *stack_len -= 1;
        let next = stack[*stack_len];
        value = match reduction {
            Reduction::And => value && next,
            Reduction::Or => value || next,
        };
    }
    push(stack, stack_len, value);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompileError {
    UnsupportedSchema(u16),
    InvalidRule(RuleError),
    TooManyOps(usize),
    RuntimeInvariant,
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for CompileError {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use veyra_party::TravelerId;
    use veyra_rules::{OccupancyContext, Occupant};

    fn occupant(id: u32, age: u16, guardian: bool) -> Occupant {
        Occupant {
            traveler_id: TravelerId::new(id),
            age,
            authorized_guardian_present: guardian,
        }
    }

    fn context() -> OccupancyContext {
        OccupancyContext::new(vec![occupant(1, 30, false)]).unwrap_or_else(|_| unreachable!())
    }

    fn composite_rule() -> Rule {
        Rule::And(vec![
            Rule::Capacity { min: 1, max: 4 },
            Rule::RequireAdult {
                adult_age: 18,
                min_adults: 1,
            },
            Rule::Or(vec![
                Rule::AgeRangeCount {
                    min_age: 0,
                    max_age: 17,
                    min_count: 0,
                    max_count: 2,
                },
                Rule::Not(Box::new(Rule::Capacity { min: 4, max: 4 })),
            ]),
            Rule::RequireGuardianForMinors {
                minor_below_age: 16,
            },
        ])
    }

    #[test]
    fn compiler_matches_reference_interpreter() {
        let context = OccupancyContext::new(vec![occupant(1, 40, false), occupant(2, 10, true)])
            .unwrap_or_else(|_| unreachable!());
        let rule = composite_rule();
        let compiled = compile(RULE_SCHEMA_V1, &rule).unwrap_or_else(|_| unreachable!());
        assert_eq!(compiled.schema_version(), RULE_SCHEMA_V1);
        assert!(compiled.op_count() > 1);
        assert_eq!(
            compiled.evaluate(&context),
            rule.evaluate(&context).map_err(CompileError::InvalidRule)
        );
    }

    #[test]
    fn unsupported_schema_and_invalid_ir_fail_closed() {
        let rule = Rule::Capacity { min: 1, max: 2 };
        assert_eq!(compile(99, &rule), Err(CompileError::UnsupportedSchema(99)));
        let invalid = Rule::Capacity { min: 3, max: 2 };
        assert_eq!(
            compile(RULE_SCHEMA_V1, &invalid),
            Err(CompileError::InvalidRule(RuleError::InvalidCountRange))
        );
    }

    #[test]
    fn boolean_bytecode_matches_reference() {
        let context = context();
        for rule in [
            Rule::And(vec![
                Rule::Capacity { min: 1, max: 1 },
                Rule::Capacity { min: 2, max: 2 },
            ]),
            Rule::Or(vec![
                Rule::Capacity { min: 2, max: 2 },
                Rule::Capacity { min: 1, max: 1 },
            ]),
            Rule::Not(Box::new(Rule::Capacity { min: 2, max: 2 })),
        ] {
            let compiled = compile(RULE_SCHEMA_V1, &rule).unwrap_or_else(|_| unreachable!());
            assert_eq!(
                compiled.evaluate(&context),
                rule.evaluate(&context).map_err(CompileError::InvalidRule)
            );
        }
    }

    #[test]
    fn malformed_bytecode_and_stack_bounds_fail_closed() {
        let context = context();
        for compiled in [
            CompiledRule {
                schema_version: 9,
                ops: vec![Op::Capacity { min: 1, max: 1 }],
            },
            CompiledRule {
                schema_version: RULE_SCHEMA_V1,
                ops: Vec::new(),
            },
            CompiledRule {
                schema_version: RULE_SCHEMA_V1,
                ops: vec![Op::Not],
            },
            CompiledRule {
                schema_version: RULE_SCHEMA_V1,
                ops: vec![Op::And(0)],
            },
            CompiledRule {
                schema_version: RULE_SCHEMA_V1,
                ops: vec![Op::Capacity { min: 1, max: 1 }, Op::Or(2)],
            },
            CompiledRule {
                schema_version: RULE_SCHEMA_V1,
                ops: vec![
                    Op::Capacity { min: 1, max: 1 },
                    Op::Capacity { min: 1, max: 1 },
                ],
            },
            CompiledRule {
                schema_version: RULE_SCHEMA_V1,
                ops: vec![Op::Capacity { min: 1, max: 1 }; MAX_OPS + 1],
            },
        ] {
            assert!(compiled.evaluate(&context).is_err());
        }
    }

    #[test]
    fn compiler_capacity_guard_and_error_surface_are_explicit() {
        let mut maximum = Rule::Capacity { min: 1, max: 1 };
        for _ in 1..MAX_OPS {
            maximum = Rule::Not(Box::new(maximum));
        }
        assert_eq!(
            compile(RULE_SCHEMA_V1, &maximum)
                .unwrap_or_else(|_| unreachable!())
                .op_count(),
            MAX_OPS
        );
        let too_complex = Rule::Not(Box::new(maximum));
        assert_eq!(
            compile(RULE_SCHEMA_V1, &too_complex),
            Err(CompileError::InvalidRule(RuleError::RuleTooComplex))
        );
        for error in [
            CompileError::UnsupportedSchema(2),
            CompileError::InvalidRule(RuleError::InvalidCountRange),
            CompileError::TooManyOps(MAX_OPS + 1),
            CompileError::RuntimeInvariant,
        ] {
            assert!(!error.to_string().is_empty());
        }
    }

    proptest! {
        #[test]
        fn compiled_capacity_matches_reference(
            count in 1_usize..32,
            min in 0_u16..32,
            max in 0_u16..32,
        ) {
            let occupants = (0..count)
                .map(|index| occupant(u32::try_from(index).unwrap_or_default(), 30, false))
                .collect::<Vec<_>>();
            let context = OccupancyContext::new(occupants).unwrap_or_else(|_| unreachable!());
            let rule = Rule::Capacity { min, max };
            if min <= max {
                let compiled = compile(RULE_SCHEMA_V1, &rule).unwrap_or_else(|_| unreachable!());
                prop_assert_eq!(
                    compiled.evaluate(&context),
                    rule.evaluate(&context).map_err(CompileError::InvalidRule)
                );
            } else {
                prop_assert!(compile(RULE_SCHEMA_V1, &rule).is_err());
            }
        }
    }
}
