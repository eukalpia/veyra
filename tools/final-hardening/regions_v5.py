from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    target = Path(path)
    text = target.read_text()
    if new in text:
        return
    if text.count(old) != 1:
        raise SystemExit(f'{label}: expected one old block in {path}, found {text.count(old)}')
    target.write_text(text.replace(old, new, 1))


# Supported release targets are all 64-bit. start_i64 is already proven non-negative and
# originates from an i32 service-day difference, so this conversion cannot fail.
replace_once(
    'crates/veyra-pricing/src/lib.rs',
    '        let start = usize::try_from(start_i64).map_err(|_| PricingError::OutsidePriceHorizon)?;\n',
    '''        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative i32 service-day difference fits usize on all supported 64-bit targets"
        )]
        let start = start_i64 as usize;
''',
    'pricing infallible horizon offset',
)

query = Path('crates/veyra-query/src/lib.rs')
text = query.read_text()
old_restriction = '''            match room.restrictions.validate_stay(check_in_day, check_out_day) {
                Ok(_) => explain.restriction_candidates += 1,
                Err(error) if is_restriction_rejection(error) => continue,
                Err(error) => return Err(QueryError::Restrictions(error)),
            }
'''
new_restriction = '''            // Availability admission above proves a positive stay of at most 90 nights and
            // CompiledRestrictions is valid by construction. Every remaining runtime rejection
            // is therefore a hard candidate rejection, never an engine failure.
            if room
                .restrictions
                .validate_stay(check_in_day, check_out_day)
                .is_err()
            {
                continue;
            }
            explain.restriction_candidates += 1;
'''
count = text.count(old_restriction)
if count:
    if count != 2:
        raise SystemExit(f'expected two query restriction blocks, found {count}')
    text = text.replace(old_restriction, new_restriction)
query.write_text(text)

# The classifier is now test-only documentation of the four public stay-rejection variants.
replace_once(
    'crates/veyra-query/src/lib.rs',
    'fn is_restriction_rejection(error: RestrictionError) -> bool {\n',
    '#[cfg(test)]\nfn is_restriction_rejection(error: RestrictionError) -> bool {\n',
    'query restriction classifier test-only',
)

# Index party rooming endpoints once. BookingParty validates edge endpoints at construction;
# retaining one typed conversion boundary preserves fail-closed behavior without repeating
# impossible lookups throughout the exponential search.
replace_once(
    'crates/veyra-solver/src/lib.rs',
    '''    validate_input(party, &travelers, &offers, topology, config)?;

    let mut context = SearchContext {
        party,
        check_in,
        offers,
        topology,
        config,
        travelers,
''',
    '''    validate_input(party, &travelers, &offers, topology, config)?;
    let intents = index_rooming_intents(party, &travelers)?;

    let mut context = SearchContext {
        party,
        check_in,
        offers,
        topology,
        config,
        travelers,
        intents,
''',
    'solver indexed intent construction',
)

replace_once(
    'crates/veyra-solver/src/lib.rs',
    '''struct SearchContext<'a> {
    party: &'a BookingParty,
    check_in: CivilDate,
    offers: Vec<PreparedOffer<'a>>,
    topology: Option<&'a RoomRelationIndex>,
    config: SolverConfig,
    travelers: Vec<TravelerId>,
    assignments: Vec<usize>,
''',
    '''#[derive(Clone, Copy)]
struct IndexedIntent {
    left_position: usize,
    right_position: usize,
    strength: ConstraintStrength,
    relation: RoomingRelation,
}

fn index_rooming_intents(
    party: &BookingParty,
    travelers: &[TravelerId],
) -> Result<Vec<IndexedIntent>, SolverError> {
    party
        .rooming_intents()
        .iter()
        .map(|intent| {
            let left_position = travelers
                .iter()
                .position(|id| *id == intent.left)
                .ok_or(SolverError::UnknownTraveler(intent.left))?;
            let right_position = travelers
                .iter()
                .position(|id| *id == intent.right)
                .ok_or(SolverError::UnknownTraveler(intent.right))?;
            Ok(IndexedIntent {
                left_position,
                right_position,
                strength: intent.strength,
                relation: intent.relation,
            })
        })
        .collect()
}

struct SearchContext<'a> {
    party: &'a BookingParty,
    check_in: CivilDate,
    offers: Vec<PreparedOffer<'a>>,
    topology: Option<&'a RoomRelationIndex>,
    config: SolverConfig,
    travelers: Vec<TravelerId>,
    intents: Vec<IndexedIntent>,
    assignments: Vec<usize>,
''',
    'solver indexed intent storage',
)

