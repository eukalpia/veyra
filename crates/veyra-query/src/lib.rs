#![forbid(unsafe_code)]

//! Typed deterministic Veyra search pipeline.
//!
//! V1 deliberately implements a single-room vertical path. Complex multi-room parties are handled
//! by `veyra-solver`. Hard validity always precedes ranking and missing projection state fails the
//! whole query closed instead of silently hiding a potentially valid property.

use core::fmt;
use veyra_availability::{AvailabilityError, AvailabilityIndex};
use veyra_occupancy::{OccupancyError, validate_room};
use veyra_party::{BookingParty, CivilDate};
use veyra_pricing::{MoneyMicros, OccupancyAdjustment, PriceVector, PricingError};
use veyra_ranking::{
    MAX_TOP_K, RankCandidate, RankedCandidate, RankingError, RankingProfile, top_k,
};
use veyra_restrictions::{CompiledRestrictions, RestrictionError};
use veyra_rule_compiler::CompiledRule;

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

#[derive(Debug)]
pub struct SearchEngine {
    availability: AvailabilityIndex,
    rooms: Vec<RoomDocument>,
}

impl SearchEngine {
    pub fn try_new(
        availability: AvailabilityIndex,
        rooms: Vec<RoomDocument>,
    ) -> Result<Self, QueryError> {
        let expected = availability.room_count() as usize;
        if rooms.len() != expected {
            return Err(QueryError::RoomDocumentCountMismatch {
                expected,
                actual: rooms.len(),
            });
        }
        for (index, room) in rooms.iter().enumerate() {
            let expected_id = index as u32;
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

        for room_id in available.iter() {
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
