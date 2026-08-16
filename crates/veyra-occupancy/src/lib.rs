#![forbid(unsafe_code)]

//! Traveler-aware room occupancy validation.
//!
//! Age and guardian semantics are derived from the requested stay and explicit Party Graph;
//! relationship labels never silently become hard room requirements.

use core::fmt;
use std::collections::BTreeSet;
use veyra_party::{BookingParty, CivilDate, ConstraintStrength, RoomingRelation, TravelerId};
use veyra_rule_compiler::{CompileError, CompiledRule};
use veyra_rules::{Occupant, OccupancyContext, RuleError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OccupancyReport {
    pub occupant_count: u16,
    pub adult_count: u16,
    pub child_count: u16,
}

/// Validates one physical room against compiled hotel policy and party hard constraints.
pub fn validate_room(
    compiled_rule: &CompiledRule,
    party: &BookingParty,
    check_in: CivilDate,
    occupant_ids: &[TravelerId],
    adult_age: u16,
) -> Result<OccupancyReport, OccupancyError> {
    if occupant_ids.is_empty() {
        return Err(OccupancyError::EmptyRoom);
    }
    if adult_age == 0 {
        return Err(OccupancyError::InvalidAdultAge);
    }
    let occupant_set = occupant_ids.iter().copied().collect::<BTreeSet<_>>();
    if occupant_set.len() != occupant_ids.len() {
        return Err(OccupancyError::DuplicateOccupant);
    }

    enforce_rooming(party, &occupant_set)?;

    let mut occupants = Vec::with_capacity(occupant_ids.len());
    let mut adults = 0_u16;
    let mut children = 0_u16;
    for id in occupant_ids {
        let traveler = party
            .traveler(*id)
            .ok_or(OccupancyError::UnknownTraveler(*id))?;
        let age = traveler
            .age_evidence()
            .age_at(check_in)
            .map_err(|_| OccupancyError::InvalidAge(*id))?;
        if age >= adult_age {
            adults = adults
                .checked_add(1)
                .ok_or(OccupancyError::CountOverflow)?;
        } else {
            children = children
                .checked_add(1)
                .ok_or(OccupancyError::CountOverflow)?;
        }
        let authorized_guardian_present = party.guardians().iter().any(|edge| {
            edge.dependent == *id
                && edge.valid_for_rooming
                && occupant_set.contains(&edge.guardian)
        });
        occupants.push(Occupant {
            traveler_id: *id,
            age,
            authorized_guardian_present,
        });
    }

    let context = OccupancyContext::new(occupants).map_err(OccupancyError::InvalidContext)?;
    if !compiled_rule
        .evaluate(&context)
        .map_err(OccupancyError::CompiledRule)?
    {
        return Err(OccupancyError::PolicyRejected);
    }
    let occupant_count = adults
        .checked_add(children)
        .ok_or(OccupancyError::CountOverflow)?;
    Ok(OccupancyReport {
        occupant_count,
        adult_count: adults,
        child_count: children,
    })
}

fn enforce_rooming(
    party: &BookingParty,
    occupants: &BTreeSet<TravelerId>,
) -> Result<(), OccupancyError> {
    for intent in party
        .rooming_intents()
        .iter()
        .filter(|intent| intent.strength == ConstraintStrength::Must)
    {
        let left = occupants.contains(&intent.left);
        let right = occupants.contains(&intent.right);
        match intent.relation {
            RoomingRelation::SameRoom if left != right => {
                return Err(OccupancyError::MustStayTogether {
                    left: intent.left,
                    right: intent.right,
                });
            }
            RoomingRelation::SeparateRoom if left && right => {
                return Err(OccupancyError::MustStaySeparate {
                    left: intent.left,
                    right: intent.right,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OccupancyError {
    EmptyRoom,
    InvalidAdultAge,
    DuplicateOccupant,
    UnknownTraveler(TravelerId),
    InvalidAge(TravelerId),
    CountOverflow,
    MustStayTogether { left: TravelerId, right: TravelerId },
    MustStaySeparate { left: TravelerId, right: TravelerId },
    InvalidContext(RuleError),
    CompiledRule(CompileError),
    PolicyRejected,
}

impl fmt::Display for OccupancyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for OccupancyError {}

#[cfg(test)]
mod tests {
    use super::*;
    use veyra_party::{AgeEvidence, GuardianRelationship, RoomingIntent, Traveler};
    use veyra_rule_compiler::{compile, RULE_SCHEMA_V1};
    use veyra_rules::Rule;

    fn date(y: i32, m: u8, d: u8) -> CivilDate {
        CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
    }

    fn policy() -> CompiledRule {
        compile(
            RULE_SCHEMA_V1,
            &Rule::And(vec![
                Rule::Capacity { min: 1, max: 4 },
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

    fn party(with_guardian: bool) -> BookingParty {
        let mut builder = BookingParty::builder();
        for traveler in [
            Traveler::new(
                TravelerId::new(1),
                AgeEvidence::BirthDate(date(1988, 1, 1)),
                false,
            ),
            Traveler::new(
                TravelerId::new(2),
                AgeEvidence::BirthDate(date(1990, 1, 1)),
                false,
            ),
            Traveler::new(
                TravelerId::new(3),
                AgeEvidence::BirthDate(date(2018, 1, 1)),
                false,
            ),
        ] {
            builder
                .add_traveler(traveler)
                .unwrap_or_else(|_| unreachable!());
        }
        if with_guardian {
            builder
                .add_guardian(GuardianRelationship {
                    guardian: TravelerId::new(1),
                    dependent: TravelerId::new(3),
                    valid_for_rooming: true,
                })
                .unwrap_or_else(|_| unreachable!());
        }
        builder.build().unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn explicit_guardian_and_actual_age_make_room_valid() {
        let report = validate_room(
            &policy(),
            &party(true),
            date(2026, 9, 21),
            &[TravelerId::new(1), TravelerId::new(3)],
            18,
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            report,
            OccupancyReport {
                occupant_count: 2,
                adult_count: 1,
                child_count: 1,
            }
        );
    }

    #[test]
    fn relationships_do_not_replace_explicit_guardian() {
        assert_eq!(
            validate_room(
                &policy(),
                &party(false),
                date(2026, 9, 21),
                &[TravelerId::new(1), TravelerId::new(3)],
                18,
            ),
            Err(OccupancyError::PolicyRejected)
        );
    }

    #[test]
    fn must_rooming_constraints_dominate_policy() {
        let mut builder = BookingParty::builder();
        for traveler in [
            Traveler::new(TravelerId::new(1), AgeEvidence::AgeAtCheckIn(30), false),
            Traveler::new(TravelerId::new(2), AgeEvidence::AgeAtCheckIn(28), false),
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
                relation: RoomingRelation::SeparateRoom,
            })
            .unwrap_or_else(|_| unreachable!());
        let party = builder.build().unwrap_or_else(|_| unreachable!());
        assert!(matches!(
            validate_room(
                &policy(),
                &party,
                date(2026, 1, 1),
                &[TravelerId::new(1), TravelerId::new(2)],
                18,
            ),
            Err(OccupancyError::MustStaySeparate { .. })
        ));
    }

    #[test]
    fn invalid_room_inputs_fail_closed() {
        let party = party(true);
        let compiled = policy();
        assert_eq!(
            validate_room(&compiled, &party, date(2026, 1, 1), &[], 18),
            Err(OccupancyError::EmptyRoom)
        );
        assert_eq!(
            validate_room(
                &compiled,
                &party,
                date(2026, 1, 1),
                &[TravelerId::new(1)],
                0,
            ),
            Err(OccupancyError::InvalidAdultAge)
        );
        assert_eq!(
            validate_room(
                &compiled,
                &party,
                date(2026, 1, 1),
                &[TravelerId::new(1), TravelerId::new(1)],
                18,
            ),
            Err(OccupancyError::DuplicateOccupant)
        );
        assert_eq!(
            validate_room(
                &compiled,
                &party,
                date(2026, 1, 1),
                &[TravelerId::new(99)],
                18,
            ),
            Err(OccupancyError::UnknownTraveler(TravelerId::new(99)))
        );
    }
}