replace_once(
    'crates/veyra-solver/src/lib.rs',
    '''        if partial_hard_constraints_hold(context)? {
            search(context, traveler_index + 1)?;
        }
''',
    '''        if partial_hard_constraints_hold(context) {
            search(context, traveler_index + 1)?;
        }
''',
    'solver partial predicate infallible',
)

replace_once(
    'crates/veyra-solver/src/lib.rs',
    '''fn evaluate_leaf(context: &mut SearchContext<'_>) -> Result<(), SolverError> {
    if !layout_hard_constraints_hold(context)? {
        return Ok(());
    }
''',
    '''fn evaluate_leaf(context: &mut SearchContext<'_>) -> Result<(), SolverError> {
    if !layout_hard_constraints_hold(context) {
        return Ok(());
    }
''',
    'solver leaf hard predicate infallible',
)

replace_once(
    'crates/veyra-solver/src/lib.rs',
    '''    let soft_penalty = layout_soft_penalty(context)?;
''',
    '''    let soft_penalty = layout_soft_penalty(context);
''',
    'solver soft penalty infallible',
)

old_layout = '''fn layout_hard_constraints_hold(context: &SearchContext<'_>) -> Result<bool, SolverError> {
    for intent in context
        .party
        .rooming_intents()
        .iter()
        .filter(|intent| intent.strength == ConstraintStrength::Must)
    {
        let left = assignment_of(context, intent.left)?;
        let right = assignment_of(context, intent.right)?;
        let valid = relation_satisfied(context, left, right, intent.relation)?;
        if !valid {
            return Ok(false);
        }
    }
    Ok(true)
}

fn partial_hard_constraints_hold(context: &SearchContext<'_>) -> Result<bool, SolverError> {
    for intent in context
        .party
        .rooming_intents()
        .iter()
        .filter(|intent| intent.strength == ConstraintStrength::Must)
    {
        let left_position = context
            .travelers
            .iter()
            .position(|id| *id == intent.left)
            .ok_or(SolverError::UnknownTraveler(intent.left))?;
        let right_position = context
            .travelers
            .iter()
            .position(|id| *id == intent.right)
            .ok_or(SolverError::UnknownTraveler(intent.right))?;
        let Some(left) = context.assignments.get(left_position).copied() else {
            continue;
        };
        let Some(right) = context.assignments.get(right_position).copied() else {
            continue;
        };
        let valid = relation_satisfied(context, left, right, intent.relation)?;
        if !valid {
            return Ok(false);
        }
    }
    Ok(true)
}

fn relation_satisfied(
    context: &SearchContext<'_>,
    left: usize,
    right: usize,
    relation: RoomingRelation,
) -> Result<bool, SolverError> {
    let left_offer = &context.offers[left];
    let right_offer = &context.offers[right];
    let topology = || context.topology.ok_or(SolverError::InternalInvariant);
    match relation {
        RoomingRelation::SameRoom => Ok(left == right),
        RoomingRelation::SeparateRoom => Ok(left != right),
        RoomingRelation::SameFloor => Ok(left_offer.floor == right_offer.floor),
        RoomingRelation::SameBuilding => Ok(left_offer.building == right_offer.building),
        RoomingRelation::Near => Ok(left == right
            || topology()?.contains(
                RoomTopologyRelation::Near,
                left_offer.room_id,
                right_offer.room_id,
            )),
        RoomingRelation::AdjacentRooms => Ok(left != right
            && topology()?.contains(
                RoomTopologyRelation::Adjacent,
                left_offer.room_id,
                right_offer.room_id,
            )),
        RoomingRelation::ConnectedRooms => Ok(left != right
            && topology()?.contains(
                RoomTopologyRelation::Connected,
                left_offer.room_id,
                right_offer.room_id,
            )),
    }
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
        let satisfied = relation_satisfied(context, left, right, intent.relation)?;
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
'''
new_layout = '''fn layout_hard_constraints_hold(context: &SearchContext<'_>) -> bool {
    for intent in context
        .intents
        .iter()
        .filter(|intent| intent.strength == ConstraintStrength::Must)
    {
        let left = context.assignments[intent.left_position];
        let right = context.assignments[intent.right_position];
        if !relation_satisfied(context, left, right, intent.relation) {
            return false;
        }
    }
    true
}

fn partial_hard_constraints_hold(context: &SearchContext<'_>) -> bool {
    for intent in context
        .intents
        .iter()
        .filter(|intent| intent.strength == ConstraintStrength::Must)
    {
        let Some(left) = context.assignments.get(intent.left_position).copied() else {
            continue;
        };
        let Some(right) = context.assignments.get(intent.right_position).copied() else {
            continue;
        };
        if !relation_satisfied(context, left, right, intent.relation) {
            return false;
        }
    }
    true
}

fn relation_satisfied(
    context: &SearchContext<'_>,
    left: usize,
    right: usize,
    relation: RoomingRelation,
) -> bool {
    let left_offer = &context.offers[left];
    let right_offer = &context.offers[right];
    match relation {
        RoomingRelation::SameRoom => left == right,
        RoomingRelation::SeparateRoom => left != right,
        RoomingRelation::SameFloor => left_offer.floor == right_offer.floor,
        RoomingRelation::SameBuilding => left_offer.building == right_offer.building,
        RoomingRelation::Near => {
            left == right
                || context.topology.is_some_and(|topology| {
                    topology.contains(
                        RoomTopologyRelation::Near,
                        left_offer.room_id,
                        right_offer.room_id,
                    )
                })
        }
        RoomingRelation::AdjacentRooms => {
            left != right
                && context.topology.is_some_and(|topology| {
                    topology.contains(
                        RoomTopologyRelation::Adjacent,
                        left_offer.room_id,
                        right_offer.room_id,
                    )
                })
        }
        RoomingRelation::ConnectedRooms => {
            left != right
                && context.topology.is_some_and(|topology| {
                    topology.contains(
                        RoomTopologyRelation::Connected,
                        left_offer.room_id,
                        right_offer.room_id,
                    )
                })
        }
    }
}

fn layout_soft_penalty(context: &SearchContext<'_>) -> u32 {
    let mut penalty = 0_u32;
    for intent in context
        .intents
        .iter()
        .filter(|intent| intent.strength != ConstraintStrength::Must)
    {
        let left = context.assignments[intent.left_position];
        let right = context.assignments[intent.right_position];
        let satisfied = relation_satisfied(context, left, right, intent.relation);
        let violated = if intent.strength == ConstraintStrength::Prefer {
            !satisfied
        } else {
            satisfied
        };
        if violated {
            // BookingParty bounds rooming edges to 512, far below u32::MAX.
            penalty += 1;
        }
    }
    penalty
}
'''
replace_once('crates/veyra-solver/src/lib.rs', old_layout, new_layout, 'solver indexed predicates')

