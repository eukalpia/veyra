#![forbid(unsafe_code)]

//! Typed deterministic rule IR for occupancy validity.

use core::fmt;
use std::collections::BTreeSet;

use veyra_party::TravelerId;

const MAX_RULE_NODES: usize = 256;
const MAX_OCCUPANTS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Occupant {
    pub traveler_id: TravelerId,
    pub age: u16,
    pub authorized_guardian_present: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OccupancyContext {
    occupants: Vec<Occupant>,
}

impl OccupancyContext {
    pub fn new(occupants: Vec<Occupant>) -> Result<Self, RuleError> {
        if occupants.is_empty() || occupants.len() > MAX_OCCUPANTS {
            return Err(RuleError::InvalidOccupantCount(occupants.len()));
        }
        let mut ids = BTreeSet::new();
        for occupant in &occupants {
            if !ids.insert(occupant.traveler_id) {
                return Err(RuleError::DuplicateTraveler(occupant.traveler_id));
            }
        }
        Ok(Self { occupants })
    }

    #[must_use]
    pub fn occupants(&self) -> &[Occupant] {
        &self.occupants
    }

    #[must_use]
    pub fn count_age_range(&self, min_inclusive: u16, max_inclusive: u16) -> usize {
        self.occupants
            .iter()
            .filter(|occupant| (min_inclusive..=max_inclusive).contains(&occupant.age))
            .count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Rule {
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
    And(Vec<Rule>),
    Or(Vec<Rule>),
    Not(Box<Rule>),
}

impl Rule {
    pub fn validate(&self) -> Result<(), RuleError> {
        let mut nodes = 0_usize;
        self.validate_inner(&mut nodes)
    }

    fn validate_inner(&self, nodes: &mut usize) -> Result<(), RuleError> {
        *nodes += 1;
        if *nodes > MAX_RULE_NODES {
            return Err(RuleError::RuleTooComplex);
        }
        match self {
            Self::Capacity { min, max } if min > max => Err(RuleError::InvalidCountRange),
            Self::AgeRangeCount {
                min_age,
                max_age,
                min_count,
                max_count,
            } if min_age > max_age || min_count > max_count => Err(RuleError::InvalidCountRange),
            Self::RequireAdult {
                adult_age,
                min_adults,
            } if *adult_age == 0 || *min_adults == 0 => Err(RuleError::InvalidAdultRule),
            Self::RequireGuardianForMinors { minor_below_age } if *minor_below_age == 0 => {
                Err(RuleError::InvalidGuardianRule)
            }
            Self::And(children) | Self::Or(children) => {
                if children.is_empty() {
                    return Err(RuleError::EmptyBooleanRule);
                }
                for child in children {
                    child.validate_inner(nodes)?;
                }
                Ok(())
            }
            Self::Not(child) => child.validate_inner(nodes),
            _ => Ok(()),
        }
    }

    pub fn evaluate(&self, context: &OccupancyContext) -> Result<bool, RuleError> {
        self.validate()?;
        Ok(self.evaluate_validated(context))
    }

    fn evaluate_validated(&self, context: &OccupancyContext) -> bool {
        match self {
            Self::Capacity { min, max } => {
                let count = context.occupants.len();
                (usize::from(*min)..=usize::from(*max)).contains(&count)
            }
            Self::AgeRangeCount {
                min_age,
                max_age,
                min_count,
                max_count,
            } => {
                let count = context.count_age_range(*min_age, *max_age);
                (usize::from(*min_count)..=usize::from(*max_count)).contains(&count)
            }
            Self::RequireAdult {
                adult_age,
                min_adults,
            } => {
                let count = context
                    .occupants
                    .iter()
                    .filter(|occupant| occupant.age >= *adult_age)
                    .count();
                count >= usize::from(*min_adults)
            }
            Self::RequireGuardianForMinors { minor_below_age } => context
                .occupants
                .iter()
                .all(|occupant| {
                    occupant.age >= *minor_below_age || occupant.authorized_guardian_present
                }),
            Self::And(children) => children
                .iter()
                .all(|child| child.evaluate_validated(context)),
            Self::Or(children) => children
                .iter()
                .any(|child| child.evaluate_validated(context)),
            Self::Not(child) => !child.evaluate_validated(context),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleError {
    InvalidOccupantCount(usize),
    DuplicateTraveler(TravelerId),
    InvalidCountRange,
    InvalidAdultRule,
    InvalidGuardianRule,
    EmptyBooleanRule,
    RuleTooComplex,
    CountOverflow,
}

impl fmt::Display for RuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for RuleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn occupant(id: u32, age: u16, guardian: bool) -> Occupant {
        Occupant {
            traveler_id: TravelerId::new(id),
            age,
            authorized_guardian_present: guardian,
        }
    }

    #[test]
    fn hard_rules_evaluate_deterministically() {
        let context = OccupancyContext::new(vec![occupant(1, 40, false), occupant(2, 8, true)])
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            Rule::Capacity { min: 1, max: 2 }.evaluate(&context),
            Ok(true)
        );
        assert_eq!(
            Rule::RequireAdult {
                adult_age: 18,
                min_adults: 1
            }
            .evaluate(&context),
            Ok(true)
        );
        assert_eq!(
            Rule::AgeRangeCount {
                min_age: 0,
                max_age: 17,
                min_count: 1,
                max_count: 1
            }
            .evaluate(&context),
            Ok(true)
        );
        assert_eq!(
            Rule::RequireGuardianForMinors {
                minor_below_age: 18
            }
            .evaluate(&context),
            Ok(true)
        );
        assert_eq!(context.occupants().len(), 2);
    }

    #[test]
    fn guardian_rule_fails_closed_for_unsupervised_minor() {
        let context =
            OccupancyContext::new(vec![occupant(1, 12, false)]).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            Rule::RequireGuardianForMinors {
                minor_below_age: 18
            }
            .evaluate(&context),
            Ok(false)
        );
    }

    #[test]
    fn boolean_ir_composes_without_dynamic_code() {
        let context =
            OccupancyContext::new(vec![occupant(1, 30, false)]).unwrap_or_else(|_| unreachable!());
        let rule = Rule::And(vec![
            Rule::Capacity { min: 1, max: 2 },
            Rule::Or(vec![
                Rule::RequireAdult {
                    adult_age: 18,
                    min_adults: 1,
                },
                Rule::Not(Box::new(Rule::Capacity { min: 1, max: 1 })),
            ]),
        ]);
        assert_eq!(rule.evaluate(&context), Ok(true));
    }

    #[test]
    fn invalid_ir_is_rejected_before_execution() {
        assert_eq!(
            Rule::Capacity { min: 2, max: 1 }.validate(),
            Err(RuleError::InvalidCountRange)
        );
        assert_eq!(
            Rule::AgeRangeCount {
                min_age: 10,
                max_age: 5,
                min_count: 0,
                max_count: 1
            }
            .validate(),
            Err(RuleError::InvalidCountRange)
        );
        assert_eq!(
            Rule::RequireAdult {
                adult_age: 0,
                min_adults: 1
            }
            .validate(),
            Err(RuleError::InvalidAdultRule)
        );
        assert_eq!(
            Rule::RequireGuardianForMinors { minor_below_age: 0 }.validate(),
            Err(RuleError::InvalidGuardianRule)
        );
        assert_eq!(
            Rule::And(Vec::new()).validate(),
            Err(RuleError::EmptyBooleanRule)
        );
        assert_eq!(
            Rule::Or(Vec::new()).validate(),
            Err(RuleError::EmptyBooleanRule)
        );
    }

    #[test]
    fn occupancy_context_validates_identity_and_bounds() {
        assert_eq!(
            OccupancyContext::new(Vec::new()),
            Err(RuleError::InvalidOccupantCount(0))
        );
        assert_eq!(
            OccupancyContext::new(vec![occupant(1, 1, false), occupant(1, 2, false)]),
            Err(RuleError::DuplicateTraveler(TravelerId::new(1)))
        );
        let context = OccupancyContext::new(vec![
            occupant(1, 5, false),
            occupant(2, 20, false),
            occupant(3, 70, false),
        ])
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(context.count_age_range(0, 17), 1);
        assert_eq!(context.count_age_range(18, 130), 2);
    }

    #[test]
    fn complexity_is_bounded() {
        let mut rule = Rule::Capacity { min: 1, max: 1 };
        for _ in 0..MAX_RULE_NODES {
            rule = Rule::Not(Box::new(rule));
        }
        assert_eq!(rule.validate(), Err(RuleError::RuleTooComplex));
    }

    proptest! {
        #[test]
        fn capacity_matches_reference(count in 1_usize..MAX_OCCUPANTS, min in 0_u16..10, max in 0_u16..10) {
            let occupants = (0..count)
                .map(|id| occupant(u32::try_from(id).unwrap_or_default(), 20, false))
                .collect();
            let context = OccupancyContext::new(occupants).unwrap_or_else(|_| unreachable!());
            let rule = Rule::Capacity { min, max };
            if min <= max {
                let count_u16 = u16::try_from(count).unwrap_or_default();
                prop_assert_eq!(rule.evaluate(&context), Ok((min..=max).contains(&count_u16)));
            } else {
                prop_assert_eq!(rule.evaluate(&context), Err(RuleError::InvalidCountRange));
            }
        }
    }
}
