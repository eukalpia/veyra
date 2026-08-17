#![forbid(unsafe_code)]

//! Deterministic bounded multi-room solver for complex parties.
//!
//! The solver is intentionally not a general CSP engine. It enumerates a tightly bounded
//! assignment space, validates every complete assignment with Veyra hard constraints, and fails
//! closed if the configured state budget cannot prove the optimum.

use core::{cmp::Ordering, fmt};
use std::collections::BTreeSet;
use veyra_occupancy::{OccupancyReport, validate_room};
use veyra_party::{BookingParty, CivilDate, ConstraintStrength, RoomingRelation, TravelerId};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector, PricingError};
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

/// Room projection whose final price depends on the occupancy chosen by the solver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PricedRoomOffer {
    pub room_id: u32,
    pub prices: PriceVector,
    pub occupancy_adjustment: OccupancyAdjustment,
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
    let prepared = offers
        .iter()
        .map(|offer| PreparedOffer {
            room_id: offer.room_id,
            floor: offer.floor,
            building: offer.building,
            adult_age: offer.adult_age,
            occupancy_rule: &offer.occupancy_rule,
            price: PriceSource::Static(offer.projected_price),
        })
        .collect::<Vec<_>>();
    solve_prepared(party, check_in, prepared, config)
}

/// Solves a multi-room stay and computes each used room price from the actual occupancy assigned
/// to that room. This is the pricing-safe entry point for family and group search.
pub fn solve_priced(
    party: &BookingParty,
    check_in: CivilDate,
    check_in_day: i32,
    check_out_day: i32,
    offers: &[PricedRoomOffer],
    config: SolverConfig,
) -> Result<SolverResult, SolverError> {
    if check_out_day <= check_in_day {
        return Err(SolverError::InvalidStayRange);
    }
    let window = PricingWindow {
        check_in_day,
        check_out_day,
    };
    let prepared = offers
        .iter()
        .map(|offer| PreparedOffer {
            room_id: offer.room_id,
            floor: offer.floor,
            building: offer.building,
            adult_age: offer.adult_age,
            occupancy_rule: &offer.occupancy_rule,
            price: PriceSource::Dynamic {
                prices: &offer.prices,
                adjustment: offer.occupancy_adjustment,
                window,
            },
        })
        .collect::<Vec<_>>();
    solve_prepared(party, check_in, prepared, config)
}

#[derive(Clone, Copy)]
struct PricingWindow {
    check_in_day: i32,
    check_out_day: i32,
}

#[derive(Clone, Copy)]
enum PriceSource<'a> {
    Static(MoneyMicros),
    Dynamic {
        prices: &'a PriceVector,
        adjustment: OccupancyAdjustment,
        window: PricingWindow,
    },
}

struct PreparedOffer<'a> {
    room_id: u32,
    floor: u16,
    building: u16,
    adult_age: u16,
    occupancy_rule: &'a CompiledRule,
    price: PriceSource<'a>,
}

impl PreparedOffer<'_> {
    fn price_for(&self, occupancy: OccupancyReport) -> Result<MoneyMicros, SolverError> {
        match self.price {
            PriceSource::Static(price) => Ok(price),
            PriceSource::Dynamic {
                prices,
                adjustment,
                window,
            } => prices
                .quote(
                    window.check_in_day,
                    window.check_out_day,
                    occupancy.adult_count,
                    occupancy.child_count,
                    adjustment,
                )
                .map(|projected| projected.total)
                .map_err(SolverError::Price),
        }
    }
}

fn solve_prepared(
    party: &BookingParty,
    check_in: CivilDate,
    offers: Vec<PreparedOffer<'_>>,
    config: SolverConfig,
) -> Result<SolverResult, SolverError> {
    let travelers = party
        .travelers()
        .map(veyra_party::Traveler::id)
        .collect::<Vec<_>>();
    validate_input(party, &travelers, &offers, config)?;

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

    context.valid.sort_by(compare_canonical);
    context.valid.dedup();
    let valid_solution_count = context.valid.len();
    if valid_solution_count > config.max_solutions {
        return Err(SolverError::TooManyValidSolutions(valid_solution_count));
    }

    let cheapest = select_best(&context.valid, compare_cheapest);
    let fewest = select_best(&context.valid, compare_fewest_rooms);
    let family = select_best(&context.valid, compare_family_layout);

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
    offers: &[PreparedOffer<'_>],
    config: SolverConfig,
) -> Result<(), SolverError> {
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
        if matches!(offer.price, PriceSource::Static(price) if price.get() < 0) {
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
    offers: Vec<PreparedOffer<'a>>,
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
        context.explored_states += 1;
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
        let Ok(occupancy) = validate_room(
            offer.occupancy_rule,
            context.party,
            context.check_in,
            &assigned,
            offer.adult_age,
        ) else {
            return Ok(());
        };
        let room_price = offer.price_for(occupancy)?;
        total_price = total_price
            .checked_add(room_price)
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
        let violated = if intent.strength == ConstraintStrength::Prefer {
            !satisfied
        } else {
            satisfied
        };
        if violated {
            penalty = penalty.checked_add(1).ok_or(SolverError::PenaltyOverflow)?;
        }
    }
    Ok(penalty)
}

fn assignment_of(context: &SearchContext<'_>, traveler: TravelerId) -> Result<usize, SolverError> {
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

fn select_best(
    solutions: &[StaySolution],
    compare: fn(&StaySolution, &StaySolution) -> Ordering,
) -> StaySolution {
    let mut best = &solutions[0];
    for solution in &solutions[1..] {
        if compare(solution, best).is_lt() {
            best = solution;
        }
    }
    best.clone()
}

fn compare_canonical(left: &StaySolution, right: &StaySolution) -> Ordering {
    left.total_price
        .cmp(&right.total_price)
        .then_with(|| left.rooms.len().cmp(&right.rooms.len()))
        .then_with(|| left.soft_penalty.cmp(&right.soft_penalty))
        .then_with(|| left.rooms.cmp(&right.rooms))
}

fn compare_cheapest(left: &StaySolution, right: &StaySolution) -> Ordering {
    compare_canonical(left, right)
}

fn compare_fewest_rooms(left: &StaySolution, right: &StaySolution) -> Ordering {
    left.rooms
        .len()
        .cmp(&right.rooms.len())
        .then_with(|| left.total_price.cmp(&right.total_price))
        .then_with(|| left.soft_penalty.cmp(&right.soft_penalty))
        .then_with(|| left.rooms.cmp(&right.rooms))
}

fn compare_family_layout(left: &StaySolution, right: &StaySolution) -> Ordering {
    left.soft_penalty
        .cmp(&right.soft_penalty)
        .then_with(|| left.total_price.cmp(&right.total_price))
        .then_with(|| left.rooms.len().cmp(&right.rooms.len()))
        .then_with(|| left.rooms.cmp(&right.rooms))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolverError {
    EmptyParty,
    TooManyTravelers(usize),
    InvalidRoomCount(usize),
    InvalidBudget,
    InvalidStayRange,
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
    use veyra_rule_compiler::{RULE_SCHEMA_V1, compile};
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
            projected_price: MoneyMicros::try_nonnegative(price).unwrap_or_else(|_| unreachable!()),
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
            solve(&party(), date(2026, 1, 1), &[], SolverConfig::default(),),
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