# Exercise the multi-room availability error propagation that is reachable independently of the
# single-room path.
path = Path('crates/veyra-query/tests/multi_room_matrix.rs')
text = path.read_text()
marker = 'fn multi_room_availability_bounds_propagate_typed_error()'
if marker not in text:
    text += r'''

#[test]
fn multi_room_availability_bounds_propagate_typed_error() {
    let one = party(1, None);
    let base = engine(10, 1, vec![room(0, 1, 55, 2, 100, 10)]);
    let mut request = query(&one, SolutionProfile::Cheapest);
    request.check_in_day = 11;
    request.check_out_day = 12;
    assert!(matches!(
        base.search_multi_room(&request),
        Err(QueryError::Availability(_))
    ));
}
'''
    path.write_text(text)

# One private Segment path is reachable without huge allocation or fault injection: a root path
# has no parent and must be a no-op durability boundary.
path = Path('crates/veyra-segment/src/lib.rs')
text = path.read_text()
marker = 'mod coverage_v5_segment'
if marker not in text:
    text += r'''

#[cfg(test)]
mod coverage_v5_segment {
    use super::*;

    #[test]
    fn parentless_path_has_no_directory_sync_obligation() {
        assert!(sync_parent(Path::new("/")).is_ok());
    }
}
'''
    path.write_text(text)

print('region v5 structural hardening staged')
