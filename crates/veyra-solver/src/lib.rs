#![forbid(unsafe_code)]

//! Deterministic bounded multi-room solver for complex parties.
//!
//! The solver is intentionally not a general CSP engine. It enumerates a tightly bounded
//! assignment space, validates every complete assignment with Veyra hard constraints, and fails
//! closed if the configured state budget cannot prove the optimum.

use core::fmt;
use std::collections::BTreeSet;
use veyra_occupancy::validate_room;
use veyra_party::{
    BookingParty, CivilDate, ConstraintStrength, RoomingRelation, TravelerId,
};
use veyra_pricing::{MoneyMicros, PricingError};
use veyra_rule_compiler::CompiledRule;

pub const HARD_MAX_TRAVELERS: usize = 16;
pub const HARD_MAX_ROOMS: usize = 8;
pub const HARD_MAX_STATES: usize = 200_000;
pub const HARD_MAX_SOLUTIONS: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomOffer {
    pub room_id: u32,
    pub projected_price: MoneyMicros,
    pub floor: u16,
    pub building: u16,
    pub adult_age: u16,
    pub occupancy_rule: CompiledRule,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SolverConfig {
    pub max_states: usize,
    pub max_solutions: usize,
}

impl Default for SolverConfig {
    fn default() -> Self {
        Self {
            max_states: 50_000,
            max_solutions: 2_000,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RoomAllocation {
    pub room_id: u32,
    pub travelers: Vec<TravelerId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaySolution {
    pub rooms: Vec<RoomAllocation>,
    pub total_price: MoneyMicros,
    pub soft_penalty: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolutionProfile {
    Cheapest,
    FewestRooms,
    BestFamilyLayout,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaggedSolution {
    pub profile: SolutionProfile,
    pub solution: StaySolution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolverResult {
    pub explored_states: usize,
    pub valid_solution_count: usize,
    pub solutions: Vec<TaggedSolution>,
}

pub fn solve(
    party: &BookingParty,
    check_in: CivilDate,
    offers: &[RoomOffer],
    config: SolverConfig,
) -> Result<SolverResult, SolverError> {
    let travelers = party
        .travelers()
        .map(|traveler| traveler.id())
        .collect::<Vec<_>>();
    validate_input(party, &travelers, offers, config)?;

    let mut context = SearchContext {
        party,
        check_in,
        offers,
        config,
        travelers,
        assignments: Vec::new(),
        explored_states: 0,
        valid: Vec::new(),
    };
    search(&mut context, 0)?;

    if context.valid.is_empty() {
        return Ok(SolverResult {
            explored_states: context.explored_states,
            valid_solution_count: 0,
            solutions: Vec::new(),
        });
    }

    context.valid.sort_by_key(solution_canonical_key);
    context.valid.dedup();
    let valid_solution_count = context.valid.len();
    if valid_solution_count > config.max_solutions {
        return Err(SolverError::TooManyValidSolutions(valid_solution_count));
    }

    let cheapest = context
        .valid
        .iter()
        .min_by_key(|solution| {
            (
                solution.total_price,
                solution.rooms.len(),
                solution.soft_penalty,
                solution.rooms.clone(),
            )
        })
        .cloned()
        .ok_or(SolverError::InternalInvariant)?;
    let fewest = context
        .valid
        .iter()
        .min_by_key(|solution| {
            (
                solution.rooms.len(),
                solution.total_price,
                solution.soft_penalty,
                solution.rooms.clone(),
            )
        })
        .cloned()
        .ok_or(SolverError::InternalInvariant)?;
    let family = context
        .valid
        .iter()
        .min_by_key(|solution| {
            (
                solution.soft_penalty,
                solution.total_price,
                solution.rooms.len(),
                solution.rooms.clone(),
            )
        })
        .cloned()
        .ok_or(SolverError::InternalInvariant)?;

    Ok(SolverResult {
        explored_states: context.explored_states,
        valid_solution_count,
        solutions: vec![
            TaggedSolution {
                profile: SolutionProfile::Cheapest,
                solution: cheapest,
            },
            TaggedSolution {
                profile: SolutionProfile::FewestRooms,
                solution: fewest,
            },
            TaggedSolution {
                profile: SolutionProfile::BestFamilyLayout,
                solution: family,
            },
        ],
    })
}

fn validate_input(
    party: &BookingParty,
    travelers: &[TravelerId],
    offers: &[RoomOffer],
    config: SolverConfig,
) -> Result<(), SolverError> {
    if travelers.is_empty() {
        return Err(SolverError::EmptyParty);
    }
    if travelers.len() > HARD_MAX_TRAVELERS {
        return Err(SolverError::TooManyTravelers(travelers.len()));
    }
    if offers.is_empty() || offers.len() > HARD_MAX_ROOMS {
        return Err(SolverError::InvalidRoomCount(offers.len()));
    }
    if config.max_states == 0
        || config.max_states > HARD_MAX_STATES
        || config.max_solutions == 0
        || config.max_solutions > HARD_MAX_SOLUTIONS
    {
        return Err(SolverError::InvalidBudget);
    }
    let mut room_ids = BTreeSet::new();
    for offer in offers {
        if offer.adult_age == 0 {
            return Err(SolverError::InvalidAdultAge(offer.room_id));
        }
        if offer.projected_price.get() < 0 {
            return Err(SolverError::NegativeRoomPrice(offer.room_id));
        }
        if !room_ids.insert(offer.room_id) {
            return Err(SolverError::DuplicateRoomId(offer.room_id));
        }
    }
    for intent in party
        .rooming_intents()
        .iter()
        .filter(|intent| intent.strength == ConstraintStrength::Must)
    {
        if matches!(
            intent.relation,
            RoomingRelation::Near
                | RoomingRelation::ConnectedRooms
                | RoomingRelation::AdjacentRooms
        ) {
            return Err(SolverError::UnsupportedHardConstraint(intent.relation));
        }
    }
    Ok(())
}

struct SearchContext<'a> {
    party: &'a BookingParty,
    check_in: CivilDate,
    offers: &'a [RoomOffer],
    config: SolverConfig,
    travelers: Vec<TravelerId>,
    assignments: Vec<usize>,
    explored_states: usize,
    valid: Vec<StaySolution>,
}

fn search(context: &mut SearchContext<'_>, traveler_index: usize) -> Result<(), SolverError> {
    if traveler_index == context.travelers.len() {
        return evaluate_leaf(context);
    }

    for room_index in 0..context.offers.len() {
        context.explored_states = context
            .explored_states
            .checked_add(1)
            .ok_or(SolverError::StateBudgetExhausted)?;
        if context.explored_states > context.config.max_states {
            return Err(SolverError::StateBudgetExhausted);
        }
        context.assignments.push(room_index);
        search(context, traveler_index + 1)?;
        context.assignments.pop();
    }
    Ok(())
}

fn evaluate_leaf(context: &mut SearchContext<'_>) -> Result<(), SolverError> {
    if !layout_hard_constraints_hold(context)? {
        return Ok(());
    }

    let mut rooms = Vec::new();
    let mut total_price = MoneyMicros::signed(0);
    for (room_index, offer) in context.offers.iter().enumerate() {
        let assigned = context
            .assignments
            .iter()
            .enumerate()
            .filter_map(|(traveler_index, assigned_room)| {
                (*assigned_room == room_index).then_some(context.travelers[traveler_index])
            })
            .collect::<Vec<_>>();
        if assigned.is_empty() {
            continue;
        }
        if validate_room(
            &offer.occupancy_rule,
            context.party,
            context.check_in,
            &assigned,
            offer.adult_age,
        )
        .is_err()
        {
            return Ok(());
        }
        total_price = total_price
            .checked_add(offer.projected_price)
            .map_err(SolverError::Price)?;
        let mut assigned = assigned;
        assigned.sort_unstable();
        rooms.push(RoomAllocation {
            room_id: offer.room_id,
            travelers: assigned,
        });
    }
    rooms.sort_unstable();
    let soft_penalty = layout_soft_penalty(context)?;
    context.valid.push(StaySolution {
        rooms,
        total_price,
        soft_penalty,
    });
    Ok(())
}

fn layout_hard_constraints_hold(context: &SearchContext<'_>) -> Result<bool, SolverError> {
    for intent in context
        .party
        .rooming_intents()
        .iter()
        .filter(|intent| intent.strength == ConstraintStrength::Must)
    {
        let left = assignment_of(context, intent.left)?;
        let right = assignment_of(context, intent.right)?;
        let left_offer = &context.offers[left];
        let right_offer = &context.offers[right];
        let valid = match intent.relation {
            RoomingRelation::SameRoom => left == right,
            RoomingRelation::SeparateRoom => left != right,
            RoomingRelation::SameFloor => left_offer.floor == right_offer.floor,
            RoomingRelation::SameBuilding => left_offer.building == right_offer.building,
            relation => return Err(SolverError::UnsupportedHardConstraint(relation)),
        };
        if !valid {
            return Ok(false);
        }
    }
    Ok(true)
}

fn layout_soft_penalty(context: &SearchContext<'_>) -> Result<u32, SolverError> {
    let mut penalty = 0_u32;
    for intent in context
        .party
        .rooming_intents()
        .iter()
        .filter(|intent| intent.strength != ConstraintStrength::Must)
    {
        let left = assignment_of(context, intent.left)?;
        let right = assignment_of(context, intent.right)?;
        let left_offer = &context.offers[left];
        let right_offer = &context.offers[right];
        let satisfied = match intent.relation {
            RoomingRelation::SameRoom => left == right,
            RoomingRelation::SeparateRoom => left != right,
            RoomingRelation::SameFloor => left_offer.floor == right_offer.floor,
            RoomingRelation::SameBuilding => left_offer.building == right_offer.building,
            relation => return Err(SolverError::UnsupportedPreference(relation)),
        };
        let violated = match intent.strength {
            ConstraintStrength::Prefer => !satisfied,
            ConstraintStrength::Avoid => satisfied,
            ConstraintStrength::Must => false,
        };
        if violated {
            penalty = penalty
                .checked_add(1)
                .ok_or(SolverError::PenaltyOverflow)?;
        }
    }
    Ok(penalty)
}

fn assignment_of(
    context: &SearchContext<'_>,
    traveler: TravelerId,
) -> Result<usize, SolverError> {
    let position = context
        .travelers
        .iter()
        .position(|id| *id == traveler)
        .ok_or(SolverError::UnknownTraveler(traveler))?;
    context
        .assignments
        .get(position)
        .copied()
        .ok_or(SolverError::InternalInvariant)
}

fn solution_canonical_key(
    solution: &StaySolution,
) -> (MoneyMicros, usize, u32, Vec<RoomAllocation>) {
    (
        solution.total_price,
        solution.rooms.len(),
        solution.soft_penalty,
        solution.rooms.clone(),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolverError {
    EmptyParty,
    TooManyTravelers(usize),
    InvalidRoomCount(usize),
    InvalidBudget,
    InvalidAdultAge(u32),
    NegativeRoomPrice(u32),
    DuplicateRoomId(u32),
    StateBudgetExhausted,
    TooManyValidSolutions(usize),
    UnsupportedHardConstraint(RoomingRelation),
    UnsupportedPreference(RoomingRelation),
    UnknownTraveler(TravelerId),
    Price(PricingError),
    PenaltyOverflow,
    InternalInvariant,
}

impl fmt::Display for SolverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for SolverError {}

#[cfg(test)]
mod tests {
    use super::*;
    use veyra_party::{AgeEvidence, GuardianRelationship, RoomingIntent, Traveler};
    use veyra_pricing::MoneyMicros;
    use veyra_rule_compiler::{compile, RULE_SCHEMA_V1};
    use veyra_rules::Rule;

    fn date(y: i32, m: u8, d: u8) -> CivilDate {
        CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
    }

    fn policy(capacity: u16) -> CompiledRule {
        compile(
            RULE_SCHEMA_V1,
            &Rule::And(vec![
                Rule::Capacity {
                    min: 1,
                    max: capacity,
                },
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
        builder
            .add_rooming_intent(RoomingIntent {
                left: TravelerId::new(1),
                right: TravelerId::new(3),
                strength: ConstraintStrength::Must,
                relation: RoomingRelation::SameRoom,
            })
            .unwrap_or_else(|_| unreachable!());
        builder.build().unwrap_or_else(|_| unreachable!())
    }

    fn room(id: u32, price: i64, floor: u16) -> RoomOffer {
        RoomOffer {
            room_id: id,
            projected_price: MoneyMicros::try_nonnegative(price)
                .unwrap_or_else(|_| unreachable!()),
            floor,
            building: 1,
            adult_age: 18,
            occupancy_rule: policy(2),
        }
    }

    #[test]
    fn exact_solver_preserves_guardian_and_must_same_room() {
        let result = solve(
            &party(),
            date(2026, 9, 21),
            &[room(10, 100, 1), room(11, 80, 1)],
            SolverConfig::default(),
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(result.solutions.len(), 3);
        assert!(result.valid_solution_count > 0);
        let cheapest = result
            .solutions
            .iter()
            .find(|solution| solution.profile == SolutionProfile::Cheapest)
            .unwrap_or_else(|| unreachable!());
        let child_room = cheapest
            .solution
            .rooms
            .iter()
            .find(|room| room.travelers.contains(&TravelerId::new(3)))
            .unwrap_or_else(|| unreachable!());
        assert!(child_room.travelers.contains(&TravelerId::new(1)));
        assert_eq!(cheapest.solution.rooms.len(), 2);
    }

    #[test]
    fn soft_preference_changes_family_profile_not_validity() {
        let mut builder = BookingParty::builder();
        for traveler in [
            Traveler::new(TravelerId::new(1), AgeEvidence::AgeAtCheckIn(30), false),
            Traveler::new(TravelerId::new(2), AgeEvidence::AgeAtCheckIn(30), false),
        ] {
            builder
                .add_traveler(traveler)
                .unwrap_or_else(|_| unreachable!());
        }
        builder
            .add_rooming_intent(RoomingIntent {
                left: TravelerId::new(1),
                right: TravelerId::new(2),
                strength: ConstraintStrength::Prefer,
                relation: RoomingRelation::SameFloor,
            })
            .unwrap_or_else(|_| unreachable!());
        let party = builder.build().unwrap_or_else(|_| unreachable!());
        let result = solve(
            &party,
            date(2026, 1, 1),
            &[room(1, 50, 1), room(2, 60, 2), room(3, 100, 1)],
            SolverConfig::default(),
        )
        .unwrap_or_else(|_| unreachable!());
        let family = result
            .solutions
            .iter()
            .find(|solution| solution.profile == SolutionProfile::BestFamilyLayout)
            .unwrap_or_else(|| unreachable!());
        assert_eq!(family.solution.soft_penalty, 0);
    }

    #[test]
    fn exhausted_budget_returns_no_unproven_optimum() {
        assert_eq!(
            solve(
                &party(),
                date(2026, 9, 21),
                &[room(1, 1, 1), room(2, 1, 1)],
                SolverConfig {
                    max_states: 1,
                    max_solutions: 10,
                },
            ),
            Err(SolverError::StateBudgetExhausted)
        );
    }

    #[test]
    fn unsupported_topology_fails_closed() {
        let mut builder = BookingParty::builder();
        for traveler in [
            Traveler::new(TravelerId::new(1), AgeEvidence::AgeAtCheckIn(30), false),
            Traveler::new(TravelerId::new(2), AgeEvidence::AgeAtCheckIn(30), false),
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
                relation: RoomingRelation::ConnectedRooms,
            })
            .unwrap_or_else(|_| unreachable!());
        let party = builder.build().unwrap_or_else(|_| unreachable!());
        assert_eq!(
            solve(
                &party,
                date(2026, 1, 1),
                &[room(1, 1, 1), room(2, 1, 1)],
                SolverConfig::default(),
            ),
            Err(SolverError::UnsupportedHardConstraint(
                RoomingRelation::ConnectedRooms
            ))
        );
    }

    #[test]
    fn invalid_input_bounds_are_explicit() {
        let empty = BookingParty::builder().build();
        assert!(empty.is_err());
        assert_eq!(
            solve(
                &party(),
                date(2026, 1, 1),
                &[],
                SolverConfig::default(),
            ),
            Err(SolverError::InvalidRoomCount(0))
        );
        assert_eq!(
            solve(
                &party(),
                date(2026, 1, 1),
                &[room(1, 1, 1)],
                SolverConfig {
                    max_states: 0,
                    max_solutions: 1,
                },
            ),
            Err(SolverError::InvalidBudget)
        );
        assert_eq!(SolverError::InvalidBudget.to_string(), "InvalidBudget");
    }
}
