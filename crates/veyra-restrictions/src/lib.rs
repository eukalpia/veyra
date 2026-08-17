#![forbid(unsafe_code)]

//! Versioned deterministic stay restrictions.
//!
//! Human-facing policy is compiled once into bounded sets. Query execution performs only
//! integer interval arithmetic and membership lookups.

use core::fmt;
use std::collections::BTreeSet;

pub const RESTRICTION_SCHEMA_V1: u16 = 1;
pub const MAX_RESTRICTION_RULES: usize = 512;
pub const MAX_STAY_NIGHTS: u16 = 365;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestrictionRule {
    MinStay(u16),
    MaxStay(u16),
    ClosedToArrival(i32),
    ClosedToDeparture(i32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledRestrictions {
    schema_version: u16,
    min_stay: u16,
    max_stay: u16,
    closed_to_arrival: BTreeSet<i32>,
    closed_to_departure: BTreeSet<i32>,
}

impl CompiledRestrictions {
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    #[must_use]
    pub const fn min_stay(&self) -> u16 {
        self.min_stay
    }

    #[must_use]
    pub const fn max_stay(&self) -> u16 {
        self.max_stay
    }

    pub fn validate_stay(&self, check_in: i32, check_out: i32) -> Result<u16, RestrictionError> {
        let nights = i64::from(check_out) - i64::from(check_in);
        if nights <= 0 {
            return Err(RestrictionError::InvalidStayInterval);
        }
        let nights = u16::try_from(nights).map_err(|_| RestrictionError::StayTooLong)?;
        if nights < self.min_stay {
            return Err(RestrictionError::BelowMinStay {
                nights,
                minimum: self.min_stay,
            });
        }
        if nights > self.max_stay {
            return Err(RestrictionError::AboveMaxStay {
                nights,
                maximum: self.max_stay,
            });
        }
        if self.closed_to_arrival.contains(&check_in) {
            return Err(RestrictionError::ClosedToArrival(check_in));
        }
        if self.closed_to_departure.contains(&check_out) {
            return Err(RestrictionError::ClosedToDeparture(check_out));
        }
        Ok(nights)
    }
}

pub fn compile(
    schema_version: u16,
    rules: &[RestrictionRule],
) -> Result<CompiledRestrictions, RestrictionError> {
    if schema_version != RESTRICTION_SCHEMA_V1 {
        return Err(RestrictionError::UnsupportedSchema(schema_version));
    }
    if rules.len() > MAX_RESTRICTION_RULES {
        return Err(RestrictionError::TooManyRules(rules.len()));
    }

    let mut min_stay = None;
    let mut max_stay = None;
    let mut cta = BTreeSet::new();
    let mut ctd = BTreeSet::new();
    for rule in rules {
        match *rule {
            RestrictionRule::MinStay(value) => set_once(&mut min_stay, value, "MIN_STAY")?,
            RestrictionRule::MaxStay(value) => set_once(&mut max_stay, value, "MAX_STAY")?,
            RestrictionRule::ClosedToArrival(day) => {
                cta.insert(day);
            }
            RestrictionRule::ClosedToDeparture(day) => {
                ctd.insert(day);
            }
        }
    }

    let min_stay = min_stay.ok_or(RestrictionError::MissingRule("MIN_STAY"))?;
    let max_stay = max_stay.ok_or(RestrictionError::MissingRule("MAX_STAY"))?;
    if min_stay == 0 || max_stay == 0 || min_stay > max_stay || max_stay > MAX_STAY_NIGHTS {
        return Err(RestrictionError::InvalidStayBounds {
            minimum: min_stay,
            maximum: max_stay,
        });
    }

    Ok(CompiledRestrictions {
        schema_version,
        min_stay,
        max_stay,
        closed_to_arrival: cta,
        closed_to_departure: ctd,
    })
}

fn set_once<T: Copy>(
    slot: &mut Option<T>,
    value: T,
    name: &'static str,
) -> Result<(), RestrictionError> {
    if slot.replace(value).is_some() {
        Err(RestrictionError::DuplicateRule(name))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestrictionError {
    UnsupportedSchema(u16),
    TooManyRules(usize),
    MissingRule(&'static str),
    DuplicateRule(&'static str),
    InvalidStayBounds { minimum: u16, maximum: u16 },
    InvalidStayInterval,
    StayTooLong,
    BelowMinStay { nights: u16, minimum: u16 },
    AboveMaxStay { nights: u16, maximum: u16 },
    ClosedToArrival(i32),
    ClosedToDeparture(i32),
}

impl fmt::Display for RestrictionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for RestrictionError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> CompiledRestrictions {
        compile(
            RESTRICTION_SCHEMA_V1,
            &[
                RestrictionRule::MinStay(2),
                RestrictionRule::MaxStay(5),
                RestrictionRule::ClosedToArrival(10),
                RestrictionRule::ClosedToDeparture(20),
            ],
        )
        .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn compiler_builds_versioned_restriction_sets() {
        let policy = policy();
        assert_eq!(policy.schema_version(), RESTRICTION_SCHEMA_V1);
        assert_eq!(policy.min_stay(), 2);
        assert_eq!(policy.max_stay(), 5);
        assert_eq!(policy.validate_stay(11, 14), Ok(3));
    }

    #[test]
    fn every_hard_restriction_fails_closed() {
        let policy = policy();
        assert_eq!(
            policy.validate_stay(1, 1),
            Err(RestrictionError::InvalidStayInterval)
        );
        assert_eq!(
            policy.validate_stay(1, 2),
            Err(RestrictionError::BelowMinStay {
                nights: 1,
                minimum: 2,
            })
        );
        assert_eq!(
            policy.validate_stay(1, 7),
            Err(RestrictionError::AboveMaxStay {
                nights: 6,
                maximum: 5,
            })
        );
        assert_eq!(
            policy.validate_stay(10, 12),
            Err(RestrictionError::ClosedToArrival(10))
        );
        assert_eq!(
            policy.validate_stay(18, 20),
            Err(RestrictionError::ClosedToDeparture(20))
        );
    }

    #[test]
    fn unknown_incomplete_duplicate_and_invalid_policy_is_rejected() {
        assert_eq!(compile(9, &[]), Err(RestrictionError::UnsupportedSchema(9)));
        assert_eq!(
            compile(RESTRICTION_SCHEMA_V1, &[]),
            Err(RestrictionError::MissingRule("MIN_STAY"))
        );
        assert_eq!(
            compile(
                RESTRICTION_SCHEMA_V1,
                &[
                    RestrictionRule::MinStay(1),
                    RestrictionRule::MinStay(2),
                    RestrictionRule::MaxStay(5),
                ],
            ),
            Err(RestrictionError::DuplicateRule("MIN_STAY"))
        );
        assert_eq!(
            compile(
                RESTRICTION_SCHEMA_V1,
                &[RestrictionRule::MinStay(6), RestrictionRule::MaxStay(5)],
            ),
            Err(RestrictionError::InvalidStayBounds {
                minimum: 6,
                maximum: 5,
            })
        );
    }

    #[test]
    fn rule_and_error_bounds_are_explicit() {
        let rules = vec![RestrictionRule::ClosedToArrival(1); MAX_RESTRICTION_RULES + 1];
        assert_eq!(
            compile(RESTRICTION_SCHEMA_V1, &rules),
            Err(RestrictionError::TooManyRules(MAX_RESTRICTION_RULES + 1))
        );
        assert_eq!(
            RestrictionError::InvalidStayInterval.to_string(),
            "InvalidStayInterval"
        );
    }
}
