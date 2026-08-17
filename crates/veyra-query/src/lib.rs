#![forbid(unsafe_code)]

//! Typed deterministic Veyra search pipeline.
//!
//! Veyra exposes both a fast single-room path and an exact bounded multi-room path.
//! Hard validity always precedes ranking and missing projection state fails the whole query closed
//! instead of silently hiding a potentially valid property.

use core::{cmp::Ordering, fmt};
use std::collections::BTreeMap;
use veyra_availability::{AvailabilityError, AvailabilityIndex, DenseRoomSet};
use veyra_occupancy::{OccupancyError, validate_room};
use veyra_party::{BookingParty, CivilDate, RoomingRelation};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector, PricingError};
use veyra_ranking::{
    MAX_TOP_K, RankCandidate, RankedCandidate, RankingError, RankingProfile, top_k,
};
use veyra_restrictions::{CompiledRestrictions, RestrictionError};
use veyra_rule_compiler::CompiledRule;
use veyra_solver::{
    HARD_MAX_ROOMS, HARD_MAX_TRAVELERS, PricedRoomOffer, RoomRelationIndex, SolverError,
    TopologyError, solve_priced, solve_priced_with_topology,
};
pub use veyra_solver::{
    RoomAllocation, RoomTopologyEdge, RoomTopologyRelation, SolutionProfile, SolverConfig,
};

