from pathlib import Path

path = Path("crates/veyra-solver/src/lib.rs")
text = path.read_text()

imports = "use veyra_rule_compiler::CompiledRule;\n"
replacement = (
    imports
    + "\nmod topology;\n"
    + "pub use topology::{RoomRelationIndex, RoomTopologyEdge, RoomTopologyRelation, TopologyError};\n"
)
if text.count(imports) != 1:
    raise SystemExit("solver import anchor changed")
text = text.replace(imports, replacement, 1)

old_call = "solve_prepared(party, check_in, prepared, config)"
if text.count(old_call) != 2:
    raise SystemExit("solve_prepared call count changed")
text = text.replace(old_call, "solve_prepared(party, check_in, prepared, None, config)")

marker = "#[derive(Clone, Copy)]\nstruct PricingWindow {\n"
topology_api = '''/// Pricing-safe exact solve with a complete explicit room-topology projection.
pub fn solve_priced_with_topology(
    party: &BookingParty,
    check_in: CivilDate,
    check_in_day: i32,
    check_out_day: i32,
    offers: &[PricedRoomOffer],
    topology: &RoomRelationIndex,
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
    solve_prepared(party, check_in, prepared, Some(topology), config)
}

'''
if text.count(marker) != 1:
    raise SystemExit("pricing window anchor changed")
text = text.replace(marker, topology_api + marker, 1)

old_signature = '''fn solve_prepared(
    party: &BookingParty,
    check_in: CivilDate,
    offers: Vec<PreparedOffer<'_>>,
    config: SolverConfig,
) -> Result<SolverResult, SolverError> {'''
new_signature = '''fn solve_prepared(
    party: &BookingParty,
    check_in: CivilDate,
    offers: Vec<PreparedOffer<'_>>,
    topology: Option<&RoomRelationIndex>,
    config: SolverConfig,
) -> Result<SolverResult, SolverError> {'''
if text.count(old_signature) != 1:
    raise SystemExit("solve_prepared signature changed")
text = text.replace(old_signature, new_signature, 1)

old_validate_call = "    validate_input(party, &travelers, &offers, config)?;\n"
if text.count(old_validate_call) != 1:
    raise SystemExit("validate_input call changed")
text = text.replace(
    old_validate_call,
    "    validate_input(party, &travelers, &offers, topology, config)?;\n",
    1,
)

old_context_init = "        offers,\n        config,\n        travelers,\n"
if text.count(old_context_init) != 1:
    raise SystemExit("search context initializer changed")
text = text.replace(
    old_context_init,
    "        offers,\n        topology,\n        config,\n        travelers,\n",
    1,
)

old_validate_sig = '''fn validate_input(
    party: &BookingParty,
    travelers: &[TravelerId],
    offers: &[PreparedOffer<'_>],
    config: SolverConfig,
) -> Result<(), SolverError> {'''
new_validate_sig = '''fn validate_input(
    party: &BookingParty,
    travelers: &[TravelerId],
    offers: &[PreparedOffer<'_>],
    topology: Option<&RoomRelationIndex>,
    config: SolverConfig,
) -> Result<(), SolverError> {'''
if text.count(old_validate_sig) != 1:
    raise SystemExit("validate_input signature changed")
text = text.replace(old_validate_sig, new_validate_sig, 1)

old_intents = '''    for intent in party
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
}'''
new_intents = '''    if let Some(index) = topology {
        for offer in offers {
            if !index.contains_room(offer.room_id) {
                return Err(SolverError::TopologyMissingRoom(offer.room_id));
            }
        }
    }
    if topology.is_none() {
        for intent in party.rooming_intents() {
            if matches!(
                intent.relation,
                RoomingRelation::Near
                    | RoomingRelation::ConnectedRooms
                    | RoomingRelation::AdjacentRooms
            ) {
                return Err(if intent.strength == ConstraintStrength::Must {
                    SolverError::UnsupportedHardConstraint(intent.relation)
                } else {
                    SolverError::UnsupportedPreference(intent.relation)
                });
            }
        }
    }
    Ok(())
}'''
if text.count(old_intents) != 1:
    raise SystemExit("rooming validation anchor changed")
text = text.replace(old_intents, new_intents, 1)

old_context = '''struct SearchContext<'a> {
    party: &'a BookingParty,
    check_in: CivilDate,
    offers: Vec<PreparedOffer<'a>>,
    config: SolverConfig,'''
new_context = '''struct SearchContext<'a> {
    party: &'a BookingParty,
    check_in: CivilDate,
    offers: Vec<PreparedOffer<'a>>,
    topology: Option<&'a RoomRelationIndex>,
    config: SolverConfig,'''
if text.count(old_context) != 1:
    raise SystemExit("search context anchor changed")
text = text.replace(old_context, new_context, 1)

old_hard_match = '''        let left_offer = &context.offers[left];
        let right_offer = &context.offers[right];
        let valid = match intent.relation {
            RoomingRelation::SameRoom => left == right,
            RoomingRelation::SeparateRoom => left != right,
            RoomingRelation::SameFloor => left_offer.floor == right_offer.floor,
            RoomingRelation::SameBuilding => left_offer.building == right_offer.building,
            relation => return Err(SolverError::UnsupportedHardConstraint(relation)),
        };'''
new_hard_match = "        let valid = relation_satisfied(context, left, right, intent.relation)?;"
if text.count(old_hard_match) != 2:
    raise SystemExit("hard relation match count changed")
text = text.replace(old_hard_match, new_hard_match)

old_soft_match = '''        let left_offer = &context.offers[left];
        let right_offer = &context.offers[right];
        let satisfied = match intent.relation {
            RoomingRelation::SameRoom => left == right,
            RoomingRelation::SeparateRoom => left != right,
            RoomingRelation::SameFloor => left_offer.floor == right_offer.floor,
            RoomingRelation::SameBuilding => left_offer.building == right_offer.building,
            relation => return Err(SolverError::UnsupportedPreference(relation)),
        };'''
new_soft_match = "        let satisfied = relation_satisfied(context, left, right, intent.relation)?;"
if text.count(old_soft_match) != 1:
    raise SystemExit("soft relation match changed")
text = text.replace(old_soft_match, new_soft_match, 1)

soft_marker = "fn layout_soft_penalty(context: &SearchContext<'_>) -> Result<u32, SolverError> {\n"
relation_helper = '''fn relation_satisfied(
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

'''
if text.count(soft_marker) != 1:
    raise SystemExit("soft penalty marker changed")
text = text.replace(soft_marker, relation_helper + soft_marker, 1)

error_anchor = "    DuplicateRoomId(u32),\n    StateBudgetExhausted,\n"
error_replacement = "    DuplicateRoomId(u32),\n    TopologyMissingRoom(u32),\n    StateBudgetExhausted,\n"
if text.count(error_anchor) != 1:
    raise SystemExit("solver error anchor changed")
text = text.replace(error_anchor, error_replacement, 1)

path.write_text(text)
