from pathlib import Path

path = Path("crates/veyra-query/src/lib.rs")
text = path.read_text()

old_import = '''use veyra_solver::{
    HARD_MAX_ROOMS, HARD_MAX_TRAVELERS, PricedRoomOffer, SolverError, solve_priced,
};
pub use veyra_solver::{RoomAllocation, SolutionProfile, SolverConfig};
'''
new_import = '''use veyra_solver::{
    HARD_MAX_ROOMS, HARD_MAX_TRAVELERS, PricedRoomOffer, RoomRelationIndex, SolverError,
    TopologyError, solve_priced, solve_priced_with_topology,
};
pub use veyra_solver::{
    RoomAllocation, RoomTopologyEdge, RoomTopologyRelation, SolutionProfile, SolverConfig,
};
'''
if text.count(old_import) != 1:
    raise SystemExit("solver import anchor changed")
text = text.replace(old_import, new_import, 1)

room_doc_marker = '''#[derive(Debug)]
pub struct SearchEngine {
    availability: AvailabilityIndex,
    rooms: Vec<RoomDocument>,
}
'''
spatial_types = '''#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
        let topology = RoomRelationIndex::try_new(&room_ids, &edges).map_err(QueryError::Topology)?;
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
'''
if text.count(room_doc_marker) != 1:
    raise SystemExit("SearchEngine declaration anchor changed")
text = text.replace(room_doc_marker, spatial_types, 1)

old_ctor = '''    pub fn try_new(
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
        })
    }
'''
new_ctor = '''    pub fn try_new(
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
'''
if text.count(old_ctor) != 1:
    raise SystemExit("SearchEngine constructor anchor changed")
text = text.replace(old_ctor, new_ctor, 1)

old_search_start = '''    ) -> Result<MultiRoomSearchResult, QueryError> {
        validate_multi_room_query(query)?;
        let available = self
'''
new_search_start = '''    ) -> Result<MultiRoomSearchResult, QueryError> {
        validate_multi_room_query(query)?;
        self.validate_multi_room_spatial_semantics(query)?;
        let available = self
'''
if text.count(old_search_start) != 1:
    raise SystemExit("multi-room search start anchor changed")
text = text.replace(old_search_start, new_search_start, 1)

old_solve_call = '''        let mut hits =
            solve_multi_room_properties(query, check_in_day, check_out_day, grouped, &mut explain)?;
'''
new_solve_call = '''        let mut hits = solve_multi_room_properties(
            query,
            check_in_day,
            check_out_day,
            grouped,
            self.spatial.as_ref().map(RoomSpatialProjection::topology),
            &mut explain,
        )?;
'''
if text.count(old_solve_call) != 1:
    raise SystemExit("multi-room solve call anchor changed")
text = text.replace(old_solve_call, new_solve_call, 1)

old_offer_fields = '''                .push(PricedRoomOffer {
                    room_id: room.room_id,
                    prices: room.prices.clone(),
                    occupancy_adjustment: room.occupancy_adjustment,
                    floor: 0,
                    building: 0,
                    adult_age: room.adult_age,
                    occupancy_rule: room.occupancy_rule.clone(),
                });
'''
new_offer_fields = '''                .push(PricedRoomOffer {
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
'''
if text.count(old_offer_fields) != 1:
    raise SystemExit("PricedRoomOffer construction anchor changed")
text = text.replace(old_offer_fields, new_offer_fields, 1)

collect_end = '''        Ok(grouped)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StayQuery<'a> {
'''
collect_replacement = '''        Ok(grouped)
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
'''
if text.count(collect_end) != 1:
    raise SystemExit("SearchEngine impl end anchor changed")
text = text.replace(collect_end, collect_replacement, 1)

old_solve_sig = '''fn solve_multi_room_properties(
    query: &MultiRoomStayQuery<'_>,
    check_in_day: i32,
    check_out_day: i32,
    grouped: BTreeMap<u32, Vec<PricedRoomOffer>>,
    explain: &mut MultiRoomQueryExplain,
) -> Result<Vec<MultiRoomSearchHit>, QueryError> {
'''
new_solve_sig = '''fn solve_multi_room_properties(
    query: &MultiRoomStayQuery<'_>,
    check_in_day: i32,
    check_out_day: i32,
    grouped: BTreeMap<u32, Vec<PricedRoomOffer>>,
    topology: Option<&RoomRelationIndex>,
    explain: &mut MultiRoomQueryExplain,
) -> Result<Vec<MultiRoomSearchHit>, QueryError> {
'''
if text.count(old_solve_sig) != 1:
    raise SystemExit("solve_multi_room_properties signature changed")
text = text.replace(old_solve_sig, new_solve_sig, 1)

old_solver = '''        let solved = solve_priced(
            query.party,
            query.check_in_date,
            check_in_day,
            check_out_day,
            &offers,
            query.solver,
        )
        .map_err(QueryError::Solver)?;
'''
new_solver = '''        let solved = match topology {
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
'''
if text.count(old_solver) != 1:
    raise SystemExit("multi-room solver invocation changed")
text = text.replace(old_solver, new_solver, 1)

old_validation = '''    for intent in query.party.rooming_intents() {
        if !matches!(
            intent.relation,
            RoomingRelation::SameRoom | RoomingRelation::SeparateRoom
        ) {
            return Err(QueryError::MissingRoomTopology(intent.relation));
        }
    }
    Ok(())
}
'''
if text.count(old_validation) != 1:
    raise SystemExit("multi-room spatial validation anchor changed")
text = text.replace(old_validation, '''    Ok(())
}
''', 1)

error_anchor = '''    NonDenseRoomId { expected: u32, actual: u32 },
    InvalidAdultAge(u32),
'''
error_replacement = '''    NonDenseRoomId { expected: u32, actual: u32 },
    SpatialNonDenseRoomId { expected: u32, actual: u32 },
    SpatialRoomCountMismatch { expected: usize, actual: usize },
    Topology(TopologyError),
    InvalidAdultAge(u32),
'''
if text.count(error_anchor) != 1:
    raise SystemExit("QueryError anchor changed")
text = text.replace(error_anchor, error_replacement, 1)

path.write_text(text)