pub const MAX_QUERY_PARTY: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomDocument {
    pub room_id: u32,
    pub property_id: u32,
    pub destination_id: u32,
    pub adult_age: u16,
    pub occupancy_rule: CompiledRule,
    pub restrictions: CompiledRestrictions,
    pub prices: PriceVector,
    pub occupancy_adjustment: OccupancyAdjustment,
    pub distance_meters: u32,
    pub quality_milli: u16,
    pub flexibility_milli: u16,
    pub family_penalty: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoomPlacement {
    pub room_id: u32,
    pub floor: u16,
    pub building: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomSpatialProjection {
    placements: Vec<RoomPlacement>,
    topology: RoomRelationIndex,
}

impl RoomSpatialProjection {
    pub fn try_new(
        placements: Vec<RoomPlacement>,
        edges: Vec<RoomTopologyEdge>,
    ) -> Result<Self, QueryError> {
        for (expected, placement) in (0_u32..).zip(&placements) {
            if placement.room_id != expected {
                return Err(QueryError::SpatialNonDenseRoomId {
                    expected,
                    actual: placement.room_id,
                });
            }
        }
        let room_ids = placements
            .iter()
            .map(|placement| placement.room_id)
            .collect::<Vec<_>>();
        let edges = edges.into_boxed_slice();
        let topology =
            RoomRelationIndex::try_new(&room_ids, &edges).map_err(QueryError::Topology)?;
        Ok(Self {
            placements,
            topology,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.placements.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    fn placement(&self, room_id: u32) -> Option<&RoomPlacement> {
        usize::try_from(room_id)
            .ok()
            .and_then(|index| self.placements.get(index))
    }

    fn topology(&self) -> &RoomRelationIndex {
        &self.topology
    }
}

#[derive(Debug)]
pub struct SearchEngine {
    availability: AvailabilityIndex,
    rooms: Vec<RoomDocument>,
    spatial: Option<RoomSpatialProjection>,
}

impl SearchEngine {
    pub fn try_new(
        availability: AvailabilityIndex,
        rooms: Vec<RoomDocument>,
    ) -> Result<Self, QueryError> {
        Self::try_new_internal(availability, rooms, None)
    }

    pub fn try_new_with_spatial(
        availability: AvailabilityIndex,
        rooms: Vec<RoomDocument>,
        spatial: RoomSpatialProjection,
    ) -> Result<Self, QueryError> {
        Self::try_new_internal(availability, rooms, Some(spatial))
    }

    fn try_new_internal(
        availability: AvailabilityIndex,
        rooms: Vec<RoomDocument>,
        spatial: Option<RoomSpatialProjection>,
    ) -> Result<Self, QueryError> {
        let expected = availability.room_count() as usize;
        if rooms.len() != expected {
            return Err(QueryError::RoomDocumentCountMismatch {
                expected,
                actual: rooms.len(),
            });
        }
        if let Some(projection) = &spatial
            && projection.len() != expected
        {
            return Err(QueryError::SpatialRoomCountMismatch {
                expected,
                actual: projection.len(),
            });
        }
        for (expected_id, room) in (0_u32..).zip(&rooms) {
            if room.room_id != expected_id {
                return Err(QueryError::NonDenseRoomId {
                    expected: expected_id,
                    actual: room.room_id,
                });
            }
            if room.adult_age == 0 {
                return Err(QueryError::InvalidAdultAge(room.room_id));
            }
        }
        Ok(Self {
            availability,
            rooms,
            spatial,
        })
    }

    pub fn search(&self, query: &StayQuery<'_>) -> Result<SearchResult, QueryError> {
        validate_query(query)?;
        let available = self
            .availability
            .available_for_stay(query.check_in_day, query.check_out_day)
            .map_err(QueryError::Availability)?;
        let occupant_ids = query
            .party
            .travelers()
            .map(veyra_party::Traveler::id)
            .collect::<Vec<_>>();
        let check_in_day = query.check_in_day_i32()?;
        let check_out_day = query.check_out_day_i32()?;

        let mut explain = QueryExplain {
            initial_room_count: self.rooms.len(),
            available_candidates: available.len(),
            destination_candidates: 0,
            restriction_candidates: 0,
            occupancy_candidates: 0,
            priced_candidates: 0,
            budget_candidates: 0,
            price_vectors_scanned: 0,
            ranked_candidates: 0,
            returned: 0,
        };
        let mut rank_candidates = Vec::new();

        for room_id in &available {
            let room = &self.rooms[room_id as usize];
            if room.destination_id != query.destination_id {
                continue;
            }
            explain.destination_candidates += 1;

            match room.restrictions.validate_stay(check_in_day, check_out_day) {
                Ok(_) => explain.restriction_candidates += 1,
                Err(error) if is_restriction_rejection(error) => continue,
                Err(error) => return Err(QueryError::Restrictions(error)),
            }

            let occupancy = match validate_room(
                &room.occupancy_rule,
                query.party,
                query.check_in_date,
                &occupant_ids,
                room.adult_age,
            ) {
                Ok(report) => report,
                Err(error) if is_occupancy_rejection(error) => continue,
                Err(error) => return Err(QueryError::Occupancy(error)),
            };
            explain.occupancy_candidates += 1;
            explain.price_vectors_scanned += 1;

            let projected = room
                .prices
                .quote(
                    check_in_day,
                    check_out_day,
                    occupancy.adult_count,
                    occupancy.child_count,
                    room.occupancy_adjustment,
                )
                .map_err(QueryError::Pricing)?;
            explain.priced_candidates += 1;

            if query.budget.is_some_and(|budget| projected.total > budget) {
                continue;
            }
            explain.budget_candidates += 1;
            rank_candidates.push(RankCandidate {
                property_id: room.property_id,
                room_id,
                projected_price: projected.total,
                distance_meters: room.distance_meters,
                quality_milli: room.quality_milli,
                flexibility_milli: room.flexibility_milli,
                family_penalty: room.family_penalty,
            });
        }

        explain.ranked_candidates = rank_candidates.len();
        let ranked =
            top_k(query.ranking, &rank_candidates, query.limit).map_err(QueryError::Ranking)?;
        let hits = ranked.into_iter().map(hit_from_ranked).collect::<Vec<_>>();
        explain.returned = hits.len();
        Ok(SearchResult { hits, explain })
    }

    /// Searches exact multi-room allocations inside each property.
    ///
    /// Candidate rooms are never mixed across properties. Any missing topology semantics, solver
    /// proof-budget exhaustion, or property candidate set larger than the solver's exact bound
    /// fails the whole query closed instead of silently dropping a potentially optimal result.
    pub fn search_multi_room(
        &self,
        query: &MultiRoomStayQuery<'_>,
    ) -> Result<MultiRoomSearchResult, QueryError> {
        validate_multi_room_query(query)?;
        self.validate_multi_room_spatial_semantics(query)?;
        let available = self
            .availability
            .available_for_stay(query.check_in_day, query.check_out_day)
            .map_err(QueryError::Availability)?;
        let check_in_day = query.check_in_day_i32()?;
        let check_out_day = query.check_out_day_i32()?;
        let mut explain = MultiRoomQueryExplain::new(self.rooms.len(), available.len());
        let grouped = self.collect_multi_room_candidates(
            query,
            check_in_day,
            check_out_day,
            &available,
            &mut explain,
        )?;
        let mut hits = solve_multi_room_properties(
            query,
            check_in_day,
            check_out_day,
            grouped,
            self.spatial.as_ref().map(RoomSpatialProjection::topology),
            &mut explain,
        )?;
        hits.sort_by(|left, right| compare_multi_room_hits(left, right, query.profile));
        hits.truncate(query.limit);
        explain.returned = hits.len();
        Ok(MultiRoomSearchResult { hits, explain })
    }

    fn collect_multi_room_candidates(
        &self,
        query: &MultiRoomStayQuery<'_>,
        check_in_day: i32,
        check_out_day: i32,
        available: &DenseRoomSet,
        explain: &mut MultiRoomQueryExplain,
    ) -> Result<BTreeMap<u32, Vec<PricedRoomOffer>>, QueryError> {
        let zero_adjustment = OccupancyAdjustment {
            per_adult_per_night: MoneyMicros::signed(0),
            per_child_per_night: MoneyMicros::signed(0),
        };
        let mut grouped = BTreeMap::<u32, Vec<PricedRoomOffer>>::new();
        for room_id in available {
            let room = &self.rooms[room_id as usize];
            if room.destination_id != query.destination_id {
                continue;
            }
            explain.destination_candidates += 1;
            match room.restrictions.validate_stay(check_in_day, check_out_day) {
                Ok(_) => explain.restriction_candidates += 1,
                Err(error) if is_restriction_rejection(error) => continue,
                Err(error) => return Err(QueryError::Restrictions(error)),
            }
            room.prices
                .quote(check_in_day, check_out_day, 0, 0, zero_adjustment)
                .map_err(QueryError::Pricing)?;
            grouped
                .entry(room.property_id)
                .or_default()
                .push(PricedRoomOffer {
                    room_id: room.room_id,
                    prices: room.prices.clone(),
                    occupancy_adjustment: room.occupancy_adjustment,
                    floor: self
                        .spatial
                        .as_ref()
                        .and_then(|projection| projection.placement(room.room_id))
                        .map_or(0, |placement| placement.floor),
                    building: self
                        .spatial
                        .as_ref()
                        .and_then(|projection| projection.placement(room.room_id))
                        .map_or(0, |placement| placement.building),
                    adult_age: room.adult_age,
                    occupancy_rule: room.occupancy_rule.clone(),
                });
        }
        Ok(grouped)
    }

    fn validate_multi_room_spatial_semantics(
        &self,
        query: &MultiRoomStayQuery<'_>,
    ) -> Result<(), QueryError> {
        if self.spatial.is_some() {
            return Ok(());
        }
        for intent in query.party.rooming_intents() {
            if !matches!(
                intent.relation,
                RoomingRelation::SameRoom | RoomingRelation::SeparateRoom
            ) {
                return Err(QueryError::MissingRoomTopology(intent.relation));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StayQuery<'a> {
    pub destination_id: u32,
    /// Projection service-day key used by availability and price vectors.
    pub check_in_day: u32,
    pub check_out_day: u32,
    /// Exact civil date used only for age-at-check-in semantics.
    pub check_in_date: CivilDate,
    pub party: &'a BookingParty,
    pub budget: Option<MoneyMicros>,
    pub ranking: RankingProfile,
    pub limit: usize,
}

impl StayQuery<'_> {
    fn check_in_day_i32(&self) -> Result<i32, QueryError> {
        i32::try_from(self.check_in_day).map_err(|_| QueryError::ServiceDayOutOfRange)
    }

    fn check_out_day_i32(&self) -> Result<i32, QueryError> {
        i32::try_from(self.check_out_day).map_err(|_| QueryError::ServiceDayOutOfRange)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MultiRoomStayQuery<'a> {
    pub destination_id: u32,
    pub check_in_day: u32,
    pub check_out_day: u32,
    pub check_in_date: CivilDate,
    pub party: &'a BookingParty,
    pub budget: Option<MoneyMicros>,
    pub profile: SolutionProfile,
    pub solver: SolverConfig,
    pub limit: usize,
}

impl MultiRoomStayQuery<'_> {
    fn check_in_day_i32(&self) -> Result<i32, QueryError> {
        i32::try_from(self.check_in_day).map_err(|_| QueryError::ServiceDayOutOfRange)
    }

    fn check_out_day_i32(&self) -> Result<i32, QueryError> {
        i32::try_from(self.check_out_day).map_err(|_| QueryError::ServiceDayOutOfRange)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiRoomSearchHit {
    pub property_id: u32,
    pub rooms: Vec<RoomAllocation>,
    pub projected_price: MoneyMicros,
    pub soft_penalty: u32,
    pub profile: SolutionProfile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultiRoomQueryExplain {
    pub initial_room_count: usize,
    pub available_candidates: usize,
    pub destination_candidates: usize,
    pub restriction_candidates: usize,
    pub properties_considered: usize,
    pub properties_solved: usize,
    pub infeasible_properties: usize,
    pub budget_rejected_properties: usize,
    pub solver_states_explored: usize,
    pub valid_solutions_seen: usize,
    pub returned: usize,
}

impl MultiRoomQueryExplain {
    const fn new(initial_room_count: usize, available_candidates: usize) -> Self {
        Self {
            initial_room_count,
            available_candidates,
            destination_candidates: 0,
            restriction_candidates: 0,
            properties_considered: 0,
            properties_solved: 0,
            infeasible_properties: 0,
            budget_rejected_properties: 0,
            solver_states_explored: 0,
            valid_solutions_seen: 0,
            returned: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiRoomSearchResult {
    pub hits: Vec<MultiRoomSearchHit>,
    pub explain: MultiRoomQueryExplain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchHit {
    pub property_id: u32,
    pub room_id: u32,
    pub projected_price: MoneyMicros,
    pub ranking_score: i128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryExplain {
    pub initial_room_count: usize,
    pub available_candidates: usize,
    pub destination_candidates: usize,
    pub restriction_candidates: usize,
    pub occupancy_candidates: usize,
    pub priced_candidates: usize,
    pub budget_candidates: usize,
    pub price_vectors_scanned: usize,
    pub ranked_candidates: usize,
    pub returned: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub explain: QueryExplain,
}

fn validate_query(query: &StayQuery<'_>) -> Result<(), QueryError> {
    if query.limit == 0 || query.limit > MAX_TOP_K {
        return Err(QueryError::InvalidLimit(query.limit));
    }
    if query.check_out_day <= query.check_in_day {
        return Err(QueryError::InvalidStayRange);
    }
    if query.budget.is_some_and(|budget| budget.get() < 0) {
        return Err(QueryError::NegativeBudget);
    }
    let party_size = query.party.travelers().len();
    if party_size == 0 || party_size > MAX_QUERY_PARTY {
        return Err(QueryError::PartyTooLarge(party_size));
    }
    Ok(())
}

fn solve_multi_room_properties(
    query: &MultiRoomStayQuery<'_>,
    check_in_day: i32,
    check_out_day: i32,
    grouped: BTreeMap<u32, Vec<PricedRoomOffer>>,
    topology: Option<&RoomRelationIndex>,
    explain: &mut MultiRoomQueryExplain,
) -> Result<Vec<MultiRoomSearchHit>, QueryError> {
    let mut hits = Vec::new();
    for (property_id, offers) in grouped {
        explain.properties_considered += 1;
        if offers.len() > HARD_MAX_ROOMS {
            return Err(QueryError::SolverRoomLimit {
                property_id,
                rooms: offers.len(),
            });
        }
        let solved = match topology {
            Some(index) => solve_priced_with_topology(
                query.party,
                query.check_in_date,
                check_in_day,
                check_out_day,
                &offers,
                index,
                query.solver,
            ),
            None => solve_priced(
                query.party,
                query.check_in_date,
                check_in_day,
                check_out_day,
                &offers,
                query.solver,
            ),
        }
        .map_err(QueryError::Solver)?;
        explain.solver_states_explored = explain
            .solver_states_explored
            .checked_add(solved.explored_states)
            .ok_or(QueryError::ExplainOverflow)?;
        explain.valid_solutions_seen = explain
            .valid_solutions_seen
            .checked_add(solved.valid_solution_count)
            .ok_or(QueryError::ExplainOverflow)?;
        if solved.valid_solution_count == 0 {
            explain.infeasible_properties += 1;
            continue;
        }
        let tagged = solved
            .solutions
            .iter()
            .find(|candidate| candidate.profile == query.profile)
            .ok_or(QueryError::SolverProfileMissing(query.profile))?;
        explain.properties_solved += 1;
        if query
            .budget
            .is_some_and(|budget| tagged.solution.total_price > budget)
        {
            explain.budget_rejected_properties += 1;
            continue;
        }
        hits.push(MultiRoomSearchHit {
            property_id,
            rooms: tagged.solution.rooms.clone(),
            projected_price: tagged.solution.total_price,
            soft_penalty: tagged.solution.soft_penalty,
            profile: tagged.profile,
        });
    }
    Ok(hits)
}

fn validate_multi_room_query(query: &MultiRoomStayQuery<'_>) -> Result<(), QueryError> {
    if query.limit == 0 || query.limit > MAX_TOP_K {
        return Err(QueryError::InvalidLimit(query.limit));
    }
    if query.check_out_day <= query.check_in_day {
        return Err(QueryError::InvalidStayRange);
    }
    if query.budget.is_some_and(|budget| budget.get() < 0) {
        return Err(QueryError::NegativeBudget);
    }
    let party_size = query.party.travelers().len();
    if party_size == 0 || party_size > HARD_MAX_TRAVELERS {
        return Err(QueryError::SolverPartyTooLarge(party_size));
    }
    Ok(())
}

fn compare_multi_room_hits(
    left: &MultiRoomSearchHit,
    right: &MultiRoomSearchHit,
    profile: SolutionProfile,
) -> Ordering {
    let profile_order = match profile {
        SolutionProfile::Cheapest => left
            .projected_price
            .cmp(&right.projected_price)
            .then_with(|| left.rooms.len().cmp(&right.rooms.len()))
            .then_with(|| left.soft_penalty.cmp(&right.soft_penalty)),
        SolutionProfile::FewestRooms => left
            .rooms
            .len()
            .cmp(&right.rooms.len())
            .then_with(|| left.projected_price.cmp(&right.projected_price))
            .then_with(|| left.soft_penalty.cmp(&right.soft_penalty)),
        SolutionProfile::BestFamilyLayout => left
            .soft_penalty
            .cmp(&right.soft_penalty)
            .then_with(|| left.projected_price.cmp(&right.projected_price))
            .then_with(|| left.rooms.len().cmp(&right.rooms.len())),
    };
    profile_order
        .then_with(|| left.property_id.cmp(&right.property_id))
        .then_with(|| left.rooms.cmp(&right.rooms))
}

fn is_restriction_rejection(error: RestrictionError) -> bool {
    matches!(
        error,
        RestrictionError::BelowMinStay { .. }
            | RestrictionError::AboveMaxStay { .. }
            | RestrictionError::ClosedToArrival(_)
            | RestrictionError::ClosedToDeparture(_)
    )
}

fn is_occupancy_rejection(error: OccupancyError) -> bool {
    matches!(
        error,
        OccupancyError::PolicyRejected
            | OccupancyError::MustStayTogether { .. }
            | OccupancyError::MustStaySeparate { .. }
    )
}

fn hit_from_ranked(ranked: RankedCandidate) -> SearchHit {
    SearchHit {
        property_id: ranked.candidate.property_id,
        room_id: ranked.candidate.room_id,
        projected_price: ranked.candidate.projected_price,
        ranking_score: ranked.deterministic_score,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryError {
    CatalogInvariant,
    RoomDocumentCountMismatch { expected: usize, actual: usize },
    NonDenseRoomId { expected: u32, actual: u32 },
    SpatialNonDenseRoomId { expected: u32, actual: u32 },
    SpatialRoomCountMismatch { expected: usize, actual: usize },
    Topology(TopologyError),
    InvalidAdultAge(u32),
    MissingRoomProjection(u32),
    InvalidLimit(usize),
    InvalidStayRange,
    ServiceDayOutOfRange,
    NegativeBudget,
    PartyTooLarge(usize),
    Availability(AvailabilityError),
    Restrictions(RestrictionError),
    Occupancy(OccupancyError),
    Pricing(PricingError),
    Ranking(RankingError),
    SolverPartyTooLarge(usize),
    SolverRoomLimit { property_id: u32, rooms: usize },
    MissingRoomTopology(RoomingRelation),
    Solver(SolverError),
    SolverProfileMissing(SolutionProfile),
    ExplainOverflow,
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for QueryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use veyra_party::{AgeEvidence, Traveler, TravelerId};
    use veyra_pricing::MoneyMicros;
    use veyra_ranking::RankingKind;
    use veyra_restrictions::{
        RESTRICTION_SCHEMA_V1, RestrictionRule, compile as compile_restrictions,
    };
    use veyra_rule_compiler::{RULE_SCHEMA_V1, compile as compile_rule};
    use veyra_rules::Rule;

    fn date(y: i32, m: u8, d: u8) -> CivilDate {
        CivilDate::new(y, m, d).unwrap_or_else(|_| unreachable!())
    }

    fn money(value: i64) -> MoneyMicros {
        MoneyMicros::try_nonnegative(value).unwrap_or_else(|_| unreachable!())
    }

    fn party() -> BookingParty {
        let mut builder = BookingParty::builder();
        for traveler in [
            Traveler::new(TravelerId::new(1), AgeEvidence::AgeAtCheckIn(30), false),
            Traveler::new(TravelerId::new(2), AgeEvidence::AgeAtCheckIn(28), false),
        ] {
            builder
                .add_traveler(traveler)
                .unwrap_or_else(|_| unreachable!());
        }
        builder.build().unwrap_or_else(|_| unreachable!())
    }

    fn room(room_id: u32, property_id: u32, destination_id: u32, nightly: i64) -> RoomDocument {
        RoomDocument {
            room_id,
            property_id,
            destination_id,
            adult_age: 18,
            occupancy_rule: compile_rule(
                RULE_SCHEMA_V1,
                &Rule::And(vec![
                    Rule::Capacity { min: 1, max: 4 },
                    Rule::RequireAdult {
                        adult_age: 18,
                        min_adults: 1,
                    },
                ]),
            )
            .unwrap_or_else(|_| unreachable!()),
            restrictions: compile_restrictions(
                RESTRICTION_SCHEMA_V1,
                &[RestrictionRule::MinStay(1), RestrictionRule::MaxStay(5)],
            )
            .unwrap_or_else(|_| unreachable!()),
            prices: PriceVector::try_new(10, vec![money(nightly); 5], 1)
                .unwrap_or_else(|_| unreachable!()),
            occupancy_adjustment: OccupancyAdjustment {
                per_adult_per_night: money(0),
                per_child_per_night: money(0),
            },
            distance_meters: 500,
            quality_milli: 700,
            flexibility_milli: 800,
            family_penalty: 0,
        }
    }

    fn engine() -> SearchEngine {
        let mut availability = AvailabilityIndex::new(10, 5, 3).unwrap_or_else(|_| unreachable!());
        for room_id in 0..3 {
            for day in 10..15 {
                availability
                    .set_available(day, room_id, true)
                    .unwrap_or_else(|_| unreachable!());
            }
        }
        SearchEngine::try_new(
            availability,
            vec![
                room(0, 100, 1, 100),
                room(1, 101, 1, 80),
                room(2, 200, 2, 50),
            ],
        )
        .unwrap_or_else(|_| unreachable!())
    }

    fn query(party: &BookingParty, budget: Option<MoneyMicros>) -> StayQuery<'_> {
        StayQuery {
            destination_id: 1,
            check_in_day: 10,
            check_out_day: 12,
            check_in_date: date(2026, 9, 21),
            party,
            budget,
            ranking: RankingProfile::v1(RankingKind::Cheapest),
            limit: 10,
        }
    }

    #[test]
    fn pipeline_filters_hard_constraints_before_ranking() {
        let party = party();
        let result = engine()
            .search(&query(&party, Some(money(170))))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].room_id, 1);
        assert_eq!(result.hits[0].projected_price, money(160));
        assert_eq!(
            result.explain,
            QueryExplain {
                initial_room_count: 3,
                available_candidates: 3,
                destination_candidates: 2,
                restriction_candidates: 2,
                occupancy_candidates: 2,
                priced_candidates: 2,
                budget_candidates: 1,
                price_vectors_scanned: 2,
                ranked_candidates: 1,
                returned: 1,
            }
        );
    }

    #[test]
    fn deterministic_ranking_orders_equal_valid_candidates() {
        let party = party();
        let result = engine()
            .search(&query(&party, None))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            result
                .hits
                .iter()
                .map(|hit| hit.room_id)
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
    }

    #[test]
    fn catalog_and_query_bounds_fail_closed() {
        let availability = AvailabilityIndex::new(0, 1, 1).unwrap_or_else(|_| unreachable!());
        assert!(matches!(
            SearchEngine::try_new(availability, Vec::new()),
            Err(QueryError::RoomDocumentCountMismatch { .. })
        ));
        let party = party();
        let mut invalid = query(&party, None);
        invalid.limit = 0;
        assert_eq!(engine().search(&invalid), Err(QueryError::InvalidLimit(0)));
        let mut invalid = query(&party, None);
        invalid.check_out_day = invalid.check_in_day;
        assert_eq!(engine().search(&invalid), Err(QueryError::InvalidStayRange));
        let invalid = query(&party, Some(MoneyMicros::signed(-1)));
        assert_eq!(engine().search(&invalid), Err(QueryError::NegativeBudget));
    }

    #[test]
    fn missing_price_projection_fails_whole_query_closed() {
        let mut availability = AvailabilityIndex::new(10, 2, 1).unwrap_or_else(|_| unreachable!());
        for day in 10..12 {
            availability
                .set_available(day, 0, true)
                .unwrap_or_else(|_| unreachable!());
        }
        let mut document = room(0, 1, 1, 1);
        document.prices =
            PriceVector::try_new(20, vec![money(1); 2], 1).unwrap_or_else(|_| unreachable!());
        let engine =
            SearchEngine::try_new(availability, vec![document]).unwrap_or_else(|_| unreachable!());
        let party = party();
        assert_eq!(
            engine.search(&query(&party, None)),
            Err(QueryError::Pricing(PricingError::OutsidePriceHorizon))
        );
    }

    #[test]
    fn error_display_is_stable_debug_form() {
        assert_eq!(QueryError::NegativeBudget.to_string(), "NegativeBudget");
    }
}
